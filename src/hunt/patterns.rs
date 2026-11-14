//! Quais seeds valem caçar e quanto prêmio se pode esperar deles, medido nas vendas do CSFloat (`csfloat_sales`, sem
//! adesivo). Tabela, guia ou lista da comunidade só indicam candidatos (`pricing::groups`); quem decide são as vendas.
//!
//! Cada venda vira uma razão sobre o preço comum do nome (a mediana das vendas dele). O prêmio de um seed é o
//! valor esperado encolhido (`pricing::rarity`): vendas do próprio seed, do grupo (seeds irmãos de catálogo, ou
//! as 8 notas vizinhas e piores em skin com nota de % de azul/fade), do resto da skin e, se existir, da tabela
//! externa (`external`). Sem prova sobra o preço comum, com pouca o prêmio fica perto dele e o limite inferior
//! cai. O seed vira candidato (`Learned`) quando o esperado, já com o corte de saída (15 %) e a tendência de 30
//! dias do grupo, rende a margem mínima de lista comprando no piso comum; guarda `[limite inferior, esperado,
//! limite de cima]` já realistas. Quem compra no aviso é o limite inferior (`hunt::detect`).
//!
//! - skin com nota: seed melhor herda a prova do pior; se as 8 vendas de nota igual ou melhor mais próximas não
//!   sustentam o prêmio, o seed também não passa dele (veto de cima);
//! - sem nota a prova vale por desgaste: só contam vendas no desgaste do anúncio ou num pior (13/11/2026, em pares do
//!   mesmo seed que pagava ≥ 1,5× no desgaste melhor, só 44 de 221 ainda pagavam ≥ 1,5× no pior);
//! - o encolhimento da skin cai quando o seed pesa no preço dela de forma significativa (`rarity::seed_k`): é
//!   assim que uma skin sem tabela entra sozinha;
//! - se a tabela externa mostra bem menos prêmio que as vendas daqui, o seed é pulado.
//!
//! Medida repetível, sobre uma cópia do banco: teste `catalog_on_a_real_db` (catálogo, prova fora da amostra e
//! quantos anúncios já observados a regra aprovaria). Antes desta regra (13/11/2026, 48 411 vendas): 18 skins e
//! 239 seeds pelo "provado ou não".
// shortcut: os limites (K, SIGMA, 1,64 desvio, corte de 15 %, 8 vizinhas) são hipóteses; o placar de `hunt::value`
// refaz a conta a cada hora com as vendas novas, recalibrar por ele.

use super::Hunt;
use super::catalog::{self, Item};
use super::external::External;
use crate::pricing::groups::{
    Group, catalog_seeds, float_band, group_of, score, set_paying, siblings, skin_base,
    table_skins, wear_index,
};
use crate::pricing::rarity::{
    Evidence, Posterior, clears_margin, diverges, posterior, seed_k, trend, triple,
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
/// Menos vendas limpas que isso não dão uma mediana confiável.
const MIN_CLEAN_SALES: usize = 10;
/// Skins com menos vendas por dia quase não têm anúncio novo para caçar.
const MIN_SALES_PER_DAY: f64 = 5.0;
/// Vendas vizinhas (nota igual ou pior) que servem de grupo a um seed de skin com nota.
const NEAR: usize = 8;
/// Desgastes de `wear_index`.
const WEARS: u8 = 5;

/// Skin-base → (seed, desgaste) que vale caçar → `[limite inferior, esperado, limite de cima]` do prêmio (preço ÷
/// preço comum do nome, já com o corte de saída). Skin medida sem seed candidato fica com o mapa vazio.
pub type Learned = HashMap<String, BTreeMap<(u16, u8), Vec<f64>>>;

/// Venda limpa com seed, relativa ao preço comum do nome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comp {
    pub seed: u16,
    /// Desgaste do nome vendido (`wear_index`).
    pub wear: u8,
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
                    wear: wear_index(name),
                    ratio,
                    sold_at: *sold_at,
                });
            }
        }
    }
    out
}

/// Os níveis de prova de um seed (razão, `sold_at`), sem venda repetida entre eles, só com vendas anteriores a
/// `before`; `cap` = o que o veto de cima (skin com nota) deixa o seed passar.
struct Levels {
    own: Vec<(f64, i64)>,
    group: Vec<(f64, i64)>,
    skin: Vec<(f64, i64)>,
    cap: Option<Posterior>,
}

fn pairs(v: &[&Comp]) -> Vec<(f64, i64)> {
    v.iter().map(|c| (c.ratio, c.sold_at)).collect()
}

fn ratios(v: &[(f64, i64)]) -> Vec<f64> {
    v.iter().map(|x| x.0).collect()
}

fn levels(base: &str, seed: u16, wear: u8, comps: &[Comp], before: i64, k: f64) -> Levels {
    let past: Vec<&Comp> = comps.iter().filter(|c| c.sold_at < before).collect();
    if let Some(mine) = score(base, seed) {
        // com nota a prova não depende do desgaste: a nota explica o prêmio
        let (own, rest): (Vec<&Comp>, Vec<&Comp>) = past.iter().partition(|c| c.seed == seed);
        let scored: Vec<(f64, &Comp)> = rest
            .iter()
            .filter_map(|c| Some((score(base, c.seed)?, *c)))
            .collect();
        let mut below: Vec<(f64, &Comp)> = scored.iter().filter(|n| n.0 <= mine).copied().collect();
        below.sort_by(|a, b| b.0.total_cmp(&a.0));
        below.truncate(NEAR);
        let group: Vec<&Comp> = below.iter().map(|n| n.1).collect();
        let skin: Vec<&Comp> = rest
            .iter()
            .filter(|c| !group.iter().any(|g| std::ptr::eq(*g, **c)))
            .copied()
            .collect();
        // Veto de cima: se as vendas de nota igual ou um pouco melhor não sustentam o prêmio, o seed também não
        // passa dele, por mais que meia dúzia de vendas piores tenha saído cara (13/11: SSG Acid Fade #188, 81 % de
        // fade, "provado" por 7 vendas de ~80 % com 2 fora da curva, enquanto 81–88 % vende a 1,02×).
        let mut above: Vec<(f64, &Comp)> = scored.iter().filter(|n| n.0 >= mine).copied().collect();
        above.sort_by(|a, b| a.0.total_cmp(&b.0));
        above.truncate(NEAR);
        let skin = pairs(&skin);
        let above: Vec<f64> = above.iter().map(|n| n.1.ratio).collect();
        let cap = (above.len() >= NEAR).then(|| {
            let evidence = Evidence {
                group: &above,
                skin: &ratios(&skin),
                ..Default::default()
            };
            posterior(&evidence, k)
        });
        return Levels {
            own: pairs(&own),
            group: pairs(&group),
            skin,
            cap,
        };
    }
    // sem nota, venda em desgaste melhor não prova o pior
    let sibs = siblings(base, seed);
    let mut lv = Levels {
        own: vec![],
        group: vec![],
        skin: vec![],
        cap: None,
    };
    for c in past.iter().filter(|c| c.wear >= wear) {
        let level = if c.seed == seed {
            &mut lv.own
        } else if sibs.is_some_and(|g| g.contains(&c.seed)) {
            &mut lv.group
        } else {
            &mut lv.skin
        };
        level.push((c.ratio, c.sold_at));
    }
    lv
}

/// `[limite inferior, esperado, limite de cima]` do seed (razões já realistas, `rarity::triple`) se ele vale
/// caçar, só com vendas anteriores a `before`; `None` se o esperado não rende a margem de lista, se há menos de
/// `RARE_MIN_SALES` vendas dele e do grupo, ou se a tabela externa discorda. `k` = `rarity::seed_k` da skin.
pub fn estimate_for(
    base: &str,
    seed: u16,
    wear: u8,
    comps: &[Comp],
    before: i64,
    k: f64,
    ext: &External,
) -> Option<[f64; 3]> {
    let lv = levels(base, seed, wear, comps, before, k);
    if lv.own.len() + lv.group.len() < RARE_MIN_SALES {
        return None;
    }
    let external = ext.multiple(base, seed);
    let mut p = posterior(
        &Evidence {
            own: &ratios(&lv.own),
            group: &ratios(&lv.group),
            skin: &ratios(&lv.skin),
            external,
        },
        k,
    );
    if let Some(c) = lv.cap {
        p = Posterior {
            mean: p.mean.min(c.mean),
            lower: p.lower.min(c.lower),
            upper: p.upper.min(c.upper),
        };
    }
    // a tendência é a do mercado do seed (ele e o grupo), até o último dado que a estimativa pode ver
    let market: Vec<(f64, i64)> = lv.own.iter().chain(&lv.group).copied().collect();
    let now = market.iter().map(|m| m.1).max().unwrap_or(0) + 1;
    let t = triple(&p, trend(&market, now));
    (clears_margin(t[1]) && external.is_none_or(|x| !diverges(t[1], x))).then_some(t)
}

/// `k` de cada skin: o encolhimento do prêmio, menor onde o seed pesa no preço.
fn skin_k(v: &[Comp]) -> f64 {
    seed_k(&v.iter().map(|c| (c.seed, c.ratio.ln())).collect::<Vec<_>>())
}

/// Os seeds que valem caçar em cada skin medida, com o prêmio esperado de cada um (sem tabela externa).
#[cfg(test)]
pub fn learn(comps: &HashMap<String, Vec<Comp>>) -> Learned {
    learn_with(comps, &External::default())
}

pub fn learn_with(comps: &HashMap<String, Vec<Comp>>, ext: &External) -> Learned {
    comps
        .iter()
        .map(|(base, v)| {
            let k = skin_k(v);
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
                .flat_map(|s| {
                    // com nota a prova não depende do desgaste: mede uma vez
                    let scored = score(base, s)
                        .is_some()
                        .then(|| estimate_for(base, s, 0, v, i64::MAX, k, ext));
                    (0..WEARS).filter_map(move |w| {
                        let e =
                            scored.unwrap_or_else(|| estimate_for(base, s, w, v, i64::MAX, k, ext));
                        Some(((s, w), e?.to_vec()))
                    })
                })
                .collect();
            (base.clone(), paying)
        })
        .collect()
}

/// Previsão fora da amostra: o `k` de cada skin sai das vendas até a mediana do tempo (de todas as skins) e as
/// vendas depois dela são previstas só com as anteriores a cada uma. `([inferior, esperado, de cima], real)` das
/// que já tinham seed candidato.
pub fn backtest_rows(comps: &HashMap<String, Vec<Comp>>, ext: &External) -> Vec<([f64; 3], f64)> {
    let mut times: Vec<i64> = comps.values().flatten().map(|c| c.sold_at).collect();
    times.sort_unstable();
    let Some(&cutoff) = times.get(times.len() / 2) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (base, v) in comps {
        let train: Vec<Comp> = v.iter().filter(|c| c.sold_at < cutoff).copied().collect();
        let k = skin_k(&train);
        for c in v.iter().filter(|c| c.sold_at >= cutoff) {
            if let Some(t) = estimate_for(base, c.seed, c.wear, v, c.sold_at, k, ext) {
                out.push((t, c.ratio));
            }
        }
    }
    out
}

/// Placar da regra fora da amostra: `(razão esperada, razão real)` de cada venda prevista (`backtest_rows`).
pub fn backtest(comps: &HashMap<String, Vec<Comp>>) -> Vec<(f64, f64)> {
    backtest_rows(comps, &External::default())
        .into_iter()
        .map(|(t, real)| (t[1], real))
        .collect()
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

/// Seeds que pagam no nome (skin e desgaste).
pub fn active(l: &Learned, name: &str) -> Vec<u16> {
    let wear = wear_index(name);
    l.get(skin_base(name)).map_or(vec![], |m| {
        m.keys().filter(|k| k.1 == wear).map(|k| k.0).collect()
    })
}

/// Grupo do seed que paga e a prova dele.
pub fn group(l: &Learned, name: &str, seed: Option<u16>) -> Option<(Group, Vec<f64>)> {
    let base = skin_base(name);
    let seed = seed?;
    let v = l.get(base)?.get(&(seed, wear_index(name)))?;
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
        l.values()
            .map(|m| m.keys().map(|k| k.0).collect::<BTreeSet<_>>().len())
            .sum(),
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
    let ext = External::read();
    let l = tokio::task::spawn_blocking(move || learn_with(&comps(&rows), &ext)).await?;
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
    let before = active(&h.lock().learned, &name).len();
    if let Err(e) = load(h).await {
        return tracing::warn!("patterns: reler falhou: {e}");
    }
    let now_active = active(&h.lock().learned, &name);
    if now_active.len() > before {
        tracing::info!(skin = skin_base(&name), seeds = ?now_active, "pattern provado");
        // a seleção põe o seed novo na agenda na próxima volta
        h.lock().selected = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::rarity;

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
        active(&learn(&comps(&rows(name, extra))), name)
    }

    #[test]
    fn unknown_seed_needs_three_sales_and_a_weak_one_only_lowers_the_bound() {
        const MP9: &str = "MP9 | Starlight Protector (Minimal Wear)";
        assert_eq!(paying(MP9, &[(890, 9.75), (890, 5.0)]), Vec::<u16>::new());
        assert_eq!(paying(MP9, &[(890, 9.75), (890, 5.0), (890, 3.0)]), [890]);
        // uma venda perto do comum não derruba o esperado, mas o limite inferior cai
        let est = |extra: &[(u16, f64)]| {
            let c = comps(&rows(MP9, extra));
            estimate_for(
                "MP9 | Starlight Protector",
                890,
                1,
                &c["MP9 | Starlight Protector"],
                i64::MAX,
                rarity::K,
                &External::default(),
            )
        };
        let (strong, weak) = (
            est(&[(890, 9.75), (890, 5.0), (890, 3.0)]).unwrap(),
            est(&[(890, 9.75), (890, 5.0), (890, 3.0), (890, 1.1)]).unwrap(),
        );
        assert!(strong[0] < strong[1] && strong[1] < strong[2], "{strong:?}");
        assert!(weak[1] < strong[1], "{weak:?} {strong:?}");
        // pouca prova, prêmio pequeno: 3 vendas a 5× valem bem menos que 5×
        assert!(strong[1] < 3.0, "{strong:?}");
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
        // 41 vendeu 3× bem: paga por conta própria e empresta o prêmio ao 350, que nunca vendeu
        assert_eq!(paying(PAW, &[(41, 3.0), (41, 2.6), (41, 2.5)]), [41, 350]);
        // 3 vendas próprias fracas valem mais que o grupo: 350 sai, 41 fica
        assert_eq!(
            paying(
                PAW,
                &[
                    (41, 3.0),
                    (41, 2.6),
                    (41, 2.5),
                    (350, 1.0),
                    (350, 1.0),
                    (350, 1.0)
                ]
            ),
            [41]
        );
        // 2 vendas não bastam, nem emprestadas
        assert_eq!(paying(PAW, &[(41, 3.0), (41, 2.6)]), Vec::<u16>::new());
        // 3 vendas fracas do grupo não dão prêmio nenhum
        assert_eq!(
            paying(PAW, &[(41, 1.1), (41, 1.0), (41, 1.2)]),
            Vec::<u16>::new()
        );
        // a identidade do grupo é a da tabela, com o prêmio estimado: [inferior, esperado, de cima]
        let l = learn(&comps(&rows(PAW, &[(41, 3.0), (41, 2.6), (41, 2.5)])));
        let (g, e) = group(&l, "StatTrak™ AWP | PAW (Minimal Wear)", Some(350)).unwrap();
        assert_eq!(g.key, "learned:AWP | PAW:350");
        assert!(g.label.ends_with("#350"), "{}", g.label);
        assert!(
            e.len() == 3 && e[0] < e[1] && e[1] < e[2] && e[1] > 1.3,
            "{e:?}"
        );
        assert_eq!(group(&l, PAW, Some(1)), None);
        // as vendas são Field-Tested: provam o Factory New, não o Battle-Scarred (13/11: Snow Splash #309)
        assert!(group(&l, "AWP | PAW (Factory New)", Some(41)).is_some());
        assert_eq!(group(&l, "AWP | PAW (Battle-Scarred)", Some(41)), None);
        assert_eq!(active(&l, "AWP | PAW (Well-Worn)"), Vec::<u16>::new());
        assert_eq!(size(&l), (1, 2));
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
        let ext = External::default();
        assert_eq!(
            estimate_for(base, top[0], 0, &c[base], T + 12, rarity::K, &ext),
            None
        );
        // uma venda a mais no melhor seed já tinha as anteriores a favor: previsto bem acima do comum (com o corte
        // de saída e o encolhimento, abaixo dos 3× das vendas), real 2×
        r.push(sale(100, top[0], 2.0));
        let (predicted, real) = *backtest(&comps(&r)).last().unwrap();
        assert_eq!(real, 2.0);
        assert!((1.3..3.0).contains(&predicted), "{predicted}");
    }

    #[test]
    fn skin_without_a_table_enters_on_its_own_when_the_seed_effect_is_significant() {
        const NAME: &str = "AUG | Foo (Field-Tested)";
        let sales = |effect: bool| -> Vec<SaleRow> {
            let mut r: Vec<SaleRow> = (0..12u16)
                .flat_map(|i| (0..3).map(move |j| (i, j)))
                .enumerate()
                .map(|(n, (i, j))| {
                    // seeds 100–103 vendem a ~3× toda vez e os outros a ~1×; sem efeito de seed, todos oscilam entre
                    // 0,7× e 1,3× sem ligação com o seed
                    let m = if effect {
                        (if i < 4 { 3.0 } else { 1.0 }) + f64::from(j) * 0.05
                    } else {
                        1.0 + f64::from((u32::from(i) * 7 + j as u32 * 13) % 5) * 0.15 - 0.3
                    };
                    (NAME.to_string(), Some(100 + i), m, Some(0.3), T + n as i64)
                })
                .collect();
            r.sort_by(|a, b| a.0.cmp(&b.0));
            r
        };
        // o seed pesa de verdade: os 4 caros entram, os 8 comuns não, e a skin nunca esteve em tabela
        let l = learn(&comps(&sales(true)));
        assert_eq!(active(&l, NAME), [100, 101, 102, 103]);
        // sem efeito do seed (a variação não depende dele): nenhum seed passa
        assert_eq!(
            active(&learn(&comps(&sales(false))), NAME),
            Vec::<u16>::new()
        );
    }

    #[test]
    fn a_falling_group_and_a_disagreeing_external_table_cost_priority() {
        const PAW: &str = "AWP | PAW (Field-Tested)";
        let day = 86_400;
        let sale = |seed: u16, m: f64, days_ago: i64, i: i64| -> SaleRow {
            (
                PAW.to_string(),
                Some(seed),
                m,
                Some(0.3),
                T - days_ago * day + i,
            )
        };
        let est = |rows: Vec<SaleRow>, ext: &External| {
            let c = comps(&rows);
            estimate_for(
                "AWP | PAW",
                41,
                2,
                &c["AWP | PAW"],
                i64::MAX,
                rarity::K,
                ext,
            )
        };
        let commons = || (0..12).map(|i| sale(900 + i as u16, 1.0, 1, i));
        let steady: Vec<SaleRow> = commons()
            .chain((0..6).map(|i| sale(41, 3.0, 20 - i, 100 + i)))
            .collect();
        // mesmas vendas, o grupo vinha a 3× e agora sai a 2×: o previsto cai mais que o encolhimento explica
        let falling: Vec<SaleRow> = commons()
            .chain((0..3).map(|i| sale(41, 3.0, 25 - i, 100 + i)))
            .chain((0..3).map(|i| sale(41, 2.0, 2 + i, 100 + i)))
            .collect();
        let none = External::default();
        let (a, b) = (est(steady, &none).unwrap(), est(falling, &none).unwrap());
        assert!(b[1] < a[1] - 0.2, "{a:?} {b:?}");
        // tabela externa que mede o grupo a 1,00×: o seed é pulado; a que concorda deixa passar
        let json = |m: f64| {
            format!(r#"{{"skins": {{"AWP | PAW": [{{"seeds": [41], "multiple": {m}}}]}}}}"#)
        };
        let rows = || -> Vec<SaleRow> {
            commons()
                .chain((0..6).map(|i| sale(41, 3.0, 20 - i, 100 + i)))
                .collect()
        };
        assert_eq!(est(rows(), &External::parse(&json(1.0)).unwrap()), None);
        assert!(est(rows(), &External::parse(&json(2.8)).unwrap()).is_some());
    }

    #[test]
    fn low_fade_with_expensive_outliers_below_is_vetoed_by_common_sales_above() {
        const SSG: &str = "SSG 08 | Acid Fade (Factory New)";
        let base = skin_base(SSG);
        let mut by_score: Vec<u16> = (0..=1000).filter(|&s| score(base, s).is_some()).collect();
        by_score.sort_by(|a, b| score(base, *a).partial_cmp(&score(base, *b)).unwrap());
        let sale = |i: usize, seed: u16, m: f64| -> SaleRow {
            (SSG.to_string(), Some(seed), m, Some(0.03), T + i as i64)
        };
        // 5 vendas caras nos fades mais baixos (fora da curva) e 8 comuns logo acima deles
        let mut r: Vec<SaleRow> = by_score[..5]
            .iter()
            .enumerate()
            .map(|(i, s)| sale(i, *s, 3.0))
            .collect();
        r.extend(
            by_score[20..28]
                .iter()
                .enumerate()
                .map(|(i, s)| sale(10 + i, *s, 1.0)),
        );
        let target = by_score[10];
        let c = comps(&r);
        let est = |c: &HashMap<String, Vec<Comp>>| {
            estimate_for(
                base,
                target,
                0,
                &c[base],
                i64::MAX,
                rarity::K,
                &External::default(),
            )
        };
        assert_eq!(est(&c), None);
        // com menos de 8 vendas acima, o prêmio de baixo continua valendo
        assert!(est(&comps(&r[..12])).is_some());
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
            let seeds: BTreeSet<u16> = m.keys().map(|k| k.0).collect();
            let mut mid: Vec<f64> = m.values().map(|e| e[e.len() / 2]).collect();
            mid.sort_by(f64::total_cmp);
            println!(
                "piso US$ {:>7.2} | {base}: {} seeds ({} com desgaste), prova mediana {:.2}x ({:.2}x a {:.2}x)",
                floor.get(base).copied().unwrap_or(0) as f64 / 100.0,
                seeds.len(),
                m.len(),
                mid[mid.len() / 2],
                mid[0],
                mid[mid.len() - 1]
            );
        }
        println!("total (skins, seeds): {:?}", size(&l));
        let (n, paid, ratio) = crate::hunt::value::backtest_summary(&backtest(&comps));
        println!("prova fora da amostra: {n} vendas, pagaram {paid:?}, real/previsto {ratio:?}");
        let rows = backtest_rows(&comps, &External::default());
        let below = rows.iter().filter(|(t, real)| real < &t[0]).count();
        let cheap = rows
            .iter()
            .filter(|(t, real)| real < &1.0 && t[1] > 1.0)
            .count();
        println!(
            "  real abaixo do limite inferior: {below} de {} ({:.1} %); real < 1× com esperado > 1×: {cheap}",
            rows.len(),
            below as f64 * 100.0 / rows.len().max(1) as f64
        );
        // SIGMA: variância do log da razão dentro do mesmo seed, mediana entre skins com repetição
        let mut sigmas: Vec<f64> = comps
            .values()
            .filter_map(|v| {
                let mut by: BTreeMap<u16, Vec<f64>> = BTreeMap::new();
                for c in v {
                    by.entry(c.seed).or_default().push(c.ratio.ln());
                }
                let (mut ss, mut df) = (0.0, 0);
                for g in by.values().filter(|g| g.len() >= 2) {
                    let m = g.iter().sum::<f64>() / g.len() as f64;
                    ss += g.iter().map(|x| (x - m).powi(2)).sum::<f64>();
                    df += g.len() - 1;
                }
                (df >= 10).then(|| (ss / df as f64).sqrt())
            })
            .collect();
        sigmas.sort_by(f64::total_cmp);
        println!(
            "SIGMA dentro do seed: {} skins, mediana {:?}",
            sigmas.len(),
            sigmas.get(sigmas.len() / 2)
        );
        let ks: Vec<f64> = comps.values().map(|v| skin_k(v)).collect();
        println!(
            "skins com efeito de seed significativo (k < {}): {} de {}",
            crate::pricing::rarity::K,
            ks.iter()
                .filter(|k| **k < crate::pricing::rarity::K)
                .count(),
            ks.len()
        );
        let ks: HashMap<String, f64> = comps.iter().map(|(b, v)| (b.clone(), skin_k(v))).collect();
        let ext = External::default();
        replay(&c, "catálogo de hoje", &|n, s, w, _| {
            l.get(skin_base(n)).and_then(|m| m.get(&(s, w))).cloned()
        });
        replay(
            &c,
            "só com vendas anteriores ao anúncio",
            &|n, s, w, at| {
                let b = skin_base(n);
                Some(estimate_for(b, s, w, comps.get(b)?, at, ks[b], &ext)?.to_vec())
            },
        );
    }

    /// Prêmio de (nome, seed, desgaste) num momento, para a repetição.
    type Premiums<'a> = dyn Fn(&str, u16, u8, i64) -> Option<Vec<f64>> + 'a;

    /// Anúncios já observados (`observations`, com seed) que a regra aprovaria: preço comum = estimativa de venda do
    /// nome com o histórico dos 7 dias anteriores à observação, prêmio = `get(nome, seed, desgaste, quando)`.
    /// Sem concorrente, chaveiro nem correção: mede a regra, não o aviso inteiro.
    fn replay(c: &rusqlite::Connection, label: &str, get: &Premiums) {
        use crate::pricing::sell::{self, PricePoint, RareEvidence};
        let listings: Vec<(String, u16, i64, i64)> = c
            .prepare("SELECT name, seed, price_cents, first_seen FROM observations WHERE seed IS NOT NULL")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .flatten()
            .collect();
        let (mut seen, mut cands, mut ok, mut ok_cheap) = (0, 0, 0, 0);
        // por quê o resto não passa: sem estimativa de preço, sem lucro no médio, lucro só no esperado e perda
        // pequena como comum (compra de exploração), lucro só no esperado e perda grande
        let (mut no_est, mut no_mid, mut explore, mut too_lossy, mut explore_cheap) =
            (0, 0, 0, 0, 0);
        let (mut mid_positive, mut mid_positive_safe) = (0, 0);
        let mut by_skin: BTreeMap<String, u32> = BTreeMap::new();
        for (name, seed, price, at) in &listings {
            seen += 1;
            let Some(e) = get(name, *seed, wear_index(name), *at) else {
                continue;
            };
            cands += 1;
            let points: Vec<PricePoint> = c
                .prepare("SELECT ts, median_cents, purchases FROM price_points WHERE name = ?1 AND ts > ?2 AND ts <= ?3")
                .unwrap()
                .query_map(rusqlite::params![name, at - 7 * 86_400, at], |r| {
                    Ok(PricePoint { ts: r.get(0)?, median_cents: r.get(1)?, purchases: r.get(2)? })
                })
                .unwrap()
                .flatten()
                .collect();
            let Ok(est) = sell::estimate(
                &points,
                *at,
                None,
                Some(RareEvidence {
                    multiples: e,
                    posterior: true,
                }),
            ) else {
                no_est += 1;
                continue;
            };
            let ev = sell::evaluate(*price, &est, 0);
            if ev.listable {
                ok += 1;
                ok_cheap += u32::from(*price <= 3600);
                *by_skin.entry(skin_base(name).to_string()).or_default() += 1;
            } else if !ev.mid_ok {
                no_mid += 1;
                if ev.sell.mid.profit_cents > 0 {
                    mid_positive += 1;
                    if ev.sell.quick.profit_cents >= -50 {
                        mid_positive_safe += 1;
                    }
                }
            } else if ev.sell.quick.profit_cents >= -50 {
                explore += 1;
                explore_cheap += u32::from(*price <= 3600);
                *by_skin.entry(skin_base(name).to_string()).or_default() += 1;
            } else {
                too_lossy += 1;
            }
        }
        let real: i64 = c
            .query_row(
                "SELECT count(DISTINCT listing_id) FROM evaluations WHERE reason = 'aprovada'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!(
            "[{label}] anúncios observados com seed: {seen}; de seed candidato: {cands}; aprovados pela regra: {ok} (até R$ 36: {ok_cheap}); aprovados de verdade (evaluations): {real}"
        );
        println!(
            "  dos candidatos: sem estimativa de preço {no_est}, sem lucro no médio {no_mid}, só exploração (perda como comum ≤ R$ 0,50) {explore} (até R$ 36: {explore_cheap}), perda grande como comum {too_lossy}; entre os sem lucro no médio, com lucro > 0 mesmo assim: {mid_positive} (perda ≤ R$ 0,50 como comum: {mid_positive_safe})"
        );
        println!("aprovados por skin: {by_skin:?}");
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
