//! Professor grátis (mina de ouro, Fase 1): cada aviso e cada quase-achado vira uma compra virtual (`ghosts`) e
//! recebe um resultado medido pelo mercado, sem comprar nada e sem gastar o balde de anúncios.
//!
//! - **Rótulo rápido (48 h):** comum = parte das vendas do histórico nas 48 h seguintes a ≥ o preço médio
//!   previsto e anúncios da mesma skin a ≥ esse preço que sumiram sem relistagem; raro = sumiço do mesmo grupo ou
//!   venda do CSFloat do grupo a ≥ o múltiplo sugerido.
//! - **Rótulo final:** comum no dia 10 = venda pelo menor entre a mediana dos dias 7–9 e o menor anúncio do livro
//!   nesses dias (lucro virtual realizado); sem nenhum dos dois até o dia 12, "sem rótulo". Raro no dia 14 =
//!   "fechou" ou não (mesmas fontes do rápido, entre os dias 7 e 14).
//!
//! Os prazos (48 h, dias 7–9, 12 e 14) e a régua de quase-achado são hipóteses de calibração.
//! Docs/projeto.md, "Aprendizado".

use super::Hunt;
use super::catalog::min_profit;
use crate::csfloat::CsfloatSale;
use crate::pricing::fees::seller_gets;
use crate::pricing::groups::group_of;
use crate::pricing::sell::weighted_quantile;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
/// O professor roda de hora em hora.
const GAP: i64 = HOUR;
/// Janela do rótulo rápido.
pub(crate) const FAST: i64 = 48 * HOUR;
/// A Steam trava por 7 dias o que é comprado no Mercado; a venda virtual olha os dias 7–9.
const SELL_FROM: i64 = 7 * DAY;
const SELL_UNTIL: i64 = 10 * DAY;
/// Sem histórico nem livro até aqui, o comum fecha como "sem rótulo".
const GIVE_UP: i64 = 12 * DAY;
/// O raro "fecha" entre os dias 7 e 14.
const RARE_UNTIL: i64 = 14 * DAY;

/// Compra virtual de um anúncio novo avaliado.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ghost {
    pub listing_id: String,
    pub name: String,
    /// Grupo raro (`Group::key`); `None` = comum.
    pub grp: Option<String>,
    pub price_cents: i64,
    /// Preço médio de venda previsto e o líquido dele.
    pub mid_price_cents: i64,
    pub mid_net_cents: i64,
    /// Raro: preço médio comum da mesma skin (sem prêmio), base do múltiplo sugerido.
    pub base_cents: Option<i64>,
    /// Virou oportunidade (aviso); senão é quase-achado.
    pub approved: bool,
}

/// Vale uma compra virtual: virou oportunidade ou faltou pouco (até R$ 0,30 ou 10 % do preço abaixo da régua).
pub(crate) fn ghost_worthy(price_cents: i64, mid_profit_cents: i64) -> bool {
    mid_profit_cents >= min_profit(price_cents) - 30.max(price_cents / 10)
}

pub(crate) async fn add_ghost(h: &Hunt, g: Ghost, now: i64) {
    let r = h
        .db
        .call(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO ghosts(listing_id, name, kind, grp, bought_at, price_cents, mid_price_cents,
                    mid_net_cents, base_cents, approved)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    g.listing_id,
                    g.name,
                    if g.grp.is_some() { "raro" } else { "desconto" },
                    g.grp,
                    now,
                    g.price_cents,
                    g.mid_price_cents,
                    g.mid_net_cents,
                    g.base_cents,
                    g.approved
                ],
            )
        })
        .await;
    if let Err(error) = r {
        tracing::warn!(%error, "professor: compra virtual não gravada");
    }
}

/// Guarda as vendas limpas (sem adesivo) do CSFloat com o grupo de cada uma. A mesma venda lida de novo é ignorada.
pub(crate) async fn store_csfloat(h: &Hunt, name: &str, sales: &[CsfloatSale]) {
    let rows: Vec<_> = {
        let s = h.lock();
        sales
            .iter()
            .filter(|x| !x.has_stickers && x.multiple.is_finite() && x.multiple > 0.0)
            .map(|x| {
                let grp = group_of(name, x.seed.map(u32::from), x.paint_index, x.float)
                    .or_else(|| super::patterns::group(&s.learned, name, x.seed).map(|(g, _)| g))
                    .map(|g| g.key);
                (
                    x.sold_at,
                    x.price_usd_cents,
                    x.base_price_usd_cents,
                    x.multiple,
                    x.seed,
                    x.float,
                    x.paint_index,
                    grp,
                )
            })
            .collect()
    };
    if rows.is_empty() {
        return;
    }
    let name = name.to_string();
    let r = h
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            for (at, price, base, m, seed, float, paint, grp) in rows {
                tx.execute(
                    "INSERT OR IGNORE INTO csfloat_sales(name, sold_at, price_usd_cents, base_usd_cents, multiple,
                        seed, \"float\", paint_index, grp)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![name, at, price, base, m, seed, float, paint, grp],
                )?;
            }
            tx.commit()
        })
        .await;
    if let Err(error) = r {
        tracing::warn!(%error, "professor: vendas do CSFloat não gravadas");
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Labeled {
    pub fast: u32,
    pub done: u32,
    pub unlabeled: u32,
}

/// Mediana ponderada (pelas compras) do histórico de `name` em `(from, to]`.
fn sales_median(c: &Connection, name: &str, from: i64, to: i64) -> rusqlite::Result<Option<i64>> {
    let pairs: Vec<(f64, f64)> = c
        .prepare(
            "SELECT median_cents, purchases FROM price_points WHERE name = ?1 AND ts > ?2 AND ts <= ?3
             AND purchases > 0 AND median_cents > 0",
        )?
        .query_map(params![name, from, to], |r| {
            Ok((r.get::<_, i64>(0)? as f64, r.get::<_, i64>(1)? as f64))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(weighted_quantile(&pairs, 0.5).map(|m| m.round() as i64))
}

/// Parte das compras do histórico em `(from, to]` cuja mediana horária ficou ≥ `price`; `None` sem compras.
fn share_at_or_above(
    c: &Connection,
    name: &str,
    from: i64,
    to: i64,
    price: i64,
) -> rusqlite::Result<Option<f64>> {
    let (above, all): (i64, i64) = c.query_row(
        "SELECT COALESCE(SUM(CASE WHEN median_cents >= ?4 THEN purchases ELSE 0 END), 0), COALESCE(SUM(purchases), 0)
         FROM price_points WHERE name = ?1 AND ts > ?2 AND ts <= ?3 AND purchases > 0",
        params![name, from, to, price],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((all > 0).then(|| above as f64 / all as f64))
}

/// Anúncios do mesmo nome (e do mesmo grupo, no raro) a ≥ `price` que sumiram sem relistagem em `[from, to]`.
fn vanished_at_or_above(
    c: &Connection,
    name: &str,
    grp: Option<&str>,
    from: i64,
    to: i64,
    price: i64,
) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT count(*) FROM observations WHERE name = ?1 AND grp IS ?2 AND relisted = 0
         AND gone_at >= ?3 AND gone_at <= ?4 AND price_cents >= ?5",
        params![name, grp, from, to, price],
        |r| r.get(0),
    )
}

/// Vendas do CSFloat do grupo em `[from, to]` a um múltiplo ≥ `multiple`.
fn csfloat_at_or_above(
    c: &Connection,
    grp: &str,
    from: i64,
    to: i64,
    multiple: f64,
) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT count(*) FROM csfloat_sales WHERE grp = ?1 AND sold_at >= ?2 AND sold_at <= ?3 AND multiple >= ?4",
        params![grp, from, to, multiple],
        |r| r.get(0),
    )
}

/// Menor anúncio do livro de `name` no primeiro registro em `[from, to)`.
fn book_low(c: &Connection, name: &str, from: i64, to: i64) -> rusqlite::Result<Option<i64>> {
    let levels: Option<String> = c
        .query_row(
            "SELECT sell_levels FROM book_snapshots WHERE name = ?1 AND at >= ?2 AND at < ?3 ORDER BY at LIMIT 1",
            params![name, from, to],
            |r| r.get(0),
        )
        .optional()?;
    Ok(levels
        .and_then(|s| serde_json::from_str::<Vec<(i64, i64)>>(&s).ok())
        .and_then(|v| v.first().map(|l| l.0)))
}

struct Row {
    id: String,
    name: String,
    grp: Option<String>,
    bought: i64,
    price: i64,
    mid: i64,
    base: Option<i64>,
}

fn rows(c: &Connection, sql: &str, p: impl rusqlite::Params) -> rusqlite::Result<Vec<Row>> {
    c.prepare(sql)?
        .query_map(p, |r| {
            Ok(Row {
                id: r.get(0)?,
                name: r.get(1)?,
                grp: r.get(2)?,
                bought: r.get(3)?,
                price: r.get(4)?,
                mid: r.get(5)?,
                base: r.get(6)?,
            })
        })?
        .collect()
}

const COLS: &str =
    "listing_id, name, grp, bought_at, price_cents, mid_price_cents, base_cents FROM ghosts";

/// Múltiplo sugerido do raro: preço médio previsto ÷ preço médio comum.
fn suggested_multiple(r: &Row) -> Option<f64> {
    r.base.filter(|&b| b > 0).map(|b| r.mid as f64 / b as f64)
}

/// "Fechou" do raro em `[from, to]`: sumiço do grupo a ≥ o preço sugerido ou venda do CSFloat a ≥ o múltiplo.
fn rare_closed(c: &Connection, r: &Row, from: i64, to: i64) -> rusqlite::Result<i64> {
    let grp = r.grp.as_deref().unwrap_or_default();
    let mut n = vanished_at_or_above(c, &r.name, Some(grp), from, to, r.mid)?;
    if let Some(m) = suggested_multiple(r) {
        n += csfloat_at_or_above(c, grp, from, to, m)?;
    }
    Ok(n)
}

/// Rotula o que já pode ser rotulado. Síncrono: rode dentro de `Db::call`.
pub(crate) fn label(c: &mut Connection, now: i64) -> rusqlite::Result<Labeled> {
    let mut out = Labeled::default();
    let tx = c.transaction()?;
    // rápido: 48 h depois da compra virtual
    for r in rows(
        &tx,
        &format!("SELECT {COLS} WHERE fast_at IS NULL AND bought_at <= ?1"),
        [now - FAST],
    )? {
        let (from, to) = (r.bought, r.bought + FAST);
        let (share, median, sold, closed) = if r.grp.is_some() {
            let n = rare_closed(&tx, &r, from, to)?;
            (None, None, n, Some(n > 0))
        } else {
            let share = share_at_or_above(&tx, &r.name, from, to, r.mid)?;
            let median = sales_median(&tx, &r.name, from, to)?;
            let n = vanished_at_or_above(&tx, &r.name, None, from, to, r.mid)?;
            // sem compra nenhuma no histórico e sem sumiço visto: não dá para dizer
            let closed = match (share, n) {
                (_, 1..) => Some(true),
                (Some(s), _) => Some(s >= 0.5),
                (None, _) => None,
            };
            (share, median, n, closed)
        };
        tx.execute(
            "UPDATE ghosts SET fast_at = ?2, fast_share = ?3, fast_median_cents = ?4, fast_sold = ?5,
                fast_closed = ?6 WHERE listing_id = ?1",
            params![r.id, now, share, median, sold, closed],
        )?;
        out.fast += 1;
    }
    // final do comum: dia 10 (mediana dos dias 7–9 completa); sem dado até o dia 12, sem rótulo
    for r in rows(
        &tx,
        &format!("SELECT {COLS} WHERE status = 'aguardando' AND grp IS NULL AND bought_at <= ?1"),
        [now - SELL_UNTIL],
    )? {
        let (from, to) = (r.bought + SELL_FROM, r.bought + SELL_UNTIL);
        let median = sales_median(&tx, &r.name, from, to)?;
        // vender no livro = 1 centavo abaixo do menor anúncio
        let book = book_low(&tx, &r.name, from, to)?.map(|p| p - 1);
        let sell = match (median, book) {
            (Some(m), Some(b)) => Some(m.min(b)),
            (m, b) => m.or(b),
        };
        let source = match (median, book) {
            (Some(_), Some(_)) => "historico+livro",
            (Some(_), None) => "historico",
            (None, Some(_)) => "livro",
            (None, None) => "",
        };
        match sell.filter(|&s| s > 0) {
            Some(s) => {
                let net = seller_gets(s);
                tx.execute(
                    "UPDATE ghosts SET status = 'rotulado', labeled_at = ?2, sell_cents = ?3, net_cents = ?4,
                        profit_cents = ?5, label_source = ?6 WHERE listing_id = ?1",
                    params![r.id, now, s, net, net - r.price, source],
                )?;
                out.done += 1;
            }
            None if now >= r.bought + GIVE_UP => {
                tx.execute(
                    "UPDATE ghosts SET status = 'sem_rotulo', labeled_at = ?2 WHERE listing_id = ?1",
                    params![r.id, now],
                )?;
                out.unlabeled += 1;
            }
            None => {}
        }
    }
    // final do raro: dia 14
    for r in rows(
        &tx,
        &format!(
            "SELECT {COLS} WHERE status = 'aguardando' AND grp IS NOT NULL AND bought_at <= ?1"
        ),
        [now - RARE_UNTIL],
    )? {
        let closed = rare_closed(&tx, &r, r.bought + SELL_FROM, r.bought + RARE_UNTIL)? > 0;
        // sem fechar, vende como comum (mediana dos dias 7–9); o lucro do raro é só informativo
        let sell = if closed {
            Some(r.mid)
        } else {
            sales_median(&tx, &r.name, r.bought + SELL_FROM, r.bought + SELL_UNTIL)?
        };
        let net = sell.map(seller_gets);
        tx.execute(
            "UPDATE ghosts SET status = 'rotulado', labeled_at = ?2, closed = ?3, sell_cents = ?4, net_cents = ?5,
                profit_cents = ?6, label_source = ?7 WHERE listing_id = ?1",
            params![
                r.id,
                now,
                closed,
                sell,
                net,
                net.map(|n| n - r.price),
                if closed { "fechou" } else { "nao_fechou" }
            ],
        )?;
        out.done += 1;
    }
    tx.commit()?;
    Ok(out)
}

/// Skins comuns com compra virtual nos dias 7–9 e sem leitura do livro registrada nesse prazo: o professor pede
/// uma leitura (balde do livro, nunca o de anúncios).
pub(crate) fn books_wanted(c: &Connection, now: i64) -> rusqlite::Result<Vec<String>> {
    c.prepare(
        "SELECT DISTINCT g.name FROM ghosts g WHERE g.status = 'aguardando' AND g.grp IS NULL
         AND g.bought_at <= ?1 AND g.bought_at > ?2
         AND NOT EXISTS (SELECT 1 FROM book_snapshots b WHERE b.name = g.name AND b.at >= g.bought_at + ?3)",
    )?
    .query_map(params![now - SELL_FROM, now - SELL_UNTIL, SELL_FROM], |r| {
        r.get(0)
    })?
    .collect()
}

/// Resumo do professor para o placar (`GET /api/learning`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Compras virtuais guardadas e as que esperam o rótulo final.
    pub ghosts: i64,
    pub waiting: i64,
    /// Rótulo rápido: avisos e quase-achados rotulados e quantos "fecharam" a ≥ o preço previsto.
    pub fast_alerts: i64,
    pub fast_alerts_closed: i64,
    pub fast_near: i64,
    pub fast_near_closed: i64,
    /// Mediana de (mediana das 48 h ÷ preço médio previsto) dos comuns: < 1 = o app prevê alto.
    pub fast_ratio: Option<f64>,
    /// Rótulo final dos avisos comuns: rotulados, quantos deram lucro, lucro virtual realizado e o previsto deles.
    pub final_alerts: i64,
    pub final_alerts_won: i64,
    pub profit_cents: i64,
    pub predicted_cents: i64,
    /// Comuns sem rótulo (sem histórico nem livro até o dia 12).
    pub unlabeled: i64,
    /// Raros com rótulo final e quantos fecharam.
    pub rare_labeled: i64,
    pub rare_closed: i64,
}

/// Mediana de (mediana das 48 h ÷ preço previsto) dos comuns rotulados a partir de `since`.
pub(crate) fn fast_ratio(c: &Connection, since: i64) -> rusqlite::Result<(Option<f64>, i64)> {
    let mut v: Vec<f64> = c
        .prepare(
            "SELECT CAST(fast_median_cents AS REAL) / mid_price_cents FROM ghosts
             WHERE grp IS NULL AND fast_at >= ?1 AND fast_median_cents > 0 AND mid_price_cents > 0",
        )?
        .query_map([since], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let n = v.len() as i64;
    v.sort_by(f64::total_cmp);
    let m = match v.len() {
        0 => None,
        k if k % 2 == 1 => Some(v[k / 2]),
        k => Some((v[k / 2 - 1] + v[k / 2]) / 2.0),
    };
    Ok((m, n))
}

pub(crate) fn summary(c: &Connection) -> rusqlite::Result<Summary> {
    const FINAL: &str = "status = 'rotulado' AND grp IS NULL AND approved = 1";
    let mut s = c.query_row(
        &format!(
            "SELECT count(*), coalesce(sum(status = 'aguardando'), 0),
                coalesce(sum(fast_at IS NOT NULL AND approved = 1), 0), coalesce(sum(fast_closed = 1 AND approved = 1), 0),
                coalesce(sum(fast_at IS NOT NULL AND approved = 0), 0), coalesce(sum(fast_closed = 1 AND approved = 0), 0),
                coalesce(sum({FINAL}), 0), coalesce(sum({FINAL} AND profit_cents > 0), 0),
                coalesce(sum(CASE WHEN {FINAL} THEN profit_cents END), 0),
                coalesce(sum(CASE WHEN {FINAL} THEN mid_net_cents - price_cents END), 0),
                coalesce(sum(status = 'sem_rotulo'), 0),
                coalesce(sum(status = 'rotulado' AND grp IS NOT NULL), 0),
                coalesce(sum(status = 'rotulado' AND grp IS NOT NULL AND closed = 1), 0)
             FROM ghosts"
        ),
        [],
        |r| {
            Ok(Summary {
                ghosts: r.get(0)?,
                waiting: r.get(1)?,
                fast_alerts: r.get(2)?,
                fast_alerts_closed: r.get(3)?,
                fast_near: r.get(4)?,
                fast_near_closed: r.get(5)?,
                fast_ratio: None,
                final_alerts: r.get(6)?,
                final_alerts_won: r.get(7)?,
                profit_cents: r.get(8)?,
                predicted_cents: r.get(9)?,
                unlabeled: r.get(10)?,
                rare_labeled: r.get(11)?,
                rare_closed: r.get(12)?,
            })
        },
    )?;
    s.fast_ratio = fast_ratio(c, 0)?.0;
    Ok(s)
}

/// Boletim de uma semana (`GET /api/learning`): consultas, achados e quase da caça; avisos rotulados em 48 h e
/// quantos venderiam; avisos comuns com rótulo final e o lucro virtual deles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Week {
    pub queries: i64,
    pub finds: i64,
    pub near: i64,
    pub alerts: i64,
    pub alerts_closed: i64,
    pub labeled: i64,
    pub profit_cents: i64,
}

/// Semana `[from, to)`.
pub(crate) fn week(c: &Connection, from: i64, to: i64) -> rusqlite::Result<Week> {
    let (queries, finds, near) = c.query_row(
        "SELECT coalesce(sum(queries), 0), coalesce(sum(opportunities), 0), coalesce(sum(near), 0)
         FROM hourly_activity WHERE hour_utc * 3600 >= ?1 AND hour_utc * 3600 < ?2",
        params![from, to],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let (alerts, alerts_closed) = c.query_row(
        "SELECT count(*), coalesce(sum(fast_closed = 1), 0) FROM ghosts
         WHERE approved = 1 AND fast_at >= ?1 AND fast_at < ?2",
        params![from, to],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (labeled, profit_cents) = c.query_row(
        "SELECT count(*), coalesce(sum(profit_cents), 0) FROM ghosts
         WHERE approved = 1 AND grp IS NULL AND status = 'rotulado' AND labeled_at >= ?1 AND labeled_at < ?2",
        params![from, to],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(Week {
        queries,
        finds,
        near,
        alerts,
        alerts_closed,
        labeled,
        profit_cents,
    })
}

/// Fator do professor em uso no comum (1 = sem desconto).
pub(crate) fn refit_factor(c: &Connection) -> rusqlite::Result<f64> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [REFIT_KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.as_deref()
        .and_then(|v| v.split(' ').next()?.parse().ok())
        .unwrap_or(1.0))
}

/// Recalibração (Fase 4): fator e dia de Brasília da última conta, `"<fator> <dia>"` em `settings`.
const REFIT_KEY: &str = "refit_common";
pub(crate) const REFIT_FLOOR: f64 = 0.80;
const REFIT_STEP: f64 = 0.02;
const REFIT_MIN_LABELS: i64 = 30;
const REFIT_MIN_DAYS: i64 = 7;

/// Fator novo do professor para o comum: alvo = fator aplicado × (real ÷ previsto) mediano das 48 h na última
/// semana, em [0,80; 1,00] (só desconta), andando no máximo 2 % por dia. Sem ≥ 7 dias de rótulos rápidos e ≥ 30
/// rótulos com mediana na semana, fica como está.
pub(crate) fn refit(prev: f64, applied: f64, ratio: Option<f64>, labels: i64, days: i64) -> f64 {
    let Some(r) = ratio.filter(|_| labels >= REFIT_MIN_LABELS && days >= REFIT_MIN_DAYS) else {
        return prev;
    };
    let next = (applied * r)
        .clamp(REFIT_FLOOR, 1.0)
        .clamp(prev - REFIT_STEP, prev + REFIT_STEP);
    (next * 1000.0).round() / 1000.0
}

/// Uma vez por dia de Brasília: recalcula o fator do comum e o põe em uso (também retoma o salvo ao subir).
async fn refit_step(h: &Hunt, now: i64) -> anyhow::Result<()> {
    let saved = super::catalog::get_setting(&h.db, REFIT_KEY).await?;
    let (prev, day) = saved
        .as_deref()
        .and_then(|v| {
            let (f, d) = v.split_once(' ')?;
            Some((f.parse::<f64>().ok()?, d.parse::<i64>().ok()?))
        })
        .unwrap_or((1.0, 0));
    let today = crate::web::learning::brt_day(now);
    if day == today {
        set_refit(h, prev);
        return Ok(());
    }
    let ((ratio, labels), first) =
        h.db.call(move |c| {
            let first: Option<i64> = c.query_row(
                "SELECT MIN(fast_at) FROM ghosts WHERE grp IS NULL AND fast_median_cents > 0",
                [],
                |r| r.get(0),
            )?;
            Ok((fast_ratio(c, now - 7 * DAY)?, first))
        })
        .await?;
    let days = first.map_or(0, |f| (now - f) / DAY);
    let applied = h.lock().correction[0].min(prev);
    let next = refit(prev, applied, ratio, labels, days);
    if next != prev {
        let why = format!(
            "real ÷ previsto {:.3} em {labels} rótulos de 48 h (7 dias); fator aplicado {applied:.3}",
            ratio.unwrap_or(1.0)
        );
        tracing::info!(
            antes = prev,
            depois = next,
            "professor recalibrou o comum: {why}"
        );
        h.db.call(move |c| {
            c.execute(
                "INSERT INTO refit_log(at, key, old, new, why) VALUES (?1, 'comum', ?2, ?3, ?4)",
                params![now, prev, next, why],
            )
            .map(|_| ())
        })
        .await?;
    }
    super::catalog::set_setting(&h.db, REFIT_KEY, format!("{next} {today}")).await?;
    set_refit(h, next);
    Ok(())
}

fn set_refit(h: &Hunt, f: f64) {
    let mut s = h.lock();
    if s.refit != f {
        s.refit = f;
        // previsões mudaram: cada anúncio volta a ser avaliado na próxima consulta
        s.evaluated.clear();
    }
}

/// Uma volta do professor, se é hora. Não fala com a Steam: só lê e escreve o banco.
pub async fn maybe_step(h: &Hunt) {
    let now = h.now();
    {
        let mut s = h.lock();
        if s.teach_at != 0 && now - s.teach_at < GAP {
            return;
        }
        s.teach_at = now;
    }
    match h
        .db
        .call(move |c| Ok((label(c, now)?, books_wanted(c, now)?)))
        .await
    {
        Ok((l, books)) => {
            if l != Labeled::default() {
                tracing::info!(
                    rapidos = l.fast,
                    finais = l.done,
                    sem_rotulo = l.unlabeled,
                    "professor rotulou"
                );
            }
            h.lock().label_books = books;
        }
        Err(e) => tracing::warn!("professor falhou: {e:#}"),
    }
    if let Err(e) = refit_step(h, now).await {
        tracing::warn!("professor: recalibração falhou: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, temp_db};
    use rusqlite::types::Value;

    const T: i64 = 1_793_000_000;
    const NAME: &str = "AK-47 | Slate (Field-Tested)";

    async fn exec(db: &Db, sql: &'static str, p: Vec<Value>) {
        db.call(move |c| c.execute(sql, rusqlite::params_from_iter(p)).map(|_| ()))
            .await
            .unwrap();
    }

    fn v(x: impl Into<Value>) -> Value {
        x.into()
    }

    async fn ghost(db: &Db, id: &str, grp: Option<&str>, price: i64, mid: i64, base: Option<i64>) {
        exec(
            db,
            "INSERT INTO ghosts(listing_id, name, kind, grp, bought_at, price_cents, mid_price_cents, mid_net_cents,
                base_cents, approved) VALUES (?1, ?2, 'x', ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            vec![
                v(id.to_string()),
                v(NAME.to_string()),
                v(grp.map(str::to_string)),
                v(T),
                v(price),
                v(mid),
                v(seller_gets(mid)),
                v(base),
            ],
        )
        .await;
    }

    async fn point(db: &Db, ts: i64, median: i64, n: i64) {
        exec(
            db,
            "INSERT INTO price_points(name, ts, median_cents, purchases) VALUES (?1, ?2, ?3, ?4)",
            vec![v(NAME.to_string()), v(ts), v(median), v(n)],
        )
        .await;
    }

    type Fast = (
        Option<i64>,
        Option<f64>,
        Option<i64>,
        Option<i64>,
        Option<bool>,
    );

    async fn fast(db: &Db, id: &str) -> Fast {
        let id = id.to_string();
        db.call(move |c| {
            c.query_row(
                "SELECT fast_at, fast_share, fast_median_cents, fast_sold, fast_closed FROM ghosts WHERE listing_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .await
        .unwrap()
    }

    type Final = (
        String,
        Option<i64>,
        Option<i64>,
        Option<bool>,
        Option<String>,
    );

    async fn fin(db: &Db, id: &str) -> Final {
        let id = id.to_string();
        db.call(move |c| {
            c.query_row(
                "SELECT status, sell_cents, profit_cents, closed, label_source FROM ghosts WHERE listing_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .await
        .unwrap()
    }

    async fn run(db: &Db, now: i64) -> Labeled {
        db.call(move |c| label(c, now)).await.unwrap()
    }

    #[test]
    fn near_and_approved_become_ghosts() {
        // R$ 10: régua de R$ 1,00 e folga de R$ 1,00 (10 %): lucro ≥ 0 vira compra virtual
        assert!(ghost_worthy(1000, 0));
        assert!(!ghost_worthy(1000, -1));
        // R$ 5: régua R$ 1,00, folga R$ 0,50
        assert!(ghost_worthy(500, 50));
        assert!(!ghost_worthy(500, 49));
        // R$ 1: folga mínima de R$ 0,30
        assert!(ghost_worthy(100, 70) && !ghost_worthy(100, 69));
    }

    #[tokio::test]
    async fn fast_label_reads_next_48h_sales() {
        let (db, _dir) = temp_db();
        ghost(&db, "a", None, 800, 1000, None).await;
        point(&db, T + 3600, 1100, 3).await;
        point(&db, T + 7200, 900, 1).await;
        point(&db, T + 50 * 3600, 2000, 10).await; // depois das 48 h: não conta
        assert_eq!(run(&db, T + FAST - 1).await.fast, 0);
        // um anúncio da skin a ≥ R$ 10 sumiu sem relistagem dentro das 48 h
        exec(
            &db,
            "INSERT INTO observations(listing_id, name, query, price_cents, first_seen, last_seen, cheapest, gone_at)
             VALUES ('o', ?1, 'q', 1000, ?2, ?2, 1, ?3)",
            vec![v(NAME.to_string()), v(T), v(T + 600)],
        )
        .await;
        assert_eq!(run(&db, T + FAST).await.fast, 1);
        let (at, share, median, sold, closed) = fast(&db, "a").await;
        assert_eq!(at, Some(T + FAST));
        assert!((share.unwrap() - 0.75).abs() < 1e-9);
        assert_eq!((median, sold, closed), (Some(1100), Some(1), Some(true)));
        // rotulado uma vez só
        assert_eq!(run(&db, T + FAST + 3600).await.fast, 0);
    }

    #[tokio::test]
    async fn fast_label_without_any_data_is_unknown() {
        let (db, _dir) = temp_db();
        ghost(&db, "a", None, 800, 1000, None).await;
        run(&db, T + FAST).await;
        let (_, share, _, sold, closed) = fast(&db, "a").await;
        assert_eq!((share, sold, closed), (None, Some(0), None));
        // vendas só abaixo do previsto: não fechou
        let (db, _dir) = temp_db();
        ghost(&db, "a", None, 800, 1000, None).await;
        point(&db, T + 3600, 900, 4).await;
        run(&db, T + FAST).await;
        assert_eq!(fast(&db, "a").await.4, Some(false));
    }

    #[tokio::test]
    async fn common_label_uses_min_of_history_and_book() {
        let (db, _dir) = temp_db();
        ghost(&db, "a", None, 800, 1000, None).await;
        point(&db, T + 7 * DAY + 3600, 1000, 5).await;
        exec(
            &db,
            "INSERT INTO book_snapshots VALUES (?1, ?2, NULL, 0, 3, '[[950,1],[990,2]]')",
            vec![v(NAME.to_string()), v(T + 7 * DAY + 7200)],
        )
        .await;
        // dia 9: a mediana dos dias 7–9 ainda não fechou
        assert_eq!(run(&db, T + 9 * DAY).await.done, 0);
        assert_eq!(run(&db, T + 10 * DAY).await.done, 1);
        let net = seller_gets(949);
        assert_eq!(
            fin(&db, "a").await,
            (
                "rotulado".into(),
                Some(949),
                Some(net - 800),
                None,
                Some("historico+livro".into())
            )
        );
    }

    #[tokio::test]
    async fn no_data_by_day_12_is_counted_as_unlabeled() {
        let (db, _dir) = temp_db();
        ghost(&db, "a", None, 800, 1000, None).await;
        assert_eq!(run(&db, T + 10 * DAY).await.done, 0);
        assert_eq!(fin(&db, "a").await.0, "aguardando");
        // o professor pede o livro enquanto está nos dias 7–9
        let want = db.call(|c| books_wanted(c, T + 8 * DAY)).await.unwrap();
        assert_eq!(want, vec![NAME.to_string()]);
        let early = db.call(|c| books_wanted(c, T + 6 * DAY)).await.unwrap();
        assert!(early.is_empty());
        let l = run(&db, T + 12 * DAY).await;
        assert_eq!((l.done, l.unlabeled), (0, 1));
        assert_eq!(fin(&db, "a").await.0, "sem_rotulo");
    }

    #[tokio::test]
    async fn rare_closed_by_group_vanish_above_suggested() {
        let (db, _dir) = temp_db();
        ghost(&db, "r1", Some("g"), 1000, 2000, Some(1000)).await;
        ghost(&db, "r2", Some("h"), 1000, 2000, Some(1000)).await;
        // r1: anúncio do mesmo grupo a R$ 21 sumiu no dia 8 sem relistagem
        exec(
            &db,
            "INSERT INTO observations(listing_id, name, query, grp, price_cents, first_seen, last_seen, cheapest, gone_at)
             VALUES ('o', ?1, 'q', 'g', 2100, ?2, ?2, 1, ?3)",
            vec![v(NAME.to_string()), v(T), v(T + 8 * DAY)],
        )
        .await;
        // r2: só venda do CSFloat a 1,9× (sugerido 2×) e a mediana comum dos dias 7–9
        exec(
            &db,
            "INSERT INTO csfloat_sales VALUES (?1, ?2, 190, 100, 1.9, 1, 0.2, NULL, 'h')",
            vec![v(NAME.to_string()), v(T + 8 * DAY)],
        )
        .await;
        point(&db, T + 8 * DAY, 1100, 5).await;
        assert_eq!(run(&db, T + 13 * DAY).await.done, 0, "raro espera o dia 14");
        assert_eq!(run(&db, T + 14 * DAY).await.done, 2);
        let r1 = fin(&db, "r1").await;
        assert_eq!((r1.1, r1.3), (Some(2000), Some(true)));
        let r2 = fin(&db, "r2").await;
        assert_eq!(
            (r2.1, r2.2, r2.3),
            (Some(1100), Some(seller_gets(1100) - 1000), Some(false))
        );
        // venda do CSFloat a 2× fecharia
        exec(
            &db,
            "UPDATE csfloat_sales SET multiple = 2.0 WHERE grp = 'h'",
            vec![],
        )
        .await;
        let n = db
            .call(|c| {
                let r = Row {
                    id: "r2".into(),
                    name: NAME.into(),
                    grp: Some("h".into()),
                    bought: T,
                    price: 1000,
                    mid: 2000,
                    base: Some(1000),
                };
                rare_closed(c, &r, T + SELL_FROM, T + RARE_UNTIL)
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn refit_waits_for_labels_and_limits_step() {
        // pouco rótulo ou pouco tempo: não mexe
        assert_eq!(refit(1.0, 1.0, Some(0.85), 29, 10), 1.0);
        assert_eq!(refit(1.0, 1.0, Some(0.85), 40, 6), 1.0);
        assert_eq!(refit(1.0, 1.0, None, 40, 10), 1.0);
        // o mercado pagou 15 % abaixo: desce 2 % por dia até o alvo
        assert_eq!(refit(1.0, 1.0, Some(0.85), 40, 10), 0.98);
        assert_eq!(refit(0.98, 0.98, Some(0.87), 40, 11), 0.96);
        // o alvo é o fator aplicado × real ÷ previsto: já descontado e batendo, fica
        assert_eq!(refit(0.9, 0.9, Some(1.0), 40, 20), 0.9);
        // nunca abaixo de 0,80, nunca acima de 1,00
        assert_eq!(refit(0.81, 0.81, Some(0.5), 40, 30), 0.8);
        assert_eq!(refit(0.99, 0.99, Some(1.3), 40, 30), 1.0);
        // as vendas do Erick já descontam mais: o alvo parte do fator aplicado
        assert_eq!(refit(1.0, 0.9, Some(1.0), 40, 10), 0.98);
    }

    #[tokio::test]
    async fn summary_splits_alerts_from_near_misses() {
        let (db, _dir) = temp_db();
        ghost(&db, "a", None, 800, 1000, None).await;
        ghost(&db, "b", None, 800, 1000, None).await;
        exec(
            &db,
            "UPDATE ghosts SET approved = 0 WHERE listing_id = 'b'",
            vec![],
        )
        .await;
        point(&db, T + 3600, 1100, 3).await;
        point(&db, T + 7 * DAY + 3600, 1000, 5).await;
        run(&db, T + FAST).await;
        run(&db, T + 10 * DAY).await;
        let s = db.call(|c| summary(c)).await.unwrap();
        let won = seller_gets(1000) - 800;
        assert!((s.fast_ratio.unwrap() - 1.1).abs() < 1e-9, "{s:?}");
        assert_eq!(
            s,
            Summary {
                ghosts: 2,
                fast_alerts: 1,
                fast_alerts_closed: 1,
                fast_near: 1,
                fast_near_closed: 1,
                fast_ratio: s.fast_ratio,
                final_alerts: 1,
                final_alerts_won: 1,
                profit_cents: won,
                predicted_cents: won,
                ..Summary::default()
            }
        );
        // boletim: só o aviso conta; a semana é [de, até)
        exec(
            &db,
            "INSERT INTO hourly_activity(hour_utc, queries, near, opportunities) VALUES (?1, 10, 2, 1)",
            vec![v(T / HOUR + 1)],
        )
        .await;
        let w = db.call(|c| week(c, T, T + 11 * DAY)).await.unwrap();
        assert_eq!(
            w,
            Week {
                queries: 10,
                finds: 1,
                near: 2,
                alerts: 1,
                alerts_closed: 1,
                labeled: 1,
                profit_cents: won,
            }
        );
        // a hora em curso conta (ela começou dentro da semana)
        let start = (T / HOUR + 1) * HOUR;
        let partial = db.call(move |c| week(c, T, start + 1)).await.unwrap();
        assert_eq!(partial.queries, 10);
        let empty = db
            .call(|c| week(c, T + 11 * DAY, T + 18 * DAY))
            .await
            .unwrap();
        assert_eq!(empty, Week::default());
        assert_eq!(db.call(|c| refit_factor(c)).await.unwrap(), 1.0);
    }
}
