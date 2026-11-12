//! Caça no CSFloat do jeito dos snipers: em vez de consultar skin por skin, lê os anúncios "compre já" mais
//! novos do site inteiro (`/listings?sort_by=most_recent`) e compara cada um com o preço previsto do próprio
//! CSFloat (`reference.predicted_price`, que já conta float e pattern). Em 12/11/2026, 50 anúncios cobriam ~35 s
//! e a cota da chave era 200 consultas por janela de 1 h: uma leitura a cada 20 s vê todo anúncio novo.
//!
//! Só avisa e grava (`csfloat_deals`, `csfloat_feed`): comprar exige saldo no CSFloat, decisão do Erick depois de
//! medir quantos achados aparecem e quanto tempo duram.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use serde_json::Value;

use crate::csfloat::CsfloatClient;
use crate::db::Db;
use crate::settings::SettingsStore;
use crate::steam::client::Error;

/// 180 leituras/h, abaixo das 200 da janela.
const EVERY: Duration = Duration::from_secs(20);
const PAGE: u32 = 50;
/// Taxa de venda do CSFloat.
const SELL_FEE: f64 = 0.02;
/// Lucro mínimo revendendo no CSFloat pelo previsto: US$ 0,50 e 10 % do preço.
// shortcut: régua igual à da Steam (R$ 1 / 10 %) convertida a olho; ajustar pelos achados gravados.
const MIN_PROFIT_CENTS: i64 = 50;
const MIN_MARGIN: f64 = 0.10;
/// Abaixo de US$ 1 a previsão é ruído de centavos.
const MIN_PRICE_CENTS: i64 = 100;
/// Previsão feita com poucos anúncios da skin não é referência.
const MIN_QUANTITY: i64 = 10;
/// Previsto acima disto × o preço-base: o lucro depende do prêmio de float/pattern que o CSFloat estimou.
const PREMIUM_FLAG: f64 = 1.2;
const SEEN_MAX: usize = 2000;
/// Conferência do achado: ainda à venda depois deste tempo = dava para comprar na mão.
const RECHECK: Duration = Duration::from_secs(60);
const REPORT_SECS: i64 = 600;

#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub id: String,
    pub created_at: i64,
    pub price: i64,
    pub predicted: i64,
    pub base: i64,
    pub quantity: i64,
    pub name: String,
    pub float: Option<f64>,
    pub seed: Option<i64>,
    pub stickers: usize,
}

impl Listing {
    /// Centavos de USD que sobram revendendo no CSFloat pelo preço previsto.
    pub fn profit(&self) -> i64 {
        (self.predicted as f64 * (1.0 - SELL_FEE)).floor() as i64 - self.price
    }

    pub fn is_deal(&self) -> bool {
        let min = MIN_PROFIT_CENTS.max((self.price as f64 * MIN_MARGIN).ceil() as i64);
        self.price >= MIN_PRICE_CENTS
            && self.quantity >= MIN_QUANTITY
            && self.base > 0
            && self.profit() >= min
    }

    /// O lucro vem de prêmio de float/pattern estimado, não do preço da skin comum.
    pub fn premium(&self) -> bool {
        self.predicted as f64 > self.base as f64 * PREMIUM_FLAG
    }

    pub fn url(&self) -> String {
        format!("https://csfloat.com/item/{}", self.id)
    }
}

/// `{"data":[...]}` (ou a lista crua) → anúncios com preço e previsão; o resto é ignorado.
pub fn parse(v: &Value) -> Vec<Listing> {
    let arr = v
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    arr.into_iter()
        .flatten()
        .filter_map(|x| {
            let item = x.get("item")?;
            let num = |p: &str| x.pointer(p).and_then(Value::as_i64);
            Some(Listing {
                id: x.get("id")?.as_str()?.to_string(),
                created_at: x
                    .get("created_at")
                    .and_then(Value::as_str)
                    .and_then(crate::csfloat::parse_rfc3339)
                    .unwrap_or(0),
                price: num("/price").filter(|p| *p > 0)?,
                predicted: num("/reference/predicted_price").filter(|p| *p > 0)?,
                base: num("/reference/base_price").unwrap_or(0),
                quantity: num("/reference/quantity").unwrap_or(0),
                name: item.get("market_hash_name")?.as_str()?.to_string(),
                float: item.get("float_value").and_then(Value::as_f64),
                seed: item.get("paint_seed").and_then(Value::as_i64),
                stickers: item
                    .get("stickers")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            })
        })
        .collect()
}

fn now() -> i64 {
    crate::db::now_unix()
}

async fn record_deal(db: &Db, l: &Listing) {
    let l = l.clone();
    let r = db
        .call(move |c| {
            c.execute(
                r#"INSERT OR IGNORE INTO csfloat_deals (id, at, created_at, name, price_cents, predicted_cents,
                   base_cents, quantity, "float", seed, stickers, profit_cents)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"#,
                rusqlite::params![
                    l.id,
                    now(),
                    l.created_at,
                    l.name,
                    l.price,
                    l.predicted,
                    l.base,
                    l.quantity,
                    l.float,
                    l.seed,
                    l.stickers as i64,
                    l.profit()
                ],
            )
        })
        .await;
    if let Err(e) = r {
        tracing::warn!("achado do CSFloat não gravou: {e:#}");
    }
}

/// Um minuto depois do aviso: o anúncio seguia à venda? Mede se dá para comprar na mão.
async fn recheck(db: Db, client: std::sync::Arc<CsfloatClient>, id: String) {
    tokio::time::sleep(RECHECK).await;
    let state = match client.listing(&id).await {
        Ok(v) => v
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
        // 404 = saiu do ar (vendido ou retirado)
        Err(Error::Csfloat { status: 404 }) => "gone".to_string(),
        Err(e) => {
            tracing::warn!("conferência do achado {id} no CSFloat falhou: {e}");
            return;
        }
    };
    tracing::info!("CSFloat: achado {id} depois de 60 s: {state}");
    let _ = db
        .call(move |c| {
            c.execute(
                "UPDATE csfloat_deals SET state_60s = ?2 WHERE id = ?1",
                rusqlite::params![id, state],
            )
        })
        .await;
}

pub async fn run(db: Db, settings: SettingsStore) {
    let client = match CsfloatClient::new() {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => {
            tracing::error!("caça no CSFloat não sobe: {e:#}");
            return;
        }
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut order: VecDeque<String> = VecDeque::new();
    let mut newest: Option<i64> = None;
    let (mut pages, mut fresh_total, mut gaps, mut deals) = (0u32, 0u32, 0u32, 0u32);
    let mut report_at = now();
    let mut warned_no_key = false;
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let key = settings.get("csfloat_key").await.ok().flatten();
        if key.is_none() {
            if !warned_no_key {
                tracing::warn!("caça no CSFloat parada: sem chave do CSFloat nas configurações");
                warned_no_key = true;
            }
            continue;
        }
        warned_no_key = false;
        client.set_key(key);
        let page = match client.recent_listings(PAGE).await {
            Ok(v) => parse(&v),
            Err(Error::RateLimited { until }) => {
                let wait = (until - now()).clamp(1, 3600) as u64;
                tracing::warn!("CSFloat: cota esgotada, esperando {wait} s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            Err(e) => {
                tracing::warn!("CSFloat: leitura do feed falhou: {e}");
                continue;
            }
        };
        pages += 1;
        // Página inteira mais nova que o último visto = anúncios que passaram sem ser lidos.
        let oldest = page.iter().map(|l| l.created_at).min();
        let gap = matches!((newest, oldest), (Some(n), Some(o)) if o > n);
        gaps += u32::from(gap);
        newest = newest.max(page.iter().map(|l| l.created_at).max());
        let (mut fresh, mut found) = (0u32, 0u32);
        for l in page {
            if !seen.insert(l.id.clone()) {
                continue;
            }
            order.push_back(l.id.clone());
            if order.len() > SEEN_MAX
                && let Some(old) = order.pop_front()
            {
                seen.remove(&old);
            }
            fresh += 1;
            if !l.is_deal() {
                continue;
            }
            found += 1;
            let usd = |c: i64| format!("US$ {:.2}", c as f64 / 100.0);
            let why = if l.premium() {
                "prêmio de float/pattern previsto"
            } else {
                "abaixo do preço da skin"
            };
            tracing::info!(
                "CSFloat: achado {} a {} (previsto {}, base {}, lucro {}; {why}) float={:?} seed={:?} {}",
                l.name,
                usd(l.price),
                usd(l.predicted),
                usd(l.base),
                usd(l.profit()),
                l.float,
                l.seed,
                l.url()
            );
            record_deal(&db, &l).await;
            let body = format!(
                "{} · lucro {} revendendo no CSFloat ({why})",
                usd(l.price),
                usd(l.profit())
            );
            if let Err(e) = crate::notify::desktop(&l.name, &body, &l.url()).await {
                tracing::warn!("aviso do achado do CSFloat falhou: {e}");
            }
            tokio::spawn(recheck(db.clone(), client.clone(), l.id.clone()));
        }
        fresh_total += fresh;
        deals += found;
        let at = now();
        let _ = db
            .call(move |c| {
                c.execute(
                    "INSERT INTO csfloat_feed (at, fresh, gap, deals) VALUES (?1,?2,?3,?4)",
                    rusqlite::params![at, fresh, gap, found],
                )
            })
            .await;
        if at - report_at >= REPORT_SECS {
            let _ = db
                .call(move |c| {
                    c.execute("DELETE FROM csfloat_feed WHERE at < ?1", [at - 30 * 86_400])
                })
                .await;
            tracing::info!(
                "CSFloat em 10 min: leituras={pages} anúncios novos={fresh_total} buracos={gaps} achados={deals} cota={:?}",
                client.rate_limit().remaining
            );
            (pages, fresh_total, gaps, deals, report_at) = (0, 0, 0, 0, at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(price: i64, predicted: i64, base: i64, quantity: i64) -> Value {
        serde_json::json!({
            "id": "1", "created_at": "2026-11-12T18:10:19.083074Z", "price": price, "type": "buy_now",
            "reference": {"base_price": base, "predicted_price": predicted, "quantity": quantity},
            "item": {"market_hash_name": "AK-47 | Redline (Field-Tested)", "float_value": 0.16, "paint_seed": 661,
                     "stickers": [{"name": "x"}]}
        })
    }

    #[test]
    fn parses_the_feed_and_flags_only_real_discounts() {
        let v = serde_json::json!({"data": [
            listing(1000, 1300, 1300, 150),
            listing(1000, 1100, 1100, 150),
            listing(1000, 1300, 1300, 3),
            listing(80, 300, 300, 150),
            listing(1000, 1600, 1000, 150),
            {"id": "sem previsão", "price": 100, "item": {"market_hash_name": "x"}}
        ]});
        let ls = parse(&v);
        assert_eq!(ls.len(), 5);
        let l = &ls[0];
        assert_eq!(
            (l.created_at > 0, l.seed, l.stickers, l.name.as_str()),
            (true, Some(661), 1, "AK-47 | Redline (Field-Tested)")
        );
        // 1300 × 0,98 − 1000 = 274 ≥ 100 (10 %)
        assert_eq!(l.profit(), 274);
        assert!(l.is_deal() && !l.premium());
        // 1100 × 0,98 − 1000 = 78 < 100
        assert!(!ls[1].is_deal());
        // previsão com 3 anúncios da skin
        assert!(!ls[2].is_deal());
        // abaixo de US$ 1
        assert!(!ls[3].is_deal());
        // lucro só pelo prêmio previsto: passa, marcado
        assert!(ls[4].is_deal() && ls[4].premium());
        assert_eq!(ls[0].url(), "https://csfloat.com/item/1");
    }
}
