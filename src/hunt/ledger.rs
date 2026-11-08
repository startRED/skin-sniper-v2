//! Registro de observações (mina de ouro, Fase 0): todo anúncio que uma consulta mostra, com o item (`asset_id`),
//! quando foi visto pela primeira e pela última vez, a idade máxima ao ser visto e quando sumiu. É a matéria-prima
//! do professor (`teacher`) e da agenda de colheita (`harvest`). Docs/projeto.md, "Aprendizado".
//!
//! Sumiço só com página não vazia e preço coberto por ela; o mesmo `asset_id` em outro anúncio em até 24 h é
//! relistagem, não venda.

use super::Hunt;
use super::scheduler::Task;
use crate::pricing::groups::group_of;
use crate::steam::listings::ListingsPage;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};

/// Item que volta em outro anúncio dentro disso é relistagem do vendedor.
pub(crate) const RELIST_WINDOW: i64 = 24 * 3600;

/// Uma chave por consulta exata (código, desgaste, filtro): só a mesma consulta prova que um anúncio sumiu.
pub fn query_key(t: &Task) -> String {
    format!("{}|{:?}|{:?}", t.code, t.wear, t.filters())
}

/// A página cobre o preço: sem `more`, todos; com `more`, abaixo de 98 % do maior preço dela (a Steam ordena
/// por outro preço-base e inverte ~1 % na borda).
fn covered(cover: Option<i64>, price: i64) -> bool {
    cover.is_none_or(|max| price * 100 < max * 98)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    pub new: u32,
    /// Novos com idade máxima conhecida (a consulta anterior da mesma chave cobria o preço).
    pub aged: u32,
    pub gone: u32,
    pub relisted: u32,
}

struct Row {
    id: String,
    asset: String,
    name: String,
    grp: Option<String>,
    price: i64,
    seed: Option<i64>,
    float: Option<f64>,
    paint: Option<i64>,
    cheapest: bool,
}

/// Grava a página no registro. Chamado uma vez por página, antes da detecção.
pub async fn record(h: &Hunt, key: &str, page: &ListingsPage, now: i64) -> Result<Recorded> {
    {
        let mut s = h.lock();
        s.cycle.pages += 1;
        s.cycle
            .seen
            .extend(page.listings.iter().map(|l| l.listing_id.clone()));
    }
    if page.listings.is_empty() {
        // página vazia pode ser transitória: não prova sumiço nem serve de cobertura para a idade
        return Ok(Recorded::default());
    }
    let cover = page
        .more
        .then(|| page.listings.iter().map(|l| l.price_cents).max())
        .flatten();
    let mut cheapest: HashMap<&str, i64> = HashMap::new();
    for l in &page.listings {
        let p = cheapest.entry(l.name.as_str()).or_insert(l.price_cents);
        *p = (*p).min(l.price_cents);
    }
    let (prev, rows) = {
        let mut s = h.lock();
        let prev = s.ledger_prev.insert(key.to_string(), (now, cover));
        let rows: Vec<Row> = page
            .listings
            .iter()
            .map(|l| Row {
                id: l.listing_id.clone(),
                asset: l.asset_id.clone(),
                name: l.name.clone(),
                grp: group_of(&l.name, l.seed.map(u32::from), l.paint_index, l.float)
                    .or_else(|| super::patterns::group(&s.learned, &l.name, l.seed).map(|(g, _)| g))
                    .map(|g| g.key),
                price: l.price_cents,
                seed: l.seed.map(i64::from),
                float: l.float,
                paint: l.paint_index.map(i64::from),
                cheapest: cheapest.get(l.name.as_str()) == Some(&l.price_cents),
            })
            .collect();
        (prev, rows)
    };
    let key = key.to_string();
    let recorded = h.db.call(move |c| {
        let tx = c.transaction()?;
        let mut out = Recorded::default();
        let present: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
        for r in &rows {
            let known = tx
                .query_row(
                    "SELECT 1 FROM observations WHERE listing_id = ?1",
                    [&r.id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if known {
                // voltou depois de dado como sumido (inversão de ordem): está vivo
                tx.execute(
                    "UPDATE observations SET last_seen = ?2, gone_at = NULL WHERE listing_id = ?1",
                    params![r.id, now],
                )?;
                continue;
            }
            let mut relist_of = None;
            if !r.asset.is_empty() {
                relist_of = tx
                    .query_row(
                        "SELECT listing_id FROM observations WHERE asset_id = ?1 AND listing_id <> ?2
                         AND (gone_at IS NULL OR gone_at >= ?3) ORDER BY first_seen DESC LIMIT 1",
                        params![r.asset, r.id, now - RELIST_WINDOW],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if let Some(old) = &relist_of {
                    tx.execute(
                        "UPDATE observations SET relisted = 1, gone_at = COALESCE(gone_at, ?2) WHERE listing_id = ?1",
                        params![old, now],
                    )?;
                    out.relisted += 1;
                }
            }
            let age_max = prev
                .filter(|&(_, cover)| covered(cover, r.price))
                .map(|(at, _)| now - at);
            tx.execute(
                "INSERT INTO observations(listing_id, asset_id, name, query, grp, price_cents, seed, \"float\",
                    paint_index, first_seen, last_seen, age_max, cheapest, relist_of)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12, ?13)",
                params![
                    r.id, r.asset, r.name, key, r.grp, r.price, r.seed, r.float, r.paint, now, age_max,
                    r.cheapest, relist_of
                ],
            )?;
            out.new += 1;
            out.aged += u32::from(age_max.is_some());
        }
        let alive: Vec<(String, i64)> = tx
            .prepare(
                "SELECT listing_id, price_cents FROM observations
                 WHERE query = ?1 AND gone_at IS NULL AND last_seen < ?2",
            )?
            .query_map(params![key, now], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, price) in alive {
            if !present.contains(&id) && covered(cover, price) {
                tx.execute(
                    "UPDATE observations SET gone_at = ?2 WHERE listing_id = ?1",
                    params![id, now],
                )?;
                out.gone += 1;
            }
        }
        tx.commit()?;
        Ok(out)
    })
    .await?;
    h.lock().cycle.new += recorded.new;
    Ok(recorded)
}

/// Soma contadores da hora no placar (`hourly_activity`). Colunas fixas do código, nunca de fora.
pub(crate) async fn count_hour(h: &Hunt, now: i64, col: &'static str, n: u32) {
    if n == 0 {
        return;
    }
    let sql = format!(
        "INSERT INTO hourly_activity(hour_utc, {col}) VALUES (?1, ?2)
         ON CONFLICT(hour_utc) DO UPDATE SET {col} = {col} + excluded.{col}"
    );
    if let Err(error) =
        h.db.call(move |c| c.execute(&sql, params![now.div_euclid(3600), n]))
            .await
    {
        tracing::warn!(%error, "placar: falha ao somar {col}");
    }
}
