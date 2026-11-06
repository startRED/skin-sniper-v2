//! Detecção: anúncio novo (via `listings_seen`) com preço ≤ saldo → estimativa → avaliação → `Opportunity`
//! no banco e evento SSE; anúncio que sumiu numa nova consulta da mesma tarefa → `gone`.
//! Docs/projeto.md, "Caça" 4 e "Precificação".

use super::catalog::{FLOOR_TTL, HISTORY_TTL, min_profit, wear_label, window_stats};
use super::scheduler::{Found, NEAR_WEIGHT, Task};
use super::{Active, Hunt};
use crate::notify::{Alert, NotifyError};
use crate::pricing::groups::group_of;
use crate::pricing::sell::{self, Confidence as SellConfidence, PricePoint, RareEvidence};
use crate::steam::buy::brl;
use crate::steam::client::Error as SteamError;
use crate::steam::history;
use crate::steam::listings::{Listing, ListingsPage};
use crate::web::api::{Confidence, Event, Kind, Opportunity, Risk, Sell, Tier};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

const DAY: i64 = 86_400;
/// Uma página vazia pode ser transitória (conhecimento 1.3): só vale como "sumiram" na segunda seguida.
const EMPTY_PAGES_TO_TRUST: u8 = 2;
/// Balanço do que foi conferido a cada 10 min, para o painel e o log.
const CYCLE_SECS: i64 = 600;
const NEAR_KEEP: usize = 3;
/// shortcut: o mapa de avaliados só esvazia quando enche; subir se a reavaliação em massa pesar.
const EVALUATED_MAX: usize = 50_000;

/// Anúncio que chegou perto de virar oportunidade (lucro pode ser negativo).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Near {
    pub name: String,
    pub price_cents: i64,
    pub mid_cents: i64,
    pub quick_cents: i64,
}

#[derive(Debug, Default)]
pub(crate) struct Cycle {
    started: i64,
    evaluated: u32,
    pub(crate) saved_listings: u32,
    passed: u32,
    pub(crate) near: Vec<Near>,
    last: Option<String>,
}

fn profit_text(c: i64) -> String {
    if c >= 0 {
        format!("{} de lucro", brl(c))
    } else {
        format!("{} de prejuízo", brl(-c))
    }
}

impl Cycle {
    pub(crate) fn record(&mut self, passed: bool, near: Option<Near>) {
        self.evaluated += 1;
        self.passed += u32::from(passed);
        // O mesmo anúncio volta em consultas seguidas; conta uma vez só na lista dos mais perto.
        if let Some(n) = near.filter(|n| !self.near.contains(n)) {
            self.near.push(n);
            self.near.sort_by_key(|n| std::cmp::Reverse(n.mid_cents));
            self.near.truncate(NEAR_KEEP);
        }
    }

    fn text(&self, secs: i64) -> String {
        let min = (secs / 60).max(1);
        let mut t = format!("{} anúncios conferidos em {min} min", self.evaluated);
        if self.passed > 0 {
            t += &format!("; {} deram lucro", self.passed);
        } else if let Some(n) = self.near.first() {
            t += &format!(
                "; nenhum deu lucro, o mais perto dava {}",
                profit_text(n.mid_cents)
            );
        } else if self.evaluated > 0 {
            t += "; nenhum deu para estimar o preço de venda";
        }
        t
    }

    /// Último balanço fechado; antes do primeiro, o parcial.
    pub(crate) fn message(&self, now: i64) -> Option<String> {
        self.last
            .clone()
            .or_else(|| (self.evaluated > 0).then(|| self.text(now - self.started)))
    }
}

/// Fecha o ciclo de 10 min: guarda o texto do painel e loga os 3 que chegaram mais perto.
pub(crate) fn roll_cycle(h: &Hunt) {
    let now = h.now();
    let mut s = h.lock();
    if s.cycle.started == 0 {
        s.cycle.started = now;
        return;
    }
    if now - s.cycle.started < CYCLE_SECS {
        return;
    }
    let c = std::mem::take(&mut s.cycle);
    s.cycle.started = now;
    let text = c.text(now - c.started);
    s.cycle.last = Some(text.clone());
    drop(s);
    tracing::info!(
        avaliados = c.evaluated,
        passaram = c.passed,
        consultas_anuncios_economizadas = c.saved_listings,
        "{text}"
    );
    for n in &c.near {
        tracing::info!(
            "perto: {} a {}; no preço médio {}, no rápido {}",
            n.name,
            brl(n.price_cents),
            profit_text(n.mid_cents),
            profit_text(n.quick_cents)
        );
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// Oportunidades novas e se vale avisar (`notify` da avaliação).
    pub found: Vec<(Opportunity, bool)>,
    pub gone: Vec<String>,
    /// O que a consulta rendeu à agenda (achados novos e quase-lucros).
    pub rewarded: Found,
}

/// Quase lucro: não entrou na lista, mas faltou pouco — até R$ 0,30 ou 10 % do preço, o que for maior.
fn near_credit(n: &Near) -> Option<f64> {
    let need = min_profit(n.price_cents);
    let short = need - n.mid_cents;
    (short > 0 && short <= 30.max(n.price_cents / 10)).then_some(NEAR_WEIGHT * need as f64)
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

async fn history_at(h: &Hunt, name: &str) -> Result<Option<i64>> {
    let n = name.to_string();
    h.db.call(move |c| {
        c.query_row("SELECT history_at FROM items WHERE name = ?1", [n], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()
        .map(Option::flatten)
    })
    .await
}

/// Pontos dos últimos 30 dias do nome; baixa o histórico se passou de 6 h.
pub(crate) async fn points_for(h: &Hunt, name: &str, now: i64) -> Result<Vec<PricePoint>> {
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
        .query_map(params![n, now - 30 * DAY], |r| {
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
                Ok(v) => {
                    h.set_keys(|k| {
                        if k.csfloat.set {
                            k.csfloat.ok = Some(true);
                            k.csfloat.detail = None;
                        }
                    });
                    (now, v)
                }
                Err(e) => {
                    if let SteamError::Csfloat { status: 401 | 403 } = e {
                        h.set_keys(|k| {
                            if k.csfloat.set {
                                k.csfloat.ok = Some(false);
                                k.csfloat.detail = Some("chave recusada pelo CSFloat".into());
                            }
                        });
                    }
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
                group_of(name, s.seed.map(u32::from), s.paint_index, s.float)
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

fn api_sell(s: &sell::Sell, correction: f64) -> Sell {
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
        correction,
    }
}

fn steam_url(code: &str, l: &Listing) -> String {
    format!(
        "https://steamcommunity.com/market/listings/730/{code}?detail={}",
        l.listing_id
    )
}

/// Resultado de uma avaliação conclusiva.
struct Judged {
    /// Virou (ou segue) oportunidade; o bool é o `notify` da avaliação.
    found: Option<(Opportunity, bool)>,
    near: Option<Near>,
}

/// Avalia um anúncio. `Ok` sem `found`: sem estimativa ou sem lucro (conclusivo). `Err`: falha de rede
/// ao buscar histórico — o anúncio não é marcado como visto e volta a ser avaliado na próxima consulta.
async fn evaluate_one(
    h: &Hunt,
    task: &Task,
    l: &Listing,
    others_min: Option<i64>,
    notify_min: i64,
) -> Result<Judged> {
    let now = h.now();
    let mut points = points_for(h, &l.name, now).await?;
    let group = group_of(&l.name, l.seed.map(u32::from), l.paint_index, l.float);
    let correction = h.correction(group.is_some());
    sell::discount(&mut points, correction);
    let rare = match &group {
        Some(g) => Some(rare_evidence(h, &l.name, &g.key).await),
        None => None,
    };
    let Ok(mut est) = sell::estimate(&points, now, others_min, rare) else {
        return Ok(Judged {
            found: None,
            near: None,
        });
    };
    if correction < 1.0 {
        est.evidence += &format!(
            " · suas vendas saíram {}% abaixo do previsto, já descontado",
            ((1.0 - correction) * 100.0).round()
        );
    }
    let ev = sell::evaluate(l.price_cents, &est, notify_min);
    let near = Some(Near {
        name: l.name.clone(),
        price_cents: l.price_cents,
        mid_cents: ev.sell.mid.net_cents - l.price_cents,
        quick_cents: ev.sell.quick.net_cents - l.price_cents,
    });
    if !ev.listable {
        return Ok(Judged { found: None, near });
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
        sell: api_sell(&ev.sell, correction),
        margin_pct: ev.margin_pct,
        safe: ev.safe,
        confidence: api_confidence(est.confidence),
        ev_profit_cents: ev.ev_profit_cents,
        days_locked: ev.days_locked,
        profit_per_day_cents: ev.profit_per_day_cents,
        risk: Some(match est.risk {
            sell::Risk::Low => Risk::Baixo,
            sell::Risk::Medium => Risk::Medio,
            sell::Risk::High => Risk::Alto,
        }),
        evidence: est.evidence,
        seen_at: now,
        gone_at: None,
    };
    Ok(Judged {
        found: Some((opp, ev.notify)),
        near,
    })
}

/// Processa uma página de uma tarefa. Ver o módulo para as regras.
pub async fn process(h: &Hunt, task: &Task, page: &ListingsPage) -> Result<Outcome> {
    let now = h.now();
    let saldo = h.saldo();
    let relevant: Vec<&Listing> = page
        .listings
        .iter()
        .filter(|l| task.rare() || l.name == task.name)
        .collect();
    let mut out = Outcome::default();
    let floor = relevant.iter().map(|l| l.price_cents).min();
    h.lock().floors.insert(task.floor_key(), (floor, now));
    let key = |x: &Listing| {
        group_of(&x.name, x.seed.map(u32::from), x.paint_index, x.float).map(|g| g.key)
    };
    // Raro: o menor anúncio de cada (nome, grupo) nas outras faixas, vistas há menos de FLOOR_TTL.
    // shortcut: o concorrente novo de uma faixa só pesa na oportunidade de outra quando a dela é consultada de
    // novo; se isso deixar lucro velho na tela, reavaliar as ativas do mesmo grupo aqui.
    let mut elsewhere = super::Rivals::new();
    if task.rare() {
        let mut here = super::Rivals::new();
        for l in &relevant {
            let p = here
                .entry((l.name.clone(), key(l)))
                .or_insert(l.price_cents);
            *p = (*p).min(l.price_cents);
        }
        let (mine, prefix) = (task.floor_key(), format!("{}#", task.id));
        let mut s = h.lock();
        for (_, (_, m)) in s
            .rivals
            .iter()
            .filter(|(k, (at, _))| **k != mine && k.starts_with(&prefix) && now - at < FLOOR_TTL)
        {
            for (g, &p) in m {
                let e = elsewhere.entry(g.clone()).or_insert(p);
                *e = (*e).min(p);
            }
        }
        s.rivals.insert(mine, (now, here));
    }
    let ids: Vec<String> = relevant.iter().map(|l| l.listing_id.clone()).collect();
    if let Err(error) = h.db.call(move |c| {
        let tx = c.transaction()?;
        for id in ids {
            let is_new = tx.execute("INSERT OR IGNORE INTO activity_seen(listing_id, last_seen) VALUES (?1, ?2)", params![id, now])? == 1;
            if is_new {
                tx.execute("INSERT INTO hourly_activity(hour_utc, listings) VALUES (?1, 1) ON CONFLICT(hour_utc) DO UPDATE SET listings = listings + 1", [now.div_euclid(3600)])?;
            } else {
                tx.execute("UPDATE activity_seen SET last_seen = ?2 WHERE listing_id = ?1", params![id, now])?;
            }
        }
        tx.commit()
    }).await {
        tracing::warn!(%error, "falha ao registrar atividade de anúncios");
    }

    // --- sumiu? só com página confiável; fora da primeira página o anúncio pode só ter sido empurrado
    let trust = {
        let mut s = h.lock();
        if page.listings.is_empty() {
            let n = s.empty_streak.entry(task.id.clone()).or_default();
            *n = n.saturating_add(1);
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
            // raro em rodízio: a página só cobre a faixa de seed consultada
            .filter(|a| {
                task.seeds.first().is_none_or(|&(lo, hi)| {
                    a.opp
                        .seed
                        .is_some_and(|s| (i64::from(lo)..=i64::from(hi)).contains(&s))
                })
            })
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
    let view = h.settings.view().await?;
    let notify_min = view.notify_min_profit_cents;
    let mut seen: Vec<&Listing> = relevant
        .iter()
        .copied()
        .filter(|l| known.contains(&l.listing_id))
        .collect();
    for l in relevant.iter().copied() {
        let was_known = known.contains(&l.listing_id);
        let active = h.lock().active.get(&l.listing_id).map(|a| a.opp.clone());
        if l.price_cents > saldo && active.is_none() {
            continue; // não cabe agora; se o saldo subir, ainda é novo
        }
        // Concorrência = outro anúncio do mesmo nome e do mesmo grupo raro (ou ambos comuns): um comum
        // barato não segura a venda de um T1. Inclui as outras faixas do mesmo raro.
        let mine = key(l);
        let others_min = page
            .listings
            .iter()
            .filter(|o| o.name == l.name && o.listing_id != l.listing_id && key(o) == mine)
            .map(|o| o.price_cents)
            .chain(elsewhere.get(&(l.name.clone(), mine.clone())).copied())
            .min();
        // Já visto: só reavalia se o preço, o histórico ou o menor concorrente mudaram desde a última
        // avaliação (ou se é oportunidade ativa e o histórico passou do prazo: a avaliação pode rebaixá-la).
        // shortcut: a janela de 7 dias anda sem mudar a chave; um dia que sai dela só pesa quando o histórico
        // vence (até 6 h). Se isso deixar oportunidade velha na tela, guardar o dia da janela na chave.
        let hist = history_at(h, &l.name).await?;
        if was_known {
            let last = h.lock().evaluated.get(&l.listing_id).copied();
            let aged = active.is_some() && hist.is_none_or(|t| now - t >= HISTORY_TTL);
            if last == Some((l.price_cents, hist.unwrap_or(0), others_min)) && !aged {
                continue;
            }
        }
        let judged = match evaluate_one(h, task, l, others_min, notify_min).await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("avaliação de {} adiada: {e:#}", l.listing_id);
                continue;
            }
        };
        let hist_now = history_at(h, &l.name).await?.unwrap_or(0);
        {
            if let Some(c) = judged
                .near
                .as_ref()
                .and_then(near_credit)
                .filter(|_| judged.found.is_none())
            {
                out.rewarded.near_hits += 1;
                out.rewarded.near_credit += c;
            }
            let mut s = h.lock();
            if s.evaluated.len() >= EVALUATED_MAX {
                s.evaluated.clear();
            }
            s.evaluated
                .insert(l.listing_id.clone(), (l.price_cents, hist_now, others_min));
            s.cycle.record(judged.found.is_some(), judged.near);
        }
        if !was_known {
            seen.push(l);
        }
        match (judged.found, active) {
            (Some((mut opp, notify)), prev) => {
                let fresh = prev.is_none();
                if let Some(p) = &prev {
                    opp.seen_at = p.seen_at; // a ordem da lista não muda por reavaliar
                    if serde_json::to_string(&opp)? == serde_json::to_string(p)? {
                        continue;
                    }
                }
                store_opportunity(h, task, l, &opp, fresh).await?;
                if fresh {
                    out.rewarded.finds += 1;
                    out.rewarded.gain_cents += opp.sell.mid.profit_cents;
                }
                out.found.push((opp, fresh && notify));
            }
            (None, Some(_)) => out
                .gone
                .extend(mark_gone(h, vec![l.listing_id.clone()]).await?),
            (None, None) => {}
        }
    }
    store_seen(h, &seen, now).await?;

    for (opp, _) in out.found.iter().filter(|(_, notify)| *notify) {
        send_alert(h, opp, view.panel_url.clone());
    }
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
                 ON CONFLICT(listing_id) DO UPDATE SET last_seen = excluded.last_seen,
                    price_cents = excluded.price_cents",
                params![id, name, price, seed, float, now],
            )?;
        }
        tx.commit()
    })
    .await
}

/// Guarda a oportunidade e o anúncio inteiro (a compra refaz o pedido a partir dele) e publica o evento.
async fn store_opportunity(
    h: &Hunt,
    task: &Task,
    l: &Listing,
    opp: &Opportunity,
    fresh: bool,
) -> Result<()> {
    let (id, json, lj, tid) = (
        opp.id.clone(),
        serde_json::to_string(opp)?,
        serde_json::to_string(l)?,
        task.id.clone(),
    );
    let at = opp.seen_at;
    h.db.call(move |c| {
        let tx = c.transaction()?;
        if fresh
            && let Err(error) = tx.execute("INSERT INTO hourly_activity(hour_utc, opportunities) VALUES (?1, 1) ON CONFLICT(hour_utc) DO UPDATE SET opportunities = opportunities + 1", [at.div_euclid(3600)]) {
            tracing::warn!(%error, "falha ao registrar atividade de oportunidades");
        }
        tx.execute(
            "INSERT OR REPLACE INTO opportunities(id, json, created_at, gone_at, task, listing_json)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
            params![id, json, at, tid, lj],
        )?;
        tx.commit()
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
    }
    let _ = h.events.send(Event::Opportunity(opp.clone()));
    Ok(())
}

/// Marca como encerradas (`gone_at`) e avisa o painel. Devolve os ids que estavam ativos.
pub(crate) async fn mark_gone(h: &Hunt, ids: Vec<String>) -> Result<Vec<String>> {
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

/// Aviso no Windows e no Telegram (uma vez por anúncio: só chega aqui o que é novo; o próprio
/// `Telegram` também lembra). Em segundo plano: o envio espera o ritmo do Telegram e não pode segurar a caça.
fn send_alert(h: &Hunt, opp: &Opportunity, panel_url: Option<String>) {
    crate::notify::desktop(
        &format!("{} de lucro · {}", brl(opp.sell.mid.profit_cents), opp.name),
        &format!(
            "Compre por {} e venda por {}. Clique para abrir na Steam.",
            brl(opp.price_cents),
            brl(opp.sell.mid.price_cents)
        ),
        &opp.steam_url,
    );
    let Some(tg) = h.telegram.clone() else {
        return;
    };
    let alert = Alert {
        id: opp.id.clone(),
        name: opp.name.clone(),
        group: opp.group.clone(),
        price_cents: opp.price_cents,
        mid_profit_cents: opp.sell.mid.profit_cents,
        safe_profit_cents: opp.sell.quick.profit_cents,
        eta_hours: opp.sell.mid.eta_hours,
    };
    tokio::spawn(async move {
        match tg.notify(&alert, panel_url.as_deref()).await {
            Ok(_) | Err(NotifyError::NotConfigured) => {}
            Err(e) => tracing::warn!("aviso no Telegram falhou: {e}"),
        }
    });
}
