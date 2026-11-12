//! Quais seeds pagam prêmio, medido nas vendas do CSFloat (`csfloat_sales`, sem adesivo). É a única porta de
//! entrada de um pattern raro: tabela, guia ou lista da comunidade só indicam candidatos (`pricing::groups`).
//!
//! Cada venda vira uma razão sobre o preço comum do nome (a mediana das vendas dele). Um seed paga quando:
//! 1. a skin tem nota por seed (% de azul, % do fade): as `NEAR` vendas de nota igual ou pior mais próximas da
//!    dele pagam (mediana ≥ `PAY_MEDIAN`, quartil de baixo ≥ `PAY_P25`). Seed melhor herda a prova do pior;
//! 2. é seed de catálogo com 3+ vendas próprias: as vendas dele passam no mesmo teste;
//! 3. é seed de catálogo com menos de 3 vendas: as vendas do grupo dele passam no teste mais duro (`POOL_P25`);
//! 4. não está em catálogo nenhum: 3+ vendas próprias, com o quartil de baixo a `MIN_RATIO` × ou mais.
//!
//! Em 12/11/2026 (45 759 vendas guardadas) a regra deixou 15 skins e 231 seeds (eram 15 grupos): a Desert Eagle
//! Heat Treated foi de 43 para 104 seeds, e saíram seeds de tier 1 que vendem a preço comum (Solitude 95/112/154,
//! Trace Lock 309/390). Prevendo cada venda só com as anteriores (`backtest`): 336 vendas de seed pagante, 97 %
//! saíram a ≥ 1,15× o comum, real ÷ previsto 1,12 (mediana). Medida repetível: teste `catalog_on_a_real_db`.
//! shortcut: os limites (1,5 / 1,2 / 2,0, 8 vizinhas) foram escolhidos olhando o mesmo retrato de 12/11 em que a
//! prova retroativa foi medida; o placar de `hunt::value` refaz a conta a cada hora com as vendas novas,
//! recalibrar por ele.

use super::Hunt;
use super::catalog::{self, Item};
use crate::pricing::groups::{
    Group, catalog_seeds, float_band, group_of, score, set_paying, siblings, skin_base, table_skins,
};
use crate::pricing::sell::{MAX_RARE_MULTIPLE, RARE_MIN_SALES};
use anyhow::Result;
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// shortcut: uma skin a cada 2 min (~720 por dia, dentro da cota do CSFloat); subir se a cota sobrar.
const GAP: i64 = 120;
// shortcut: aquece os nomes vigiados a cada 15 s; reavaliar após medir cota e latência em produção.
const WARM_GAP: i64 = 15;
/// A mesma skin só é relida depois disso.
const REFETCH: i64 = 24 * 3600;
/// Seed fora de catálogo: o quartil de baixo das vendas dele precisa chegar a esta razão.
pub const MIN_RATIO: f64 = 2.0;
/// Menos vendas limpas que isso não dão uma mediana confiável.
const MIN_CLEAN_SALES: usize = 10;
/// Skins com menos vendas por dia quase não têm anúncio novo para caçar.
const MIN_SALES_PER_DAY: f64 = 5.0;
/// Vendas vizinhas (nota igual ou pior) que provam um seed de skin com nota.
const NEAR: usize = 8;
/// Prova própria ou por nota: mediana e quartil de baixo das razões.
const PAY_MEDIAN: f64 = 1.5;
const PAY_P25: f64 = 1.2;
/// Prova emprestada do grupo de catálogo: mais dura, o seed ainda não vendeu.
const POOL_P25: f64 = 1.5;

/// Skin-base → seed que paga → razões (preço ÷ preço comum do nome) das vendas que provam, em ordem crescente.
/// Skin medida sem seed pagante fica com o mapa vazio.
pub type Learned = HashMap<String, BTreeMap<u16, Vec<f64>>>;

/// Venda limpa com seed, relativa ao preço comum do nome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comp {
    pub seed: u16,
    pub ratio: f64,
    pub sold_at: i64,
}

/// Linha de `csfloat_sales`: `(nome, seed, múltiplo, float, sold_at)`, em ordem de nome.
pub type SaleRow = (String, Option<u16>, f64, Option<f64>, i64);

/// Skin-base → vendas comparáveis. Nome com menos de `MIN_CLEAN_SALES` vendas não tem preço comum confiável
/// e fica de fora; venda que o float de elite explica, sem seed ou absurda não ensina pattern.
pub fn comps(rows: &[SaleRow]) -> HashMap<String, Vec<Comp>> {
    let mut out: HashMap<String, Vec<Comp>> = HashMap::new();
    for of_name in rows.chunk_by(|a, b| a.0 == b.0) {
        if of_name.len() < MIN_CLEAN_SALES {
            continue;
        }
        let name = &of_name[0].0;
        let mut m: Vec<f64> = of_name.iter().map(|r| r.2).collect();
        m.sort_by(f64::total_cmp);
        let median = m[m.len() / 2];
        if !(median.is_finite() && median > 0.0) {
            continue;
        }
        let band = float_band(name);
        let of_base = out.entry(skin_base(name).to_string()).or_default();
        for (_, seed, multiple, float, sold_at) in of_name {
            let ratio = multiple / median;
            let elite = float
                .zip(band)
                .is_some_and(|(f, (lo, hi))| (lo..=hi).contains(&f));
            if let Some(seed) = *seed
                && !elite
                && ratio > 0.0
                && ratio <= MAX_RARE_MULTIPLE
            {
                of_base.push(Comp {
                    seed,
                    ratio,
                    sold_at: *sold_at,
                });
            }
        }
    }
    out
}

/// As razões que provam que o seed paga, só com vendas anteriores a `before`; `None` se não paga (regra no
/// topo do arquivo).
pub fn evidence_for(base: &str, seed: u16, comps: &[Comp], before: i64) -> Option<Vec<f64>> {
    let pays = |mut v: Vec<f64>, median: f64, p25: f64| {
        v.sort_by(f64::total_cmp);
        // quartil de baixo arredondado para baixo: com 3 ou 4 vendas é a pior delas
        (v.len() >= RARE_MIN_SALES && v[v.len() / 2] >= median && v[(v.len() - 1) / 4] >= p25)
            .then_some(v)
    };
    let past = comps.iter().filter(|c| c.sold_at < before);
    if let Some(mine) = score(base, seed) {
        let mut near: Vec<(f64, f64)> = past
            .filter_map(|c| {
                let s = score(base, c.seed)?;
                (s <= mine).then_some((s, c.ratio))
            })
            .collect();
        near.sort_by(|a, b| b.0.total_cmp(&a.0));
        near.truncate(NEAR);
        return pays(near.into_iter().map(|n| n.1).collect(), PAY_MEDIAN, PAY_P25);
    }
    let own: Vec<f64> = past
        .clone()
        .filter(|c| c.seed == seed)
        .map(|c| c.ratio)
        .collect();
    match siblings(base, seed) {
        Some(_) if own.len() >= RARE_MIN_SALES => pays(own, PAY_MEDIAN, PAY_P25),
        Some(group) => pays(
            past.filter(|c| group.contains(&c.seed))
                .map(|c| c.ratio)
                .collect(),
            0.0,
            POOL_P25,
        ),
        None => pays(own, 0.0, MIN_RATIO),
    }
}

/// Os seeds que pagam em cada skin medida, com a prova de cada um.
pub fn learn(comps: &HashMap<String, Vec<Comp>>) -> Learned {
    comps
        .iter()
        .map(|(base, v)| {
            // skin com nota: qualquer seed pode herdar a prova; sem nota, só quem vendeu ou é de catálogo
            let seeds: BTreeSet<u16> = if score(base, 0).is_some() {
                (0..=1000).collect()
            } else {
                v.iter()
                    .map(|c| c.seed)
                    .chain(catalog_seeds(base))
                    .collect()
            };
            let paying = seeds
                .into_iter()
                .filter_map(|s| Some((s, evidence_for(base, s, v, i64::MAX)?)))
                .collect();
            (base.clone(), paying)
        })
        .collect()
}

/// Placar da regra fora da amostra: para cada venda cujo seed já pagava só com as vendas anteriores a ela,
/// `(razão prevista, razão real)`. A previsão é a mediana da prova.
pub fn backtest(comps: &HashMap<String, Vec<Comp>>) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for (base, v) in comps {
        for c in v {
            if let Some(e) = evidence_for(base, c.seed, v, c.sold_at) {
                out.push((e[e.len() / 2], c.ratio));
            }
        }
    }
    out
}

fn candidates(mut items: Vec<Item>, saldo: i64, ratio: Option<f64>, now: i64) -> Vec<String> {
    items.retain(|i| {
        i.sales_per_day.is_some_and(|s| s >= MIN_SALES_PER_DAY)
            && !i.name.starts_with('★')
            && i.name.contains('|')
            && catalog::wear_of(&i.name).is_some()
            && (saldo <= 0
                || ratio
                    .and_then(|r| i.triage_brl(now, r))
                    .is_some_and(|price| price <= saldo))
    });
    items.sort_by(|a, b| {
        b.sales_per_day
            .unwrap_or_default()
            .total_cmp(&a.sales_per_day.unwrap_or_default())
            .then(a.name.cmp(&b.name))
    });
    items.into_iter().map(|i| i.name).collect()
}

/// Nomes de skin de catálogo de pattern, do mais vendido ao menos: lidos mesmo com pouco giro ou acima do
/// saldo, porque é a venda deles que prova (ou derruba) o seed.
fn catalog_names(items: &[Item]) -> Vec<String> {
    let table: HashSet<String> = table_skins().into_iter().collect();
    let mut v: Vec<&Item> = items
        .iter()
        .filter(|i| catalog::wear_of(&i.name).is_some() && table.contains(skin_base(&i.name)))
        .collect();
    v.sort_by(|a, b| {
        b.sales_per_day
            .unwrap_or_default()
            .total_cmp(&a.sales_per_day.unwrap_or_default())
            .then(a.name.cmp(&b.name))
    });
    v.into_iter().map(|i| i.name.clone()).collect()
}

/// Seeds que pagam na skin.
pub fn active(l: &Learned, base: &str) -> Vec<u16> {
    l.get(base).map_or(vec![], |m| m.keys().copied().collect())
}

/// Grupo do seed que paga e a prova dele.
pub fn group(l: &Learned, name: &str, seed: Option<u16>) -> Option<(Group, Vec<f64>)> {
    let base = skin_base(name);
    let seed = seed?;
    let v = l.get(base)?.get(&seed)?;
    // `group_of` lê a mesma medida (`set_paying`); o reserva cobre a tabela montada à mão em teste
    let g = group_of(name, Some(seed.into()), None, None).unwrap_or_else(|| Group {
        key: format!("learned:{base}:{seed}"),
        label: format!("Pattern #{seed}"),
    });
    Some((g, v.clone()))
}

/// `(skins com seed pagante, seeds pagantes)`.
pub fn size(l: &Learned) -> (usize, usize) {
    (
        l.values().filter(|m| !m.is_empty()).count(),
        l.values().map(BTreeMap::len).sum(),
    )
}

const SALES_SQL: &str = "SELECT name, seed, multiple, \"float\", sold_at FROM csfloat_sales
     WHERE multiple > 0 ORDER BY name, sold_at, seed";

fn read_sales(c: &rusqlite::Connection) -> rusqlite::Result<Vec<SaleRow>> {
    c.prepare(SALES_SQL)?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect()
}

pub(crate) async fn sale_rows(h: &Hunt) -> Result<Vec<SaleRow>> {
    h.db.call(|c| read_sales(c)).await
}

/// Refaz a medida com as vendas guardadas e publica os seeds pagantes. Barato: uma leitura da tabela.
// shortcut: o `grp` já gravado em `csfloat_sales` não é refeito quando um seed entra ou sai; ele só pesa na
// prova de skin ainda sem medida. Regravar se o selo passar a depender dele.
pub async fn load(h: &Hunt) -> Result<()> {
    let rows = sale_rows(h).await?;
    // skin com nota confere os 1 001 seeds contra todas as vendas dela: fora do laço da caça
    let l = tokio::task::spawn_blocking(move || learn(&comps(&rows))).await?;
    set_paying(
        l.iter()
            .map(|(base, m)| (base.clone(), m.keys().copied().collect()))
            .collect(),
    );
    let (old, new) = (size(&h.lock().learned), size(&l));
    h.lock().learned = l;
    if old != new {
        tracing::info!(skins = new.0, seeds = new.1, "catálogo de patterns medido");
        let at = h.now();
        h.db.call(move |c| {
            c.execute(
                "INSERT INTO refit_log(at, key, old, new, why) VALUES (?1, 'catalogo:seeds', ?2, ?3, ?4)",
                params![at, old.1 as f64, new.1 as f64, format!("{} skins com seed que paga", new.0)],
            )
        })
        .await?;
    }
    Ok(())
}

pub(crate) async fn load_cache(h: &Hunt) -> Result<()> {
    let now = h.now();
    let cutoff = now - super::catalog::HISTORY_TTL;
    let rows: Vec<(String, i64, String)> =
        h.db.call(move |c| {
            c.prepare("SELECT name, fetched_at, json FROM csfloat_cache WHERE fetched_at > ?1 AND fetched_at <= ?2")?
                .query_map([cutoff, now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })
        .await?;
    for (name, at, json) in rows {
        match serde_json::from_str(&json) {
            Ok(sales) => {
                h.lock().csfloat_cache.insert(name, (at, sales));
            }
            Err(e) => tracing::warn!("cache de vendas ignorado para {name}: {e}"),
        }
    }
    Ok(())
}

pub(crate) async fn cache_sales(h: &Hunt, name: &str, sales: &[crate::csfloat::CsfloatSale]) {
    let at = h.now();
    let n = name.to_string();
    match serde_json::to_string(sales) {
        Ok(json) => {
            if let Err(e) = h.db.call(move |c| {
                c.execute("INSERT INTO csfloat_cache(name,fetched_at,json) VALUES (?1,?2,?3)
                    ON CONFLICT(name) DO UPDATE SET fetched_at=excluded.fetched_at,json=excluded.json",
                    params![n,at,json])?;
                Ok(())
            }).await { tracing::warn!("cache de vendas não persistiu: {e:#}"); }
        }
        Err(e) => tracing::warn!("cache de vendas não serializou: {e}"),
    }
    h.lock()
        .csfloat_cache
        .insert(name.into(), (at, sales.to_vec()));
    super::teacher::store_csfloat(h, name, sales).await;
}

/// Lê as vendas da próxima skin do catálogo, se é hora. Não gasta consulta da Steam: só o CSFloat.
pub async fn maybe_step(h: &Hunt) {
    let now = h.now();
    // saldo desconhecido (0): qualquer piso; aprender não compra nada
    let (saldo, ratio, seen) = {
        let mut s = h.lock();
        // a primeira leitura só depois de `GAP` ligado: a caça começa antes
        if s.learn_at == 0 {
            s.learn_at = now;
        }
        let warming = h.csfloat.status().is_none_or(|(ok, _)| ok)
            && s.queue.tasks().iter().any(|t| {
                s.queue.external.as_ref() != Some(&t.id)
                    && s.learn_seen.get(&t.name).is_none_or(|at| now - at >= 600)
                    && s.csfloat_cache
                        .get(&t.name)
                        .is_none_or(|(at, _)| now - at >= super::catalog::HISTORY_TTL)
            });
        if now - s.learn_at < if warming { WARM_GAP } else { GAP } {
            return;
        }
        s.learn_at = now;
        (s.saldo, s.ratio.map(|(r, _)| r), s.learn_seen.clone())
    };
    let names = match catalog::load_items(&h.db).await {
        Ok(items) => {
            // metade das leituras para as skins de catálogo de pattern, metade para descobrir seed novo
            let (table, rest) = (catalog_names(&items), candidates(items, saldo, ratio, now));
            if (now / GAP) % 2 == 0 {
                [table, rest].concat()
            } else {
                [rest, table].concat()
            }
        }
        Err(e) => return tracing::warn!("patterns: catálogo ilegível: {e}"),
    };
    let name = {
        let s = h.lock();
        // A mesma vaga aquece qualquer nome vigiado, inclusive os de giro baixo/sem piso do catálogo.
        s.queue
            .tasks()
            .into_iter()
            .filter(|t| s.queue.external.as_ref() != Some(&t.id))
            .map(|t| t.name)
            .filter(|n| {
                seen.get(n).is_none_or(|at| now - at >= 600)
                    && s.csfloat_cache
                        .get(n)
                        .is_none_or(|(at, _)| now - at >= super::catalog::HISTORY_TTL)
            })
            .min_by_key(|n| (seen.get(n).copied().unwrap_or(0), n.clone()))
            .or_else(|| {
                names
                    .into_iter()
                    .find(|n| seen.get(n).is_none_or(|at| now - at >= REFETCH))
            })
    };
    let Some(name) = name else {
        return;
    };
    h.lock().learn_seen.insert(name.clone(), now);
    let sales = match h.csfloat.sales(&name).await {
        Ok(s) => s,
        Err(e) => return tracing::warn!("patterns: CSFloat falhou para {name}: {e}"),
    };
    cache_sales(h, &name, &sales).await;
    let before = active(&h.lock().learned, skin_base(&name)).len();
    if let Err(e) = load(h).await {
        return tracing::warn!("patterns: reler falhou: {e}");
    }
    let now_active = active(&h.lock().learned, skin_base(&name));
    if now_active.len() > before {
        tracing::info!(skin = skin_base(&name), seeds = ?now_active, "pattern provado");
        // a seleção põe o seed novo na agenda na próxima volta
        h.lock().selected = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_use_measured_or_fresh_reference_without_inventing_price_or_sales() {
        let item = Item {
            name: "AK-47 | Redline (Field-Tested)".into(),
            code: None,
            floor_usd_cents: None,
            reference_usd_cents: Some(100),
            reference_at: Some(100),
            history_price_cents: None,
            rank: 0,
            sales_per_day: Some(5.0),
            dispersion: None,
            history_at: None,
        };
        assert_eq!(candidates(vec![item.clone()], 600, Some(5.0), 100).len(), 1);
        assert!(candidates(vec![item.clone()], 400, Some(5.0), 100).is_empty());
        assert!(candidates(vec![item.clone()], 600, None, 100).is_empty());
        assert!(candidates(vec![item.clone()], 600, Some(5.0), 100 + 8 * 86_400).is_empty());
        let mut missing = item.clone();
        missing.sales_per_day = None;
        assert!(candidates(vec![missing], 600, Some(5.0), 100).is_empty());
        let mut no_price = item.clone();
        no_price.reference_usd_cents = None;
        assert_eq!(candidates(vec![no_price], 0, None, 100).len(), 1);
        let mut measured = item;
        measured.reference_usd_cents = None;
        measured.floor_usd_cents = Some(100);
        assert_eq!(candidates(vec![measured], 600, Some(5.0), 100).len(), 1);
    }

    const T: i64 = 1_793_000_000;

    /// 12 vendas comuns (seeds 1–12 a 1×) e as vendas pedidas, todas do mesmo nome.
    fn rows(name: &str, extra: &[(u16, f64)]) -> Vec<SaleRow> {
        (1..=12)
            .map(|i| (i, 1.0))
            .chain(extra.iter().copied())
            .enumerate()
            .map(|(i, (seed, m))| (name.to_string(), Some(seed), m, Some(0.3), T + i as i64))
            .collect()
    }

    fn paying(name: &str, extra: &[(u16, f64)]) -> Vec<u16> {
        active(&learn(&comps(&rows(name, extra))), skin_base(name))
    }

    #[test]
    fn unknown_seed_needs_three_sales_that_all_pay() {
        const MP9: &str = "MP9 | Starlight Protector (Minimal Wear)";
        assert_eq!(paying(MP9, &[(890, 9.75), (890, 5.0)]), Vec::<u16>::new());
        assert_eq!(paying(MP9, &[(890, 9.75), (890, 5.0), (890, 3.0)]), [890]);
        // uma venda perto do comum no quartil de baixo derruba: o seed não paga sempre
        assert_eq!(
            paying(MP9, &[(890, 9.75), (890, 5.0), (890, 3.0), (890, 1.1)]),
            Vec::<u16>::new()
        );
        // dado ruim (500×) e venda sem seed não ensinam
        let mut r = rows(MP9, &[(891, 500.0), (891, 500.0), (891, 500.0)]);
        r.push((MP9.into(), None, 9.0, None, T));
        let l = learn(&comps(&r));
        assert_eq!(size(&l), (0, 0));
        assert!(
            l.contains_key("MP9 | Starlight Protector"),
            "medida, sem seed"
        );
        // poucas vendas: sem preço comum, a skin nem é medida
        assert!(comps(&r[..5]).is_empty());
        // float de elite explica a venda: não ensina seed (MW vai de 0,07 a 0,15; elite até 0,078)
        let mut f = rows(MP9, &[]);
        f.extend((0..3).map(|i| (MP9.to_string(), Some(77), 4.0, Some(0.071), T + 50 + i)));
        f.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(size(&learn(&comps(&f))), (0, 0));
    }

    #[test]
    fn catalog_seed_pays_by_its_own_sales_or_by_its_group() {
        // PAW: Golden Cat = 41 e 350; Stoner Cat = 420 e 788
        const PAW: &str = "AWP | PAW (Field-Tested)";
        // 41 vendeu 3× bem: paga por conta própria e empresta a prova ao 350, que nunca vendeu
        assert_eq!(paying(PAW, &[(41, 2.0), (41, 1.6), (41, 1.5)]), [41, 350]);
        // 3 vendas próprias fracas valem mais que o grupo: 350 sai, 41 fica
        assert_eq!(
            paying(
                PAW,
                &[
                    (41, 2.0),
                    (41, 1.6),
                    (41, 1.5),
                    (350, 1.0),
                    (350, 1.1),
                    (350, 1.0)
                ]
            ),
            [41]
        );
        // grupo com venda fraca no quartil de baixo não empresta prova
        assert_eq!(paying(PAW, &[(41, 2.0), (41, 1.3)]), Vec::<u16>::new());
        // a identidade do grupo é a da tabela, com a prova medida
        let l = learn(&comps(&rows(PAW, &[(41, 2.0), (41, 1.6), (41, 1.5)])));
        let (g, e) = group(&l, "StatTrak™ AWP | PAW (Minimal Wear)", Some(350)).unwrap();
        assert_eq!(g.key, "learned:AWP | PAW:350");
        assert!(g.label.ends_with("#350"), "{}", g.label);
        assert_eq!(e, [1.5, 1.6, 2.0]);
        assert_eq!(group(&l, PAW, Some(1)), None);
        assert_eq!(group(&l, PAW, None), None);
    }

    #[test]
    fn scored_skin_inherits_proof_from_worse_seeds_and_backtest_only_looks_back() {
        const SSG: &str = "SSG 08 | Acid Fade (Factory New)";
        let base = skin_base(SSG);
        let mut by_score: Vec<u16> = (0..=1000).collect();
        by_score.sort_by(|a, b| score(base, *b).partial_cmp(&score(base, *a)).unwrap());
        // 12 vendas comuns nos seeds de fade mais baixo, depois 10 vendas caras nos 10 de fade mais alto
        let top = &by_score[..10];
        let sale = |i: usize, seed: u16, m: f64| -> SaleRow {
            (SSG.to_string(), Some(seed), m, Some(0.03), T + i as i64)
        };
        let commons = by_score[989..].iter().enumerate();
        let mut r: Vec<SaleRow> = commons.map(|(i, s)| sale(i, *s, 1.0)).collect();
        r.extend(top.iter().enumerate().map(|(i, s)| sale(12 + i, *s, 3.0)));
        let c = comps(&r);
        let pay = active(&learn(&c), base);
        // o melhor seed herda a prova das 8 vendas de nota igual ou pior mais próximas; o de fade médio só tem
        // as comuns abaixo dele
        assert!(pay.contains(&top[0]), "{pay:?}");
        assert!(
            !pay.contains(&by_score[500]) && pay.len() < 30,
            "{}",
            pay.len()
        );
        // a prova só olha para trás: antes da primeira venda cara ninguém pagava
        assert_eq!(evidence_for(base, top[0], &c[base], T + 12), None);
        // uma 11ª venda no melhor seed já tinha as anteriores a favor: previsto 3×, real 2×
        r.push(sale(100, top[0], 2.0));
        assert_eq!(backtest(&comps(&r)).last(), Some(&(3.0, 2.0)));
    }

    #[test]
    #[ignore = "medida: SNIPER_DB aponta para um banco de verdade (só leitura); imprime o catálogo e a prova retroativa"]
    fn catalog_on_a_real_db() {
        let path = std::env::var("SNIPER_DB").expect("SNIPER_DB");
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let c = rusqlite::Connection::open_with_flags(path, flags).unwrap();
        let rows = read_sales(&c).unwrap();
        // menor piso (centavos de dólar) entre os desgastes de cada skin
        let mut floor: HashMap<String, i64> = HashMap::new();
        let mut q = c
            .prepare("SELECT name, floor_cents FROM items WHERE floor_cents > 0")
            .unwrap();
        let items = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)));
        for (name, cents) in items.unwrap().flatten() {
            let e = floor.entry(skin_base(&name).to_string()).or_insert(cents);
            *e = (*e).min(cents);
        }
        let started = std::time::Instant::now();
        let comps = comps(&rows);
        let l = learn(&comps);
        println!("{} vendas, medida em {:?}", rows.len(), started.elapsed());
        let mut skins: Vec<_> = l.iter().filter(|(_, m)| !m.is_empty()).collect();
        skins.sort_by_key(|(base, _)| floor.get(*base).copied().unwrap_or(i64::MAX));
        for (base, m) in skins {
            let mut mid: Vec<f64> = m.values().map(|e| e[e.len() / 2]).collect();
            mid.sort_by(f64::total_cmp);
            println!(
                "piso US$ {:>7.2} | {base}: {} seeds, prova mediana {:.2}x ({:.2}x a {:.2}x)",
                floor.get(base).copied().unwrap_or(0) as f64 / 100.0,
                m.len(),
                mid[mid.len() / 2],
                mid[0],
                mid[mid.len() - 1]
            );
        }
        println!("total (skins, seeds): {:?}", size(&l));
        let (n, paid, ratio) = crate::hunt::value::backtest_summary(&backtest(&comps));
        println!("prova retroativa: {n} vendas, pagaram {paid:?}, real/previsto {ratio:?}");
    }

    #[test]
    fn catalog_names_come_even_when_slow_or_expensive() {
        let item = |name: &str, sales: Option<f64>| Item {
            name: name.into(),
            code: None,
            floor_usd_cents: Some(100_000),
            reference_usd_cents: None,
            reference_at: None,
            history_price_cents: None,
            rank: 0,
            sales_per_day: sales,
            dispersion: None,
            history_at: None,
        };
        let items = vec![
            item("AK-47 | Redline (Field-Tested)", Some(50.0)),
            item("AWP | PAW (Factory New)", Some(0.2)),
            item("Glock-18 | Moonrise (Field-Tested)", Some(9.0)),
            item("★ Karambit | Fade (Factory New)", Some(1.0)),
        ];
        assert_eq!(
            catalog_names(&items),
            [
                "Glock-18 | Moonrise (Field-Tested)",
                "AWP | PAW (Factory New)"
            ]
        );
    }
}
