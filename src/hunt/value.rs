//! Valor de uma leitura: o único número que a agenda compara para decidir quem consultar agora.
//!
//! `V = taxa × L·(1 − e^(−Δ/L)) × retorno` (centavos esperados):
//! - **taxa**: anúncios novos dentro do saldo por segundo de exposição do alvo, encolhida para a taxa da classe
//!   (`d`/`r`/`f`) enquanto ele tem pouca medida, com otimismo de contagem (`Z·√a`) e teto por seca (`capped`);
//! - **estoque**: o que se acumulou desde a última leitura (`Δ`), limitado pela vida `L` de um anúncio barato;
//! - **retorno**: centavos de lucro esperado por anúncio novo, do alvo encolhido para a classe e para o total.
//!
//! O sinal é o anúncio novo (milhares por semana), não o achado (3 na vida do app): todo achado é um anúncio novo.
//! De hora em hora o app repete as leituras dos últimos 7 dias, mede o erro de previsão de cada combinação de
//! parâmetros e troca quando outra prevê ≥ 1 % melhor (`refit_log`, chaves `fluxo:*`).
//! `N0`, `Z`, o p25 da vida, a margem de 1 % e o teto por seca são hipóteses de calibração, não medidas.
//! Docs/projeto.md, "Aprendizado".

use super::Hunt;
use super::catalog::{get_setting, set_setting};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DAY: i64 = 86_400;
/// Resumo para o placar e parâmetros em uso (JSON em `settings`).
pub const INFO_KEY: &str = "value_info";
pub const PARAMS_KEY: &str = "value_params";
const GAP: i64 = 3600;
/// Sumiços medidos para trocar a vida padrão pelo percentil 25.
const MIN_LIVES: usize = 20;
/// Leituras pontuadas para o ajuste poder trocar parâmetro.
const MIN_SCORED: u64 = 1000;
/// Só troca se a previsão melhorar pelo menos isto.
const ADOPT_MARGIN: f64 = 0.01;
/// Páginas do mesmo alvo em sequência contam como uma leitura na repetição.
const MERGE_SECS: i64 = 90;
/// Peso, em anúncios novos, da classe no retorno de um alvo (e do total no retorno da classe).
const N0: f64 = 500.0;
/// Otimismo da taxa na escolha, em desvios da contagem.
pub const Z: f64 = 1.0;
const MU_FLOOR: f64 = 1e-6;
/// Contagem de Poisson que deu zero: com 95 % de confiança a média não passa de 3 (regra de três).
const DRY_CAP: f64 = 3.0;
const CLASSES: [&str; 3] = ["d", "r", "f"];
const WINDOWS: [f64; 5] = [1800.0, 3600.0, 7200.0, 14_400.0, 28_800.0];
const PRIORS: [f64; 4] = [0.5, 1.0, 2.0, 4.0];
const HALF_LIVES: [f64; 3] = [24.0 * 3600.0, 72.0 * 3600.0, 168.0 * 3600.0];

/// Padrões medidos em 10/11 no registro da produção (7 100 leituras): docs/superpowers/specs/2026-11-10-*.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Janela (s) em que anúncio novo se acumula entre duas leituras do mesmo alvo.
    pub window: f64,
    /// Peso da classe na taxa de um alvo, em janelas de exposição.
    pub prior: f64,
    /// Contagens e exposição pesam metade a cada tanto (s).
    pub half_life: f64,
    /// Vida (s) de um anúncio barato: até onde o estoque de achado cresce sem leitura.
    pub life: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            window: 7200.0,
            prior: 1.0,
            half_life: 72.0 * 3600.0,
            life: 900.0,
        }
    }
}

/// Classe do alvo pelo prefixo do id: 0 = desconto, 1 = raro por seed, 2 = faixa de float.
pub fn class(id: &str) -> usize {
    match id.as_bytes().first() {
        Some(b'r') => 1,
        Some(b'f') => 2,
        _ => 0,
    }
}

/// Exposição de uma leitura feita `gap` segundos depois da anterior do mesmo alvo (`None` = primeira).
pub fn exposure(gap: Option<i64>, window: f64) -> f64 {
    match gap {
        Some(d) => window * (1.0 - (-(d.max(0) as f64) / window).exp()),
        None => window,
    }
}

/// Zero anúncio novo em `dry` segundos de exposição seguidos: a taxa não passa de 3 ÷ `dry`. É o que faz o alvo que
/// secou perder espaço em horas e não em dias; o primeiro anúncio novo zera `dry` e a taxa medida volta inteira.
/// No histórico da produção (10/11) o teto não piora a previsão (0,3551 × 0,3563 sem ele).
pub fn capped(rate: f64, dry: f64) -> f64 {
    if dry > 0.0 {
        rate.min(DRY_CAP / dry)
    } else {
        rate
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Sum {
    r: f64,
    exposure: f64,
    gain: f64,
    fresh: f64,
}

/// Somas por classe e do total: o ponto de partida de quem ainda tem pouca medida.
#[derive(Debug, Clone, Default)]
pub struct Pool {
    class: [Sum; 3],
    all: Sum,
}

impl Pool {
    pub fn add(&mut self, id: &str, r: f64, exposure: f64, gain: f64, fresh: f64) {
        for s in [&mut self.class[class(id)], &mut self.all] {
            s.r += r;
            s.exposure += exposure;
            s.gain += gain;
            s.fresh += fresh;
        }
    }

    /// Anúncios novos por segundo de exposição na classe; sem medida na classe, no total; sem nenhuma, 0.
    fn lambda(&self, c: usize) -> f64 {
        [self.class[c], self.all]
            .iter()
            .find(|s| s.exposure > 0.0)
            .map_or(0.0, |s| s.r / s.exposure)
    }

    /// Taxa do alvo (anúncios novos por segundo); `z` = 0 dá a média, `Z` a versão otimista da escolha.
    pub fn rate(&self, p: &Params, id: &str, r: f64, exposure: f64, z: f64) -> f64 {
        let prior = p.prior * p.window;
        let a = r + prior * self.lambda(class(id));
        (a + z * a.sqrt()) / (exposure + prior)
    }

    /// Centavos de lucro esperado por anúncio novo do alvo. Sem nenhum ganho medido vale 1 para todos.
    pub fn payoff(&self, id: &str, gain: f64, fresh: f64) -> f64 {
        if self.all.gain <= 0.0 || self.all.fresh <= 0.0 {
            return 1.0;
        }
        let c = self.class[class(id)];
        let of_class = (N0 * self.all.gain / self.all.fresh + c.gain) / (N0 + c.fresh);
        (N0 * of_class + gain) / (N0 + fresh)
    }
}

/// Estado de um alvo no fim da repetição: contagens já decaídas até `at` (a última leitura; 0 = nenhuma).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ArmState {
    pub r: f64,
    pub exposure: f64,
    pub at: i64,
    /// Exposição somada desde o último anúncio novo (sem decaimento).
    pub dry: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Replay {
    /// Perda de Poisson somada do modelo e da taxa fixa por classe, nas mesmas `scored` leituras.
    pub loss: f64,
    pub base_loss: f64,
    pub scored: u64,
    pub predicted: f64,
    pub actual: f64,
    pub arms: Vec<ArmState>,
}

fn poisson_loss(mu: f64, f: f64) -> f64 {
    let mu = mu.max(MU_FLOOR);
    mu - f * mu.ln()
}

/// Repete as leituras em ordem e prevê cada uma só com o que veio antes. `reads` = (hora, índice do alvo, anúncios
/// novos), em ordem de hora; `class_of[alvo]` = classe. A primeira leitura de cada alvo ensina mas não pontua.
pub fn replay(reads: &[(i64, usize, f64)], class_of: &[usize], p: &Params) -> Replay {
    let mut out = Replay {
        arms: vec![ArmState::default(); class_of.len()],
        ..Replay::default()
    };
    // (r, exposição, hora) por classe: todo mundo decai na mesma taxa, então a soma decai junto
    let mut pool = [(0.0_f64, 0.0_f64, 0_i64); 3];
    let mut base = [(0.0_f64, 0.0_f64); 3];
    let decay = |dt: i64| 0.5_f64.powf(dt.max(0) as f64 / p.half_life);
    for &(at, arm, f) in reads {
        let c = class_of[arm];
        let a = &mut out.arms[arm];
        let (cr, ce, cat) = &mut pool[c];
        let (fa, fc) = (decay(at - a.at), decay(at - *cat));
        a.r *= fa;
        a.exposure *= fa;
        *cr *= fc;
        *ce *= fc;
        *cat = at;
        let e = exposure((a.at > 0).then(|| at - a.at), p.window);
        if a.at > 0 && *ce > 0.0 {
            let prior = p.prior * p.window;
            let mu = capped((a.r + prior * *cr / *ce) / (a.exposure + prior), a.dry) * e;
            out.loss += poisson_loss(mu, f);
            // shortcut: a referência parte de 0,5 por leitura; some depois de poucas leituras da classe
            out.base_loss += poisson_loss((base[c].0 + 1.0) / (base[c].1 + 2.0), f);
            out.scored += 1;
            out.predicted += mu;
            out.actual += f;
        }
        a.r += f;
        a.exposure += e;
        a.at = at;
        a.dry = if f > 0.0 { 0.0 } else { a.dry + e };
        *cr += f;
        *ce += e;
        base[c].0 += f;
        base[c].1 += 1.0;
    }
    out
}

/// A combinação que melhor prevê e se ela ganha da atual pela margem (com leituras suficientes).
pub fn tune(
    reads: &[(i64, usize, f64)],
    class_of: &[usize],
    cur: Params,
) -> (Params, Replay, bool) {
    let current = replay(reads, class_of, &cur);
    let mut best = (cur, current.clone());
    for window in WINDOWS {
        for prior in PRIORS {
            for half_life in HALF_LIVES {
                let p = Params {
                    window,
                    prior,
                    half_life,
                    life: cur.life,
                };
                let r = replay(reads, class_of, &p);
                if r.loss < best.1.loss {
                    best = (p, r);
                }
            }
        }
    }
    let better = current.scored >= MIN_SCORED
        && current.loss - best.1.loss > ADOPT_MARGIN * current.loss.abs();
    if better {
        (best.0, best.1, true)
    } else {
        (cur, current, false)
    }
}

/// Junta as páginas seguidas do mesmo alvo e troca o id por índice. Devolve (ids, leituras).
pub fn merge(rows: Vec<(i64, String, i64)>) -> (Vec<String>, Vec<(i64, usize, f64)>) {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut ids = vec![];
    let mut last: Vec<Option<usize>> = vec![];
    let mut reads: Vec<(i64, usize, f64)> = vec![];
    for (at, id, fresh) in rows {
        let arm = *index.entry(id.clone()).or_insert_with(|| {
            ids.push(id);
            last.push(None);
            ids.len() - 1
        });
        match last[arm] {
            Some(i) if at - reads[i].0 <= MERGE_SECS => reads[i].2 += fresh as f64,
            _ => {
                last[arm] = Some(reads.len());
                reads.push((at, arm, fresh as f64));
            }
        }
    }
    (ids, reads)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassInfo {
    pub class: String,
    pub reads: u64,
    pub fresh: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub window_s: f64,
    pub prior: f64,
    pub half_life_h: f64,
    /// Vida do anúncio barato em uso e quantos sumiços a mediram (0 = padrão).
    pub life_s: f64,
    pub lives: u32,
    /// Leituras de alvo e anúncios novos nos últimos 7 dias, no total e por classe.
    pub reads: u64,
    pub fresh: u64,
    pub classes: Vec<ClassInfo>,
    /// Erro de previsão por leitura pontuada: o modelo e a taxa fixa por classe (menor é melhor).
    pub scored: u64,
    pub loss: Option<f64>,
    pub base_loss: Option<f64>,
    pub predicted: f64,
    pub actual: f64,
    pub tuned_at: i64,
}

fn percentile25(v: &mut [i64]) -> Option<f64> {
    v.sort_unstable();
    v.get(v.len() / 4).map(|&x| x as f64)
}

/// De hora em hora: mede `L`, repete as leituras, troca os parâmetros se outro prevê melhor e grava o resumo.
/// Só o banco, nunca a Steam.
pub async fn maybe_step(h: &Hunt) {
    let now = h.now();
    {
        let mut s = h.lock();
        if s.value_at != 0 && now - s.value_at < GAP {
            return;
        }
        s.value_at = now;
    }
    if let Err(e) = step(h, now).await {
        tracing::warn!("agenda: ajuste falhou: {e:#}");
    }
}

async fn step(h: &Hunt, now: i64) -> anyhow::Result<()> {
    // as leituras ainda em memória entram na repetição
    super::scheduler::save_stats(h).await?;
    let since = now - 7 * DAY;
    let (mut lives, rows) = h
        .db
        .call(move |c| {
            let lives: Vec<i64> = c
                .prepare(
                    "SELECT gone_at - first_seen FROM observations WHERE cheapest = 1 AND relisted = 0
                     AND gone_at >= ?1",
                )?
                .query_map([since], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let rows: Vec<(i64, String, i64)> = c
                .prepare("SELECT at, arm, fresh FROM reads WHERE at >= ?1 ORDER BY at, rowid")?
                .query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok((lives, rows))
        })
        .await?;
    let cur = h.lock().queue.params;
    let measured = lives.len() >= MIN_LIVES;
    let life = percentile25(&mut lives)
        .filter(|_| measured)
        .map_or(Params::default().life, |l| l.max(60.0));
    let (ids, reads) = merge(rows);
    let class_of: Vec<usize> = ids.iter().map(|id| class(id)).collect();
    let mut classes: Vec<ClassInfo> = CLASSES
        .iter()
        .map(|c| ClassInfo {
            class: (*c).into(),
            ..ClassInfo::default()
        })
        .collect();
    for &(_, arm, f) in &reads {
        classes[class_of[arm]].reads += 1;
        classes[class_of[arm]].fresh += f as u64;
    }
    let (p, r, changed) = {
        let (reads, class_of) = (reads.clone(), class_of.clone());
        tokio::task::spawn_blocking(move || tune(&reads, &class_of, cur)).await?
    };
    let p = Params { life, ..p };
    {
        let mut s = h.lock();
        s.queue.params = p;
        if changed {
            s.queue.rebuild(&ids, &r.arms);
        }
    }
    if changed {
        tracing::info!(
            janela_s = p.window,
            prior = p.prior,
            meia_vida_h = p.half_life / 3600.0,
            "agenda: parâmetros trocados pelo ajuste"
        );
        let why = format!(
            "erro de previsão {:.4} -> {:.4} por leitura em {} leituras",
            replay(&reads, &class_of, &cur).loss / r.scored as f64,
            r.loss / r.scored as f64,
            r.scored
        );
        let log = [
            ("fluxo:janela", cur.window, p.window),
            ("fluxo:prior", cur.prior, p.prior),
            ("fluxo:meia_vida", cur.half_life, p.half_life),
        ];
        h.db.call(move |c| {
            for (key, old, new) in log.into_iter().filter(|(_, old, new)| old != new) {
                c.execute(
                    "INSERT INTO refit_log(at, key, old, new, why) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![now, key, old, new, why],
                )?;
            }
            Ok(())
        })
        .await?;
    }
    let per = |x: f64| (r.scored > 0).then(|| x / r.scored as f64);
    let info = Info {
        window_s: p.window,
        prior: p.prior,
        half_life_h: p.half_life / 3600.0,
        life_s: life,
        lives: if measured { lives.len() as u32 } else { 0 },
        reads: reads.len() as u64,
        fresh: reads.iter().map(|r| r.2 as u64).sum(),
        classes,
        scored: r.scored,
        loss: per(r.loss),
        base_loss: per(r.base_loss),
        predicted: r.predicted,
        actual: r.actual,
        tuned_at: now,
    };
    set_setting(&h.db, PARAMS_KEY, serde_json::to_string(&p)?).await?;
    set_setting(&h.db, INFO_KEY, serde_json::to_string(&info)?).await?;
    Ok(())
}

/// Parâmetros gravados pelo último ajuste (arranque).
pub async fn load(h: &Hunt) -> anyhow::Result<()> {
    if let Some(p) = get_setting(&h.db, PARAMS_KEY)
        .await?
        .and_then(|v| serde_json::from_str::<Params>(&v).ok())
        .filter(|p| p.window > 0.0 && p.prior > 0.0 && p.half_life > 0.0 && p.life > 0.0)
    {
        h.lock().queue.params = p;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_grows_then_saturates_at_the_window() {
        let w = 7200.0;
        assert_eq!(exposure(None, w), w);
        assert_eq!(exposure(Some(0), w), 0.0);
        assert!((exposure(Some(7200), w) - w * (1.0 - (-1f64).exp())).abs() < 1e-9);
        assert!(exposure(Some(60), w) < exposure(Some(120), w));
        assert!(w - exposure(Some(100_000), w) < 0.01);
    }

    #[test]
    fn new_arm_starts_at_its_class_rate_and_evidence_moves_it() {
        let p = Params::default();
        let mut pool = Pool::default();
        // classe d: 10 anúncios novos em 20 000 s; classe r sem medida usa o total
        pool.add("d:a", 10.0, 20_000.0, 0.0, 10.0);
        let class_rate = 10.0 / 20_000.0;
        assert!((pool.rate(&p, "d:new", 0.0, 0.0, 0.0) - class_rate).abs() < 1e-12);
        assert!((pool.rate(&p, "r:new", 0.0, 0.0, 0.0) - class_rate).abs() < 1e-12);
        // medida própria puxa a taxa; o otimismo encolhe com a contagem
        let rich = pool.rate(&p, "d:rich", 50.0, 20_000.0, 0.0);
        let poor = pool.rate(&p, "d:poor", 0.0, 20_000.0, 0.0);
        assert!(rich > class_rate && poor < class_rate);
        let bonus =
            |r: f64| pool.rate(&p, "d:x", r, 20_000.0, Z) / pool.rate(&p, "d:x", r, 20_000.0, 0.0);
        assert!(bonus(1.0) > bonus(100.0) && bonus(100.0) > 1.0);
        // sem medida nenhuma a taxa é zero, não NaN
        assert_eq!(Pool::default().rate(&p, "d:a", 0.0, 0.0, Z), 0.0);
    }

    #[test]
    fn payoff_is_flat_without_gain_and_shrinks_to_class_and_total() {
        let mut pool = Pool::default();
        pool.add("d:a", 0.0, 0.0, 0.0, 1000.0);
        assert_eq!(pool.payoff("d:a", 0.0, 1000.0), 1.0);
        // um achado de R$ 24 no raro: a classe rara vale mais por anúncio novo que a comum
        pool.add("r:a", 0.0, 0.0, 2400.0, 30.0);
        let (d, r) = (pool.payoff("d:x", 0.0, 0.0), pool.payoff("r:x", 0.0, 0.0));
        assert!(r > 3.0 * d && d > 0.0, "{d} {r}");
        // o alvo que rendeu fica acima da classe; quem viu muito anúncio sem render fica abaixo
        assert!(pool.payoff("r:a", 2400.0, 30.0) > r);
        assert!(pool.payoff("d:a", 0.0, 1000.0) < d);
    }

    #[test]
    fn a_dry_spell_caps_the_rate_until_the_next_new_listing() {
        // 1 por minuto medido; 1 h de exposição sem nada: no máximo 3 por hora
        assert_eq!(capped(1.0 / 60.0, 3600.0), 3.0 / 3600.0);
        // seca curta ou taxa baixa: o teto não encosta
        assert_eq!(capped(1.0 / 60.0, 60.0), 1.0 / 60.0);
        assert_eq!(capped(1e-5, 3600.0), 1e-5);
        assert_eq!(capped(1.0 / 60.0, 0.0), 1.0 / 60.0);
    }

    /// Dois alvos da mesma classe: "rico" recebe 1 anúncio novo a cada 600 s, "seco" nenhum; leituras alternadas
    /// com intervalos variados.
    fn synthetic(n: usize) -> (Vec<(i64, usize, f64)>, Vec<usize>) {
        let (mut reads, mut last, mut t) = (vec![], [0_i64; 2], 1_000_000_i64);
        for i in 0..n {
            let arm = i % 2;
            t += [300, 900, 2700][i % 3];
            let fresh = if arm == 0 && last[0] > 0 {
                (t - last[0]) / 600
            } else {
                0
            };
            last[arm] = t;
            reads.push((t, arm, fresh as f64));
        }
        (reads, vec![0, 0])
    }

    #[test]
    fn replay_predicts_better_than_the_class_average() {
        let (reads, class_of) = synthetic(2000);
        let r = replay(&reads, &class_of, &Params::default());
        assert_eq!(r.scored, 1998); // a primeira de cada alvo não pontua
        assert!(r.loss < r.base_loss, "{} {}", r.loss, r.base_loss);
        // calibrado: o previsto fica perto do real
        assert!((r.predicted / r.actual - 1.0).abs() < 0.15, "{r:?}");
        assert!(r.arms[0].r > 10.0 && r.arms[1].r == 0.0);
        assert_eq!(r.arms[1].at, reads.last().unwrap().0);
    }

    #[test]
    fn tune_only_switches_with_enough_reads_and_a_real_gain() {
        let bad = Params {
            window: 1800.0,
            prior: 4.0,
            ..Params::default()
        };
        // poucas leituras: fica como está, mesmo havendo combinação melhor
        let (reads, class_of) = synthetic(400);
        let (p, _, changed) = tune(&reads, &class_of, bad);
        assert!(!changed && p == bad);
        let (reads, class_of) = synthetic(4000);
        let (p, r, changed) = tune(&reads, &class_of, bad);
        assert!(changed && p != bad, "{p:?}");
        assert!(r.loss < replay(&reads, &class_of, &bad).loss);
        assert_eq!(p.life, bad.life); // a vida vem do registro, não da grade
        // já no melhor ponto: não troca de novo
        assert!(!tune(&reads, &class_of, p).2);
    }

    #[test]
    fn merge_joins_pages_of_one_read_and_indexes_arms() {
        let (ids, reads) = merge(vec![
            (100, "d:a".into(), 2),
            (112, "d:a".into(), 3), // segunda página da mesma leitura
            (130, "r:b".into(), 0),
            (400, "d:a".into(), 1),
        ]);
        assert_eq!(ids, vec!["d:a".to_string(), "r:b".to_string()]);
        assert_eq!(reads, vec![(100, 0, 5.0), (130, 1, 0.0), (400, 0, 1.0)]);
        assert_eq!((class("d:a"), class("r:b"), class("f:c")), (0, 1, 2));
    }

    #[test]
    fn life_is_the_lower_quartile() {
        assert_eq!(percentile25(&mut [900, 100, 300, 500]), Some(300.0));
        assert_eq!(percentile25(&mut []), None);
    }
}
