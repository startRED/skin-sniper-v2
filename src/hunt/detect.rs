//! Detecção: anúncio novo (via `listings_seen`) com preço ≤ saldo → estimativa → avaliação → `Opportunity`
//! no banco e evento SSE; anúncio que sumiu numa nova consulta da mesma tarefa → `gone`.
//! Docs/projeto.md, "Caça" 4 e "Precificação".

use super::catalog::{HISTORY_TTL, wear_label, window_stats};
use super::scheduler::Task;
use super::{Active, Hunt};
use crate::pricing::groups::group_of;
use crate::pricing::sell::{self, Confidence as SellConfidence, PricePoint, RareEvidence};
use crate::steam::history;
use crate::steam::listings::{Listing, ListingsPage};
use crate::web::api::{Confidence, Event, Kind, Opportunity, Sell, Tier};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

const DAY: i64 = 86_400;
/// Uma página vazia pode ser transitória (conhecimento 1.3): só vale como "sumiram" na segunda seguida.
const EMPTY_PAGES_TO_TRUST: u8 = 2;

#[derive(Debug, Default)]
pub struct Outcome {
    /// Oportunidades novas e se vale avisar (`notify` da avaliação).
    pub found: Vec<(Opportunity, bool)>,
    pub gone: Vec<String>,
}

/// Baixa `QueryPriceHistory` do nome e grava em `price_points` e em `items` (vendas/dia, dispersão).
pub async fn fetch_and_store_history(h: &Hunt, name: &str) -> Result<()> {
    let pts = history::fetch_history(&h.steam, name).await?;
    let now = h.now();
    let sell_pts: Vec<PricePoint> = pts.iter().map(to_sell_point).collect();
    let (spd, disp) = window_stats(&sell_pts, now).unwrap_or((0.0, 0.0));
    let name = name.to_string();
    h.db.call(move |c| {
        let tx = c.transaction()?;
        {
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO price_points(name, ts, median_cents, purchases) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for p in &pts {
                ins.execute(params![name, p.ts, p.median_cents, p.purchases])?;
            }
        }
        tx.execute(
            "INSERT INTO items(name, updated_at, history_at, sales_per_day, dispersion) VALUES (?1, ?2, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET history_at = excluded.history_at,
                sales_per_day = excluded.sales_per_day, dispersion = excluded.dispersion",
            params![name, now, spd, disp],
        )?;
        tx.commit()
    })
    .await
}

fn to_sell_point(p: &history::PricePoint) -> PricePoint {
    PricePoint {
        ts: p.ts,
        median_cents: p.median_cents,
        purchases: p.purchases,
    }
}

/// Pontos dos últimos 15 dias do nome (a tendência usa até 14); baixa o histórico se passou de 6 h.
async fn points_for(h: &Hunt, name: &str, now: i64) -> Result<Vec<PricePoint>> {
    let n = name.to_string();
    let last: Option<i64> =
        h.db.call(move |c| {
            c.query_row("SELECT history_at FROM items WHERE name = ?1", [n], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()
            .map(Option::flatten)
        })
        .await?;
    if last.is_none_or(|t| now - t >= HISTORY_TTL) {
        fetch_and_store_history(h, name).await?;
        h.lock().queue.spend_discovery();
    }
    let n = name.to_string();
    h.db.call(move |c| {
        c.prepare(
            "SELECT ts, median_cents, purchases FROM price_points WHERE name = ?1 AND ts >= ?2 ORDER BY ts",
        )?
        .query_map(params![n, now - 15 * DAY], |r| {
            Ok(PricePoint {
                ts: r.get(0)?,
                median_cents: r.get(1)?,
                purchases: r.get(2)?,
            })
        })?
        .collect()
    })
    .await
}

/// Múltiplos `price / base_price` das vendas do CSFloat do mesmo grupo (cache de 6 h por nome).
async fn rare_evidence(h: &Hunt, name: &str, group_key: &str) -> RareEvidence {
    let now = h.now();
    let cached = h
        .lock()
        .csfloat_cache
        .get(name)
        .filter(|(at, _)| now - at < HISTORY_TTL)
        .map(|(_, v)| v.clone());
    let sales = match cached {
        Some(v) => v,
        None => {
            let (at, v) = match h.csfloat.sales(name).await {
                Ok(v) => (now, v),
                Err(e) => {
                    tracing::warn!("CSFloat indisponível para {name}: {e}");
                    // tenta de novo em ~10 min, sem martelar
                    (now - HISTORY_TTL + 600, vec![])
                }
            };
            h.lock()
                .csfloat_cache
                .insert(name.to_string(), (at, v.clone()));
            v
        }
    };
    RareEvidence {
        multiples: sales
            .iter()
            .filter(|s| !s.has_stickers)
            .filter(|s| {
                group_of(name, s.seed.map(u32::from), s.paint_index)
                    .is_some_and(|g| g.key == group_key)
            })
            .map(|s| s.multiple)
            .collect(),
    }
}

fn api_confidence(c: SellConfidence) -> Confidence {
    match c {
        SellConfidence::High => Confidence::Alta,
        SellConfidence::Medium => Confidence::Media,
        SellConfidence::Low => Confidence::Baixa,
    }
}

fn api_sell(s: &sell::Sell) -> Sell {
    let t = |t: &sell::SellTier| Tier {
        price_cents: t.price_cents,
        net_cents: t.net_cents,
        profit_cents: t.profit_cents,
        eta_hours: t.eta_hours,
    };
    Sell {
        high: t(&s.high),
        mid: t(&s.mid),
        quick: t(&s.quick),
    }
}

fn steam_url(code: &str, l: &Listing) -> String {
    format!(
        "https://steamcommunity.com/market/listings/730/{code}?detail={}",
        l.listing_id
    )
}

/// Avalia um anúncio novo. `Ok(None)`: sem estimativa ou sem lucro (conclusivo). `Err`: falha de rede
/// ao buscar histórico — o anúncio não é marcado como visto e volta a ser avaliado na próxima consulta.
async fn evaluate_one(
    h: &Hunt,
    task: &Task,
    l: &Listing,
    others_min: Option<i64>,
    notify_min: i64,
) -> Result<Option<(Opportunity, bool)>> {
    let now = h.now();
    let points = points_for(h, &l.name, now).await?;
    let group = group_of(&l.name, l.seed.map(u32::from), l.paint_index);
    let rare = match &group {
        Some(g) => Some(rare_evidence(h, &l.name, &g.key).await),
        None => None,
    };
    let Ok(est) = sell::estimate(&points, now, others_min, rare) else {
        return Ok(None);
    };
    let ev = sell::evaluate(l.price_cents, &est, notify_min);
    if !ev.listable {
        return Ok(None);
    }
    let opp = Opportunity {
        id: l.listing_id.clone(),
        name: l.name.clone(),
        icon_url: l.icon_url.clone(),
        steam_url: steam_url(&task.code, l),
        inspect_url: l.inspect_url.clone(),
        wear: wear_label(&l.name).map(str::to_string),
        float: l.float,
        seed: l.seed.map(i64::from),
        group: group.as_ref().map(|g| g.label.clone()),
        kind: if group.is_some() {
            Kind::Raro
        } else {
            Kind::Desconto
        },
        price_cents: l.price_cents,
        sell: api_sell(&ev.sell),
        margin_pct: ev.margin_pct,
        safe: ev.safe,
        confidence: api_confidence(est.confidence),
        evidence: est.evidence,
        seen_at: now,
    };
    Ok(Some((opp, ev.notify)))
}

/// Processa uma página de uma tarefa. Ver o módulo para as regras.
pub async fn process(h: &Hunt, task: &Task, page: &ListingsPage) -> Result<Outcome> {
    let now = h.now();
    let saldo = h.saldo();
    let relevant: Vec<&Listing> = page
        .listings
        .iter()
        .filter(|l| task.seeds.is_some() || l.name == task.name)
        .collect();
    let mut out = Outcome::default();

    // --- sumiu? só com página confiável; fora da primeira página o anúncio pode só ter sido empurrado
    let trust = {
        let mut s = h.lock();
        if page.listings.is_empty() {
            let n = s.empty_streak.entry(task.id.clone()).or_default();
            *n += 1;
            *n >= EMPTY_PAGES_TO_TRUST
        } else {
            s.empty_streak.remove(&task.id);
            true
        }
    };
    if trust {
        let present: HashSet<&str> = relevant.iter().map(|l| l.listing_id.as_str()).collect();
        let page_max = page
            .listings
            .iter()
            .map(|l| l.price_cents)
            .max()
            .unwrap_or(0);
        let missing: Vec<String> = h
            .lock()
            .active
            .values()
            .filter(|a| a.task == task.id && !present.contains(a.opp.id.as_str()))
            .filter(|a| !page.more || a.opp.price_cents <= page_max)
            .map(|a| a.opp.id.clone())
            .collect();
        out.gone = mark_gone(h, missing).await?;
    }

    // --- novos
    let ids: Vec<String> = relevant.iter().map(|l| l.listing_id.clone()).collect();
    let known: HashSet<String> =
        h.db.call(move |c| {
            let mut known = HashSet::new();
            for id in ids {
                let hit = c
                    .query_row(
                        "SELECT 1 FROM listings_seen WHERE listing_id = ?1",
                        [&id],
                        |_| Ok(()),
                    )
                    .optional()?;
                if hit.is_some() {
                    known.insert(id);
                }
            }
            Ok(known)
        })
        .await?;
    let notify_min = h.settings.view().await?.notify_min_profit_cents;
    let mut seen: Vec<&Listing> = relevant
        .iter()
        .copied()
        .filter(|l| known.contains(&l.listing_id))
        .collect();
    for l in relevant
        .iter()
        .copied()
        .filter(|l| !known.contains(&l.listing_id))
    {
        if l.price_cents > saldo {
            continue; // não cabe agora; se o saldo subir, ainda é novo
        }
        // Concorrência = outro anúncio do mesmo nome e do mesmo grupo raro (ou ambos comuns): um comum
        // barato não segura a venda de um T1.
        let key =
            |x: &Listing| group_of(&x.name, x.seed.map(u32::from), x.paint_index).map(|g| g.key);
        let mine = key(l);
        let others_min = page
            .listings
            .iter()
            .filter(|o| o.name == l.name && o.listing_id != l.listing_id && key(o) == mine)
            .map(|o| o.price_cents)
            .min();
        match evaluate_one(h, task, l, others_min, notify_min).await {
            Ok(found) => {
                seen.push(l);
                if let Some((opp, notify)) = found {
                    store_opportunity(h, task, l, &opp).await?;
                    out.found.push((opp, notify));
                }
            }
            Err(e) => tracing::warn!("avaliação de {} adiada: {e:#}", l.listing_id),
        }
    }
    store_seen(h, &seen, now).await?;

    if !out.found.is_empty() || !out.gone.is_empty() {
        h.recompute_combo();
        h.publish_state();
    }
    Ok(out)
}

/// id, nome, preço, seed, float.
type SeenRow = (String, String, i64, Option<i64>, Option<f64>);

async fn store_seen(h: &Hunt, seen: &[&Listing], now: i64) -> Result<()> {
    let rows: Vec<SeenRow> = seen
        .iter()
        .map(|l| {
            (
                l.listing_id.clone(),
                l.name.clone(),
                l.price_cents,
                l.seed.map(i64::from),
                l.float,
            )
        })
        .collect();
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for (id, name, price, seed, float) in rows {
            tx.execute(
                "INSERT INTO listings_seen(listing_id, name, price_cents, seed, \"float\", first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(listing_id) DO UPDATE SET last_seen = excluded.last_seen",
                params![id, name, price, seed, float, now],
            )?;
        }
        tx.commit()
    })
    .await
}

/// Guarda a oportunidade e o anúncio inteiro (a compra refaz o pedido a partir dele) e publica o evento.
async fn store_opportunity(h: &Hunt, task: &Task, l: &Listing, opp: &Opportunity) -> Result<()> {
    let (id, json, lj, tid) = (
        opp.id.clone(),
        serde_json::to_string(opp)?,
        serde_json::to_string(l)?,
        task.id.clone(),
    );
    let at = opp.seen_at;
    h.db.call(move |c| {
        c.execute(
            "INSERT OR REPLACE INTO opportunities(id, json, created_at, gone_at, task, listing_json)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
            params![id, json, at, tid, lj],
        )
        .map(|_| ())
    })
    .await?;
    {
        let mut s = h.lock();
        s.active.insert(
            opp.id.clone(),
            Active {
                opp: opp.clone(),
                task: task.id.clone(),
            },
        );
        s.queue.add_hit(&task.id);
    }
    let _ = h.events.send(Event::Opportunity(opp.clone()));
    Ok(())
}

/// Marca como encerradas (`gone_at`) e avisa o painel. Devolve os ids que estavam ativos.
async fn mark_gone(h: &Hunt, ids: Vec<String>) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(ids);
    }
    let now = h.now();
    let list = ids.clone();
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for id in &list {
            tx.execute(
                "UPDATE opportunities SET gone_at = ?2 WHERE id = ?1 AND gone_at IS NULL",
                params![id, now],
            )?;
        }
        tx.commit()
    })
    .await?;
    {
        let mut s = h.lock();
        for id in &ids {
            s.active.remove(id);
        }
    }
    for id in &ids {
        let _ = h.events.send(Event::Gone { id: id.clone() });
    }
    Ok(ids)
}

/// Tarefas que saíram da seleção deixam de ser conferidas: suas oportunidades não podem ficar na lista
/// para sempre como se ainda estivessem vivas.
pub async fn drop_tasks(h: &Hunt, tasks: &[String]) -> Result<()> {
    let ids: Vec<String> = h
        .lock()
        .active
        .values()
        .filter(|a| tasks.contains(&a.task))
        .map(|a| a.opp.id.clone())
        .collect();
    if !mark_gone(h, ids).await?.is_empty() {
        h.recompute_combo();
    }
    Ok(())
}
