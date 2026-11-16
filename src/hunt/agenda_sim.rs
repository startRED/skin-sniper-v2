//! Simulação offline da agenda sobre uma cópia do banco: quantos anúncios novos de seed pagante (≤ saldo) cada
//! leitura pega. Só roda à mão (`SNIPER_DB=<cópia>/sniper.db cargo test --release agenda_on_a_real_db --
//! --ignored --nocapture`); abre o banco para escrita (migração, `refit_log`), então use sempre uma cópia.
//!
//! Premissas (não medidas, só o que a conta assume):
//! - cada faixa recebe anúncio novo ≤ saldo como Poisson, em dois mercados: **observado** (anúncios da faixa em
//!   `observations` que apareceram depois da primeira vez que a faixa foi vista, ÷ o tempo desde então; faixa
//!   nunca vista = 0, então favorece as faixas que a agenda já lia) e **giro** (vendas/dia da skin × seeds da
//!   faixa ÷ 1 001 × fração ≤ saldo dos anúncios de seed pagante da skin);
//! - cada anúncio vive um tempo exponencial de média `L` (três cenários, a vida real do raro barato não foi
//!   medida: 9 sumiços em 215 vistos);
//! - uma leitura a cada `LISTINGS_REST_SECS` (o ritmo de hoje), sem falha, todas em alvo raro; a agenda começa
//!   sem nada aprendido e aprende com o que pega, com os parâmetros do banco ou com a janela que o ajuste adota
//!   olhando só as releituras de faixa.

use crate::db::Db;
use crate::hunt::catalog::{self, Ctx, select_ranges};
use crate::hunt::scheduler::{Found, LISTINGS_REST_SECS, Queue, Range, Task};
use crate::hunt::value::Params;
use crate::hunt::{Hunt, HuntDeps};
use std::collections::HashMap;
use std::sync::Arc;

const DAY: i64 = 86_400;
/// Repetições da semana simulada (sementes do sorteio) somadas em cada linha.
const SEEDS: u64 = 10;

/// Gerador determinístico (xorshift): mesma sequência para a agenda antiga e a nova.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.unit()).ln()
    }
}

/// Um anúncio simulado: chega, vive até `gone` e conta uma vez só.
struct Ad {
    at: f64,
    gone: f64,
    seen: bool,
}

pub struct Outcome {
    pub reads: u64,
    pub caught: u64,
    /// Anúncios pegos × lucro por anúncio da faixa (centavos).
    pub worth: f64,
    pub arrived: u64,
    pub ranges_read: usize,
}

/// Roda a agenda `days` dias sobre o mercado `rates` (anúncios/s por faixa, na ordem de `Task::seeds`).
pub fn simulate(
    tasks: &[Task],
    ranges: &HashMap<String, Vec<Range>>,
    rates: &HashMap<String, Vec<f64>>,
    params: Params,
    life: f64,
    days: i64,
    seed: u64,
) -> Outcome {
    let start = 1_800_000_000;
    let end = start + days * DAY;
    let mut rng = Rng(seed);
    let mut ads: HashMap<(String, usize), Vec<Ad>> = HashMap::new();
    let mut arrived = 0;
    for t in tasks {
        for (i, rate) in rates[&t.id].iter().enumerate() {
            if *rate <= 0.0 {
                continue;
            }
            let mut at = start as f64 + rng.exp(1.0 / rate);
            let list = ads.entry((t.id.clone(), i)).or_default();
            while at < end as f64 {
                list.push(Ad {
                    at,
                    gone: at + rng.exp(life),
                    seen: false,
                });
                arrived += 1;
                at += rng.exp(1.0 / rate);
            }
        }
    }
    let mut q = Queue {
        params,
        ..Queue::default()
    };
    q.set_tasks(tasks.to_vec());
    q.set_ranges(ranges.clone());
    let mut out = Outcome {
        reads: 0,
        caught: 0,
        worth: 0.0,
        arrived,
        ranges_read: 0,
    };
    let mut read_ranges = std::collections::HashSet::new();
    let mut now = start;
    while now < end {
        let Some(task) = q.next_matching(now, Task::rare) else {
            break;
        };
        let full = tasks.iter().find(|t| t.id == task.id).unwrap();
        let narrowed = q.focus(task, now);
        let i = full
            .seeds
            .iter()
            .position(|r| *r == narrowed.seeds[0])
            .unwrap();
        read_ranges.insert((full.id.clone(), i));
        let mut fresh = 0;
        for ad in ads.get_mut(&(full.id.clone(), i)).into_iter().flatten() {
            if !ad.seen && ad.at <= now as f64 && (now as f64) < ad.gone {
                ad.seen = true;
                fresh += 1;
            }
        }
        q.reward(
            &full.id,
            now,
            Found {
                fresh,
                ..Found::default()
            },
        );
        q.take_reads();
        out.reads += 1;
        out.caught += u64::from(fresh);
        out.worth += f64::from(fresh) * ranges.get(&full.id).map_or(1.0, |r| r[i].gain);
        now += LISTINGS_REST_SECS;
    }
    out.ranges_read = read_ranges.len();
    out
}

/// Taxa observada por faixa: anúncios ≤ saldo que apareceram depois da primeira vez que a faixa foi vista, ÷ o
/// tempo desde então (`since` limita a janela).
fn observed_rates(
    c: &rusqlite::Connection,
    tasks: &[Task],
    saldo: i64,
    since: i64,
    end: i64,
) -> HashMap<String, Vec<f64>> {
    let mut q = c
        .prepare(
            "SELECT price_cents, first_seen FROM observations
             WHERE query LIKE ?1 || '|%' AND seed BETWEEN ?2 AND ?3 AND first_seen >= ?4",
        )
        .unwrap();
    tasks
        .iter()
        .map(|t| {
            let rates = t
                .seeds
                .iter()
                .map(|&(lo, hi)| {
                    let rows: Vec<(i64, i64)> = q
                        .query_map(rusqlite::params![t.code, lo, hi, since], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })
                        .unwrap()
                        .flatten()
                        .collect();
                    let Some(first) = rows.iter().map(|r| r.1).min() else {
                        return 0.0;
                    };
                    let new = rows
                        .iter()
                        .filter(|(price, at)| *price <= saldo && *at > first + 60)
                        .count();
                    new as f64 / (end - first).max(DAY) as f64
                })
                .collect();
            (t.id.clone(), rates)
        })
        .collect()
}

/// Taxa pelo giro: vendas/dia da skin (todos os desgastes) × seeds da faixa ÷ 1 001 × fração dos anúncios de seed
/// pagante da skin que cabem no saldo (`observations`; sem nenhum, a fração de todas as skins).
fn turnover_rates(
    c: &rusqlite::Connection,
    items: &[catalog::Item],
    tasks: &[Task],
    saldo: i64,
) -> HashMap<String, Vec<f64>> {
    let cheap = |code: &str| -> Option<f64> {
        c.query_row(
            "SELECT AVG(price_cents <= ?2) FROM observations WHERE query LIKE ?1 || '|%property_id%'",
            rusqlite::params![code, saldo],
            |r| r.get::<_, Option<f64>>(0),
        )
        .unwrap()
    };
    let all = cheap("").unwrap_or(0.0);
    tasks
        .iter()
        .map(|t| {
            let base = crate::pricing::groups::skin_base(&t.name);
            let spd: f64 = items
                .iter()
                .filter(|i| {
                    crate::pricing::groups::skin_base(&i.name) == base
                        && catalog::wear_of(&i.name).is_some()
                        && !i.name.contains("StatTrak™")
                        && !i.name.starts_with("Souvenir")
                })
                .filter_map(|i| i.sales_per_day)
                .sum();
            let f = cheap(&t.code).unwrap_or(all);
            let rates = t
                .seeds
                .iter()
                .map(|&(lo, hi)| spd * f64::from(hi - lo + 1) / 1001.0 * f / DAY as f64)
                .collect();
            (t.id.clone(), rates)
        })
        .collect()
}

#[tokio::test]
#[ignore = "medida: SNIPER_DB aponta para uma CÓPIA do banco (abre para escrita); imprime a simulação da agenda"]
async fn agenda_on_a_real_db() {
    let path = std::path::PathBuf::from(std::env::var("SNIPER_DB").expect("SNIPER_DB"));
    let saldo: i64 = std::env::var("SALDO")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3608);
    let db = Db::open(&path).unwrap();
    let raw =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let end: i64 = raw
        .query_row("SELECT MAX(at) FROM reads", [], |r| r.get(0))
        .unwrap();
    let params: Params = raw
        .query_row(
            "SELECT value FROM settings WHERE key = 'value_params'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    let h: Arc<Hunt> = Hunt::with_clock(
        HuntDeps {
            settings: crate::settings::SettingsStore::new(db.clone()),
            db: db.clone(),
            steam: Arc::new(crate::steam::client::SteamClient::new().unwrap()),
            csfloat: Arc::new(crate::csfloat::CsfloatClient::new().unwrap()),
            events: tokio::sync::broadcast::channel(16).0,
            telegram: None,
        },
        crate::db::now_unix,
    );
    crate::hunt::patterns::load(&h).await.unwrap();
    let learned = h.lock().learned.clone();
    let items = catalog::load_items(&db).await.unwrap();
    let deals = catalog::load_deals(&db, &learned, end).await.unwrap();
    let (ratio, _) = catalog::load_ratio(&db)
        .await
        .unwrap()
        .expect("razão BRL/USD");
    let (boost, floors) = (HashMap::new(), HashMap::new());
    let ctx = Ctx {
        ratio,
        saldo,
        now: end,
        boost: &boost,
        floors: &floors,
        learned: &learned,
        deals: &deals,
    };
    let (mut tasks, ranges) = select_ranges(&items, &ctx);
    tasks.retain(Task::rare);
    let observed = observed_rates(&raw, &tasks, saldo, end - 7 * DAY, end);
    let turnover = turnover_rates(&raw, &items, &tasks, saldo);
    let n_ranges: usize = tasks.iter().map(|t| t.seeds.len()).sum();
    // a janela que melhor prevê as releituras de faixa (só as do raro), na mesma grade do ajuste de hora em hora
    let rows: Vec<crate::hunt::value::ReadRow> = raw
        .prepare("SELECT at, arm, fresh, weight, gap FROM reads WHERE weight IS NOT NULL ORDER BY at, rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .flatten()
        .collect();
    let (ids, reads) = crate::hunt::value::merge(rows);
    let classes: Vec<usize> = ids.iter().map(|id| crate::hunt::value::class(id)).collect();
    let (fit, _, adopted) = crate::hunt::value::tune(&reads, &classes, params);
    println!(
        "{} alvos, {n_ranges} faixas; parâmetros do banco {params:?}; janela que melhor prevê só as releituras de \
         faixa: {} s (adotada pelo ajuste: {adopted})",
        tasks.len(),
        fit.window
    );
    // o banco como está e a janela que o ajuste adota se olhar só as releituras de faixa
    let fitted = Params {
        window: fit.window,
        ..params
    };
    for ((market, rates), (label, params)) in [("observado", &observed), ("giro", &turnover)]
        .into_iter()
        .flat_map(|m| [(m, ("banco", params)), (m, ("janela das faixas", fitted))])
    {
        let live = rates.values().flatten().filter(|r| **r > 0.0).count();
        let per_day: f64 = rates.values().flatten().sum::<f64>() * DAY as f64;
        println!(
            "mercado {market}, parâmetros {label}: {live} faixas com chegada, {per_day:.1} anúncios novos ≤ saldo/dia"
        );
        for life in [900.0, 7200.0, 43_200.0] {
            let runs: Vec<Outcome> = (1..=SEEDS)
                .map(|s| simulate(&tasks, &ranges, rates, params, life, 7, 0x5eed * s))
                .collect();
            let reads: u64 = runs.iter().map(|o| o.reads).sum();
            let caught: u64 = runs.iter().map(|o| o.caught).sum();
            let arrived: u64 = runs.iter().map(|o| o.arrived).sum();
            let worth: f64 = runs.iter().map(|o| o.worth).sum();
            let ranges_read = runs.iter().map(|o| o.ranges_read).sum::<usize>() / runs.len();
            println!(
                "  vida {:>4.0} min: {caught} pegos de {arrived} ({:.1} %), {:.4} anúncio novo/leitura, \
                 {:.2} centavos/leitura, {ranges_read} faixas lidas",
                life / 60.0,
                100.0 * caught as f64 / arrived.max(1) as f64,
                caught as f64 / reads.max(1) as f64,
                worth / reads.max(1) as f64,
            );
        }
    }
}

#[test]
fn comparison_requires_thirty_in_each_arm_and_exact_bands() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE reads(choice_range TEXT, choice_gap INTEGER, choice_control INTEGER, fresh INTEGER, new_listings INTEGER, at INTEGER DEFAULT 10, outcome TEXT DEFAULT 'ok');").unwrap();
    for gap in [599, 600, 3599, 3600, 21599, 21600] {
        for arm in [0, 1] {
            for _ in 0..30 {
                c.execute(
                    "INSERT INTO reads(choice_range,choice_gap,choice_control,fresh,new_listings) VALUES('r:a#1-2',?1,?2,1,4)",
                    rusqlite::params![gap, arm],
                )
                .unwrap();
            }
        }
    }
    c.execute(
        "DELETE FROM reads WHERE rowid=(SELECT min(rowid) FROM reads)",
        [],
    )
    .unwrap();
    let report = comparison_report(&c, 0, 100).unwrap();
    assert!(report.contains("dados insuficientes"));
    assert!(report.contains("600-3600"));
    assert!(report.contains("3600-21600"));
    assert!(report.contains(">=21600"));
    assert!(report.contains("<600 s | agenda=29 controle=30 | anúncios raros novos dentro do saldo por leitura: dados insuficientes"));
    assert!(report.contains("600-3600 s | agenda=60 controle=60 | anúncios raros"));
    assert!(report.contains("3600-21600 s | agenda=60 controle=60 | anúncios raros"));
    assert!(report.contains(">=21600 s | agenda=30 controle=30 | anúncios raros"));
    assert!(report.contains("agenda=1.000000 controle=1.000000"));
    c.execute("INSERT INTO reads(choice_range,choice_gap,choice_control,fresh,new_listings) VALUES('r:b#1-2',21600,0,7,NULL)", [])
        .unwrap();
    c.execute("INSERT INTO reads(choice_range,choice_gap,choice_control,fresh,new_listings) VALUES('r:a#1-3',21600,1,7,NULL)", [])
        .unwrap();
    let report = comparison_report(&c, 0, 100).unwrap();
    assert_eq!(report.matches("dados insuficientes").count(), 6);
}

/// Associação observacional por faixa e banda no período `since..=until` (unix, inclusivo), igual para os
/// dois braços; consultas que falharam ficam fora. Nunca inicia a caça nem lê configurações.
fn comparison_report(c: &rusqlite::Connection, since: i64, until: i64) -> rusqlite::Result<String> {
    let mut out = format!(
        "Período comum: {since}..={until} (unix, inclusivo).\n\
         Associação com a agenda, sem causalidade. Bandas não igualam esperas, saldo ou mercado.\n"
    );
    let rows = crate::hunt::comparison::compare(c, since, until)?;
    let mut count = 0;
    for row in rows {
        let crate::hunt::comparison::Comparison {
            range,
            band,
            policy,
            control,
            policy_fresh: pf,
            control_fresh: cf,
            policy_new_count: pn,
            control_new_count: cn,
            policy_new: pt,
            control_new: ct,
            ..
        } = row;
        count += 1;
        let label = ["<600", "600-3600", "3600-21600", ">=21600"][band as usize];
        for (metric, policy, control, pf, cf) in [
            (
                "anúncios raros novos dentro do saldo por leitura",
                policy,
                control,
                pf,
                cf,
            ),
            ("anúncios novos por leitura", pn, cn, pt, ct),
        ] {
            out.push_str(&format!(
                "{range} | {label} s | agenda={policy} controle={control} | {metric}: "
            ));
            if policy < 30 || control < 30 {
                out.push_str("dados insuficientes (mínimo 30 CADA braço)\n");
            } else {
                out.push_str(&format!(
                    "agenda={:.6} controle={:.6}\n",
                    pf as f64 / policy as f64,
                    cf as f64 / control as f64
                ));
            }
        }
    }
    if count == 0 {
        out.push_str("dados insuficientes: nenhuma faixa/banda comparável registrada\n");
    }
    Ok(out)
}

#[test]
#[ignore = "exige caminho explícito de cópia offline; somente leitura"]
fn compare_recorded_reads() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("informe {k}"));
    let at = |k: &str| var(k).parse::<i64>().expect("timestamp unix em segundos");
    let (since, until) = (at("SNIPER_COMPARISON_SINCE"), at("SNIPER_COMPARISON_UNTIL"));
    assert!(since <= until, "período vazio");
    let c = rusqlite::Connection::open_with_flags(
        var("SNIPER_COMPARISON_DB"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    println!("{}", comparison_report(&c, since, until).unwrap());
}

#[test]
fn comparison_distinguishes_total_new_from_affordable_and_missing() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE reads(choice_range TEXT, choice_gap INTEGER, choice_control INTEGER, fresh INTEGER, new_listings INTEGER, at INTEGER DEFAULT 10, outcome TEXT DEFAULT 'ok');").unwrap();
    for arm in [0, 1] {
        for _ in 0..30 {
            c.execute("INSERT INTO reads(choice_range,choice_gap,choice_control,fresh,new_listings) VALUES('r:a#1-2',600,?1,1,4)", [arm])
                .unwrap();
        }
    }
    let report = comparison_report(&c, 0, 100).unwrap();
    assert!(report.contains("anúncios novos por leitura: agenda=4.000000 controle=4.000000"));
    assert!(report.contains(
        "anúncios raros novos dentro do saldo por leitura: agenda=1.000000 controle=1.000000"
    ));
    c.execute("UPDATE reads SET new_listings=NULL WHERE rowid=1", [])
        .unwrap();
    let report = comparison_report(&c, 0, 100).unwrap();
    assert!(
        report.contains("agenda=29 controle=30 | anúncios novos por leitura: dados insuficientes")
    );
    assert!(report.contains(
        "anúncios raros novos dentro do saldo por leitura: agenda=1.000000 controle=1.000000"
    ));
}

#[test]
fn comparison_uses_only_the_common_period_and_skips_failures() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE reads(choice_range TEXT, choice_gap INTEGER, choice_control INTEGER, fresh INTEGER, new_listings INTEGER, at INTEGER, outcome TEXT);").unwrap();
    let add = |at: i64, arm: i64, fresh: i64, new: Option<i64>, outcome: &str| {
        c.execute(
            "INSERT INTO reads VALUES('r:a#1-2',600,?1,?2,?3,?4,?5)",
            rusqlite::params![arm, fresh, new, at, outcome],
        )
        .unwrap();
    };
    for arm in [0, 1] {
        // vazios bem-sucedidos contam como zero; o achado fica na borda inicial do período
        for _ in 0..29 {
            add(150, arm, 0, Some(0), "ok");
        }
        add(100, arm, 3, Some(3), "ok");
        add(200, arm, 0, Some(0), "ok");
        for _ in 0..40 {
            // fora do período: não entram nem completam o mínimo
            add(99, arm, 9, Some(9), "ok");
            add(201, arm, 9, Some(9), "ok");
            // falha enviada dentro do período: nem zero nem leitura
            add(150, arm, 0, None, "failed");
        }
    }
    let report = comparison_report(&c, 100, 200).unwrap();
    assert!(report.starts_with("Período comum: 100..=200"));
    assert!(report.contains("agenda=31 controle=31 | anúncios raros novos dentro do saldo por leitura: agenda=0.096774 controle=0.096774"));
    assert!(report.contains("anúncios novos por leitura: agenda=0.096774 controle=0.096774"));
    // período que corta as bordas: 29 por braço, abaixo do mínimo, sem conclusão
    let report = comparison_report(&c, 101, 199).unwrap();
    assert!(report.contains("agenda=29 controle=29"));
    assert_eq!(report.matches("dados insuficientes").count(), 2);
    assert!(!report.contains("agenda=0."));
}
