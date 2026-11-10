//! Patterns aprendidos: as tabelas embutidas só têm 4 skins de pattern (mais blue gem e fade), e nas vendas do
//! CSFloat de 06/11 apareceram seeds caros fora delas (MP7 Amberline 890 duas vezes a 5–10×, M4A4 Etch Lord 158 a
//! 18×). Um coletor lê as vendas de uma skin do catálogo a cada `GAP` e guarda as vendas sem adesivo, fora das
//! tabelas e do float de elite, que saíram a `MIN_RATIO` × a mediana da skin ou mais. Um seed vira alvo e grupo
//! raro quando junta `RARE_MIN_SALES` vendas assim (somando os desgastes): a mesma prova que o T1 exige. As razões
//! guardadas são o prêmio do grupo. Os limites (2×, 60 dias) são hipóteses, não calibração.

use super::Hunt;
use super::catalog::{self, Item};
use crate::csfloat::CsfloatSale;
use crate::pricing::groups::{Group, group_of, skin_base};
use crate::pricing::sell::{MAX_RARE_MULTIPLE, RARE_MIN_SALES};
use anyhow::Result;
use rusqlite::params;
use std::collections::{BTreeMap, HashMap};

/// shortcut: uma skin a cada 2 min (~720 por dia, dentro da cota do CSFloat); subir se a cota sobrar.
const GAP: i64 = 120;
// shortcut: aquece os nomes vigiados a cada 15 s; reavaliar após medir cota e latência em produção.
const WARM_GAP: i64 = 15;
/// A mesma skin só é relida depois disso.
const REFETCH: i64 = 24 * 3600;
/// Venda a esta razão da mediana da skin ou mais conta como prêmio de pattern.
pub const MIN_RATIO: f64 = 2.0;
/// Menos vendas limpas que isso não dão uma mediana confiável.
const MIN_CLEAN_SALES: usize = 10;
/// Skins com menos vendas por dia quase não têm anúncio novo para caçar.
const MIN_SALES_PER_DAY: f64 = 5.0;

/// Skin-base → seed → razões (preço ÷ mediana da skin) das vendas que ensinaram.
pub type Learned = HashMap<String, BTreeMap<u16, Vec<f64>>>;

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

/// Vendas da skin que pagaram prêmio de pattern ainda desconhecido: `(seed, razão, sold_at)`.
pub fn premium_sales(name: &str, sales: &[CsfloatSale]) -> Vec<(u16, f64, i64)> {
    let clean: Vec<&CsfloatSale> = sales
        .iter()
        .filter(|s| !s.has_stickers && s.multiple.is_finite() && s.multiple > 0.0)
        .collect();
    if clean.len() < MIN_CLEAN_SALES {
        return vec![];
    }
    let mut m: Vec<f64> = clean.iter().map(|s| s.multiple).collect();
    m.sort_by(f64::total_cmp);
    let median = m[m.len() / 2];
    clean
        .iter()
        .filter_map(|s| {
            let seed = s.seed?;
            // tabela e float de elite já têm grupo próprio
            if group_of(name, Some(seed.into()), s.paint_index, s.float)
                .is_some_and(|g| !g.key.starts_with("learned:"))
            {
                return None;
            }
            let r = s.multiple / median;
            (MIN_RATIO..=MAX_RARE_MULTIPLE)
                .contains(&r)
                .then_some((seed, r, s.sold_at))
        })
        .collect()
}

/// Seeds aprendidos com prova suficiente para a skin.
pub fn active(l: &Learned, base: &str) -> Vec<u16> {
    l.get(base).map_or(vec![], |m| {
        m.iter()
            .filter(|(_, v)| v.len() >= RARE_MIN_SALES)
            .map(|(s, _)| *s)
            .collect()
    })
}

/// Grupo aprendido do anúncio e o prêmio dele (as razões guardadas).
pub fn group(l: &Learned, name: &str, seed: Option<u16>) -> Option<(Group, Vec<f64>)> {
    let base = skin_base(name);
    let seed = seed?;
    let v = l.get(base)?.get(&seed)?;
    (v.len() >= RARE_MIN_SALES).then(|| {
        (
            Group {
                key: format!("learned:{base}:{seed}"),
                label: format!("Pattern #{seed}"),
            },
            v.clone(),
        )
    })
}

/// Venda limpa guardada: `(nome, seed, múltiplo, grupo)`, em ordem de nome e múltiplo.
type Sale = (String, Option<u16>, f64, Option<String>);

/// Prova contra: o seed já provado que também vendeu perto do preço comum (menos de `MIN_RATIO` × a mediana da
/// skin) leva essas vendas no prêmio; sem elas o prêmio só enxerga as vendas caras. Venda comum não completa prova:
/// só entra em seed que já tem `RARE_MIN_SALES` vendas caras.
fn counter(l: &mut Learned, sales: &[Sale]) {
    for of_name in sales.chunk_by(|a, b| a.0 == b.0) {
        let Some(seeds) = l.get_mut(skin_base(&of_name[0].0)) else {
            continue;
        };
        if of_name.len() < MIN_CLEAN_SALES {
            continue;
        }
        let median = of_name[of_name.len() / 2].2;
        for (_, seed, multiple, grp) in of_name {
            // tabela e float de elite têm grupo próprio
            if multiple / median < MIN_RATIO
                && grp.as_deref().is_none_or(|g| g.starts_with("learned:"))
                && let Some(v) = seed.and_then(|s| seeds.get_mut(&s))
                && v.len() >= RARE_MIN_SALES
            {
                v.push(multiple / median);
            }
        }
    }
}

pub async fn load(h: &Hunt) -> Result<()> {
    let (rows, sales): (Vec<(String, u16, f64)>, Vec<Sale>) =
        h.db.call(|c| {
            let rows = c
                .prepare("SELECT name, seed, ratio FROM learned_sales")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let sales = c
                .prepare(
                    "SELECT name, seed, multiple, grp FROM csfloat_sales ORDER BY name, multiple",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok((rows, sales))
        })
        .await?;
    let mut l = Learned::new();
    for (name, seed, ratio) in rows {
        l.entry(skin_base(&name).to_string())
            .or_default()
            .entry(seed)
            .or_default()
            .push(ratio);
    }
    counter(&mut l, &sales);
    h.lock().learned = l;
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
        Ok(items) => candidates(items, saldo, ratio, now),
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
    let found = premium_sales(&name, &sales);
    if found.is_empty() {
        return;
    }
    let before = active(&h.lock().learned, skin_base(&name)).len();
    let n = name.clone();
    if let Err(e) = h
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            for (seed, ratio, at) in found {
                tx.execute(
                    "INSERT OR IGNORE INTO learned_sales(name, seed, ratio, sold_at) VALUES (?1, ?2, ?3, ?4)",
                    params![n, seed, ratio, at],
                )?;
            }
            tx.commit()
        })
        .await
    {
        return tracing::warn!("patterns: gravar falhou: {e}");
    }
    if let Err(e) = load(h).await {
        return tracing::warn!("patterns: reler falhou: {e}");
    }
    let now_active = active(&h.lock().learned, skin_base(&name));
    if now_active.len() > before {
        tracing::info!(skin = skin_base(&name), seeds = ?now_active, "pattern aprendido");
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

    fn sale(seed: u16, multiple: f64, stickers: bool) -> CsfloatSale {
        CsfloatSale {
            price_usd_cents: (multiple * 100.0) as i64,
            base_price_usd_cents: 100,
            multiple,
            seed: Some(seed),
            float: Some(0.3),
            paint_index: None,
            sold_at: 1_793_000_000 + i64::from(seed),
            has_stickers: stickers,
            sticker_overpay_cents: 0,
        }
    }

    #[test]
    fn premium_sales_pick_unknown_seeds_far_above_the_median() {
        const MP7: &str = "MP7 | Amberline (Minimal Wear)";
        let mut v: Vec<CsfloatSale> = (1..=12).map(|i| sale(i, 1.0, false)).collect();
        v.push(sale(890, 9.75, false));
        v.push(sale(891, 1.9, false)); // abaixo de 2×
        v.push(sale(892, 8.0, true)); // adesivo: o prêmio pode ser dele
        v.push(sale(893, 500.0, false)); // dado ruim
        let got = premium_sales(MP7, &v);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0, got[0].1), (890, 9.75));
        // amostra pequena: sem mediana confiável
        assert!(premium_sales(MP7, &v[..5]).is_empty());
        // seed de tabela (T1 do Moonrise) já tem grupo: não é aprendido
        let mut m: Vec<CsfloatSale> = (1..=12).map(|i| sale(i, 1.0, false)).collect();
        m.push(sale(59, 5.0, false));
        assert!(premium_sales("Glock-18 | Moonrise (Field-Tested)", &m).is_empty());
    }

    #[test]
    fn seed_needs_three_sales_across_wears_to_become_a_group() {
        let mut l = Learned::new();
        l.entry("MP7 | Amberline".into())
            .or_default()
            .insert(890, vec![9.75, 5.48]);
        let mw = "MP7 | Amberline (Minimal Wear)";
        assert!(active(&l, "MP7 | Amberline").is_empty());
        assert_eq!(group(&l, mw, Some(890)), None);
        l.get_mut("MP7 | Amberline")
            .unwrap()
            .get_mut(&890)
            .unwrap()
            .push(3.0);
        assert_eq!(active(&l, "MP7 | Amberline"), vec![890]);
        // vale para o StatTrak e outro desgaste da mesma skin
        let (g, prem) = group(&l, "StatTrak™ MP7 | Amberline (Field-Tested)", Some(890)).unwrap();
        assert_eq!(
            (g.key.as_str(), g.label.as_str(), prem.len()),
            ("learned:MP7 | Amberline:890", "Pattern #890", 3)
        );
        assert_eq!(group(&l, mw, Some(1)), None);
        assert_eq!(group(&l, mw, None), None);
    }

    #[test]
    fn researched_seed_keeps_learning_with_the_existing_group_identity() {
        let name = "Galil AR | Sandstorm (Field-Tested)";
        let mut sales: Vec<_> = (1..=12).map(|i| sale(i, 1.0, false)).collect();
        sales.push(sale(583, 5.0, false));
        assert_eq!(premium_sales(name, &sales)[0].0, 583);
        let learned = HashMap::from([(
            "Galil AR | Sandstorm".into(),
            BTreeMap::from([(583, vec![4.0, 5.0, 6.0])]),
        )]);
        let (g, evidence) = group(&learned, name, Some(583)).unwrap();
        assert_eq!(
            g.key,
            group_of(name, Some(583), None, Some(0.3)).unwrap().key
        );
        assert_eq!(evidence.len(), 3);
    }

    #[test]
    fn a_proven_seed_also_carries_its_ordinary_sales() {
        const NAME: &str = "MP7 | Amberline (Minimal Wear)";
        let mut l = Learned::new();
        let seeds = l.entry("MP7 | Amberline".into()).or_default();
        seeds.insert(7, vec![2.0, 2.5, 3.0]);
        seeds.insert(8, vec![2.0, 2.5]);
        let row = |seed, multiple, grp: Option<&str>| -> Sale {
            (NAME.into(), Some(seed), multiple, grp.map(String::from))
        };
        let mut sales: Vec<Sale> = (0..8).map(|_| row(100, 1.0, None)).collect();
        sales.extend([
            row(7, 1.0, None),
            row(7, 1.2, Some("learned:MP7 | Amberline:7")),
            row(7, 1.1, Some("float:elite")), // outro grupo
            row(7, 3.0, None),                // cara: já está em `learned_sales`
            row(8, 1.0, None),                // seed sem prova: fica como está
        ]);
        sales.sort_by(|a, b| a.2.total_cmp(&b.2));
        counter(&mut l, &sales);
        assert_eq!(l["MP7 | Amberline"][&7], vec![2.0, 2.5, 3.0, 1.0, 1.2]);
        assert_eq!(l["MP7 | Amberline"][&8], vec![2.0, 2.5]);
        // menos de `MIN_CLEAN_SALES` vendas não dão mediana: nada muda
        counter(&mut l, &sales[..5]);
        assert_eq!(l["MP7 | Amberline"][&7].len(), 5);
    }
}
