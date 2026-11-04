//! O que a caça está vigiando agora (`GET /api/watch`): menor anúncio visto, o preço máximo que ainda daria
//! lucro e o ritmo de vendas. Serve para o usuário ver que a caça presta mesmo com a lista vazia.

use super::Hunt;
use crate::pricing::fees::seller_gets;
use crate::pricing::groups::group_of;
use crate::pricing::sell::{self, MIN_LIST_MARGIN_PCT, MIN_LIST_PROFIT_CENTS, PricePoint};
use crate::web::api::{Kind, WatchRow};
use anyhow::Result;
use rusqlite::OptionalExtension;

const DAY: i64 = 86_400;

/// Maior preço de compra que ainda entra na lista (lucro ≥ R$ 0,20 e margem ≥ 5 % no preço médio).
fn max_buy(net_mid: i64) -> i64 {
    let by_margin = (net_mid as f64 * 100.0 / (100.0 + MIN_LIST_MARGIN_PCT)).floor() as i64;
    by_margin.min(net_mid - MIN_LIST_PROFIT_CENTS)
}

/// (nome completo no catálogo, ícone, vendas/dia, pontos dos últimos 15 dias) do item de uma tarefa.
type Facts = (String, String, f64, Vec<PricePoint>);

pub async fn list(h: &Hunt) -> Result<Vec<WatchRow>> {
    let now = h.now();
    let (tasks, floors) = {
        let s = h.lock();
        (s.queue.tasks(), s.floors.clone())
    };
    let names: Vec<String> = tasks.iter().map(|t| t.name.clone()).collect();
    let facts: Vec<Option<Facts>> =
        h.db.call(move |c| {
            let mut out = vec![];
            for n in names {
                let item = c
                    .query_row(
                        "SELECT name, COALESCE(icon_url, ''), COALESCE(sales_per_day, 0) FROM items
                         WHERE name = ?1 OR name LIKE ?1 || ' (%' ORDER BY name = ?1 DESC LIMIT 1",
                        [&n],
                        |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                let Some((full, icon, spd)) = item else {
                    out.push(None);
                    continue;
                };
                let pts = c
                    .prepare(
                        "SELECT ts, median_cents, purchases FROM price_points
                         WHERE name = ?1 AND ts >= ?2 ORDER BY ts",
                    )?
                    .query_map(rusqlite::params![full, now - 15 * DAY], |r| {
                        Ok(PricePoint {
                            ts: r.get(0)?,
                            median_cents: r.get(1)?,
                            purchases: r.get(2)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                out.push(Some((full, icon, spd, pts)));
            }
            Ok(out)
        })
        .await?;

    let mut rows: Vec<WatchRow> = tasks
        .iter()
        .zip(facts)
        .map(|(t, f)| {
            let (floor, at) = match floors.get(&t.id) {
                Some((fl, at)) => (*fl, Some(*at)),
                None => (None, None),
            };
            let rare = t.seeds.is_some();
            let group = t
                .seeds
                .and_then(|(lo, _)| group_of(&t.name, Some(u32::from(lo)), None))
                .map(|g| g.label);
            let (icon, spd, pts) = match f {
                Some((_, i, s, p)) => (i, s, p),
                None => (String::new(), 0.0, vec![]),
            };
            // Raro: o preço depende do grupo e da evidência do CSFloat; sem isso não há teto honesto.
            let max = (!rare)
                .then(|| sell::estimate(&pts, now, floor, None).ok())
                .flatten()
                .map(|e| max_buy(seller_gets(e.mid.price_cents)));
            WatchRow {
                name: t.name.clone(),
                icon_url: icon,
                floor_cents: floor,
                max_buy_cents: max,
                sales_per_day: spd,
                kind: if rare { Kind::Raro } else { Kind::Desconto },
                group,
                last_checked_at: at,
            }
        })
        .collect();
    // mais perto de compensar primeiro (piso − teto menor = mais perto); sem os dois números, por último
    rows.sort_by(|a, b| {
        let gap = |r: &WatchRow| r.floor_cents.zip(r.max_buy_cents).map(|(f, m)| f - m);
        gap(a)
            .unwrap_or(i64::MAX)
            .cmp(&gap(b).unwrap_or(i64::MAX))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(rows)
}
