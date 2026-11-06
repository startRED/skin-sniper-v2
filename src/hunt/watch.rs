//! O que a caça está vigiando agora (`GET /api/watch`): menor anúncio visto, o preço máximo que ainda daria
//! lucro e o ritmo de vendas. Serve para o usuário ver que a caça presta mesmo com a lista vazia.

use super::{Hunt, catalog};
use crate::pricing::fees::seller_gets;
use crate::pricing::groups::group_of;
use crate::pricing::sell::{
    self, MIN_LIST_MARGIN_PCT, MIN_LIST_PROFIT_CENTS, PricePoint, RARE_MIN_SALES, RareEvidence,
};
use crate::web::api::{Kind, WatchRow};
use anyhow::Result;
use rusqlite::OptionalExtension;

const DAY: i64 = 86_400;

/// Maior preço de compra que ainda entra na lista (lucro ≥ R$ 1,00 e margem ≥ 10 % no preço médio).
fn max_buy(net_mid: i64) -> i64 {
    let by_margin = (net_mid as f64 * 100.0 / (100.0 + MIN_LIST_MARGIN_PCT)).floor() as i64;
    by_margin.min(net_mid - MIN_LIST_PROFIT_CENTS)
}

/// (nome completo no catálogo, ícone, vendas/dia, pontos dos últimos 15 dias) do item de uma tarefa.
type Facts = (String, String, f64, Vec<PricePoint>);

pub async fn list(h: &Hunt) -> Result<Vec<WatchRow>> {
    rows(h, true).await
}

/// Teto para os filtros que olham o menor anúncio antes de ler a página (busca e livro): ali o menor anúncio
/// do livro é o próprio candidato, e como concorrente ele derrubaria o teto abaixo de si mesmo.
pub async fn list_for_scan(h: &Hunt) -> Result<Vec<WatchRow>> {
    rows(h, false).await
}

async fn rows(h: &Hunt, book_rival: bool) -> Result<Vec<WatchRow>> {
    let now = h.now();
    let (tasks, floors, learned) = {
        let s = h.lock();
        let tasks = s.queue.tasks();
        let learned: Vec<(f64, String)> =
            tasks.iter().map(|t| s.queue.describe(&t.id, now)).collect();
        (tasks, s.floors.clone(), learned)
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

    let (correction, books, saldo) = {
        let s = h.lock();
        (s.correction, s.books.clone(), s.saldo)
    };
    let mut rows: Vec<WatchRow> = tasks
        .iter()
        .zip(facts)
        .zip(learned)
        .map(|((t, f), (score, reason))| {
            let seen = catalog::observed(&floors, &t.id, now);
            let (floor, at) = (seen.floor, seen.at);
            let rare = t.rare();
            let group = t
                .seeds
                .first()
                .and_then(|&(lo, _)| group_of(&t.name, Some(u32::from(lo)), None, None))
                .or_else(|| {
                    t.float
                        .and_then(|(lo, _)| group_of(&t.name, None, None, Some(lo)))
                })
                .or_else(|| {
                    let seed = t.seeds.first().map(|r| r.0);
                    super::patterns::group(&h.lock().learned, &t.name, seed).map(|(g, _)| g)
                });
            let (icon, spd, mut pts) = match f {
                Some((_, i, s, p)) => (i, s, p),
                None => (String::new(), 0.0, vec![]),
            };
            sell::discount(&mut pts, correction[usize::from(rare)]);
            // Raro: o teto vem do prêmio do grupo (vendas do CSFloat já em cache); com menos de
            // `RARE_MIN_SALES` vendas não há teto honesto.
            let evidence = if rare {
                let multiples = group
                    .as_ref()
                    .map_or(vec![], |g| rare_multiples(h, &t.name, &g.key));
                (multiples.len() >= RARE_MIN_SALES).then_some(RareEvidence { multiples })
            } else {
                None
            };
            // O piso entra como concorrente de propósito: o teto vale para um anúncio NOVO abaixo dele, que
            // disputa a venda com o piso. O próprio anúncio do piso já foi julgado pelo `detect` contra o
            // segundo mais barato da página. O menor anúncio do livro (≤ 1 h) também concorre.
            let book = books.get(&t.name).filter(|_| !rare);
            let fresh = book
                .filter(|(at, _)| now - at <= super::orders::STALE)
                .and_then(|(_, b)| b.as_ref());
            let rival = floor
                .into_iter()
                .chain(fresh.filter(|_| book_rival).and_then(|b| b.min_sell_cents))
                .min()
                .filter(|_| !rare);
            let est = (!rare || evidence.is_some())
                .then(|| sell::estimate(&pts, now, rival, evidence).ok())
                .flatten();
            let max = est
                .as_ref()
                .map(|e| max_buy(seller_gets(e.mid.price_cents)))
                .filter(|m| *m > 0); // sem preço de compra que dê lucro: sem teto
            let bid = fresh
                .and_then(|b| b.max_buy_cents)
                .map(|top| top + 1)
                .filter(|&p| p <= saldo && rival.is_none_or(|r| p < r))
                .zip(est.as_ref())
                .map(|(p, e)| (p, sell::evaluate(p, e, 0)))
                .filter(|(_, ev)| ev.listable && ev.ev_profit_cents.is_some_and(|x| x > 0));
            WatchRow {
                name: t.name.clone(),
                icon_url: icon,
                floor_cents: floor,
                max_buy_cents: max,
                sales_per_day: spd,
                kind: if rare { Kind::Raro } else { Kind::Desconto },
                group: group.map(|g| g.label),
                last_checked_at: at,
                mode: t.mode().label().to_string(),
                score,
                reason,
                bid_cents: bid.as_ref().map(|(p, _)| *p),
                bid_profit_cents: bid.map(|(_, ev)| ev.sell.mid.profit_cents),
                book_at: book.map(|(at, _)| *at),
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

/// Múltiplos (preço ÷ base) das vendas do CSFloat do grupo que já estão em cache; não consulta a rede.
fn rare_multiples(h: &Hunt, name: &str, group_key: &str) -> Vec<f64> {
    let s = h.lock();
    s.csfloat_cache.get(name).map_or(vec![], |(_, sales)| {
        sales
            .iter()
            .filter(|x| !x.has_stickers)
            .filter(|x| {
                group_of(name, x.seed.map(u32::from), x.paint_index, x.float)
                    .is_some_and(|g| g.key == group_key)
            })
            .map(|x| x.multiple)
            .collect()
    })
}
