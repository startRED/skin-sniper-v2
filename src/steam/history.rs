//! `QueryPriceHistory` → pontos `(ts, mediana em centavos, compras)` (conhecimento 1.6; pesquisa-web 2.2).
#![allow(dead_code)]

use serde_json::{Value, json};

use super::client::{CURRENCY_BRL, Endpoint, Error, Result, SteamClient};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PricePoint {
    /// Segundos Unix.
    pub ts: i64,
    pub median_cents: i64,
    pub purchases: u32,
}

/// Histórico da skin comum do nome (logado quando há cookie; funciona sem). Mede o preço da skin, nunca o prêmio de um exemplar raro.
pub async fn fetch_history(c: &SteamClient, market_hash_name: &str) -> Result<Vec<PricePoint>> {
    let result = c
        .action(
            Endpoint::History,
            "QueryPriceHistory",
            &json!([730, market_hash_name]),
        )
        .await;
    match result {
        Ok(data) => parse_history(&data),
        Err(
            Error::RateLimited { .. } | Error::BreakerOpen { .. } | Error::ActionUnavailable { .. },
        ) if c.cookie().is_some() => {
            let q = [
                ("appid", "730".into()),
                ("market_hash_name", market_hash_name.into()),
            ];
            let data = c.legacy_json("/market/pricehistory/", &q, true).await?;
            super::legacy::parse_history(&data)
        }
        Err(e) => Err(e),
    }
}

/// `price_median` chega como float em BRL (ex.: `5.855159282684326`, um float32 promovido). Vira centavos
/// por `round()` (meio para longe do zero): 5.855… → 586. Truncar erra por 1 centavo (`0.29` vira 28).
/// Descartados: ts ≤ 0, mediana não finita ou que arredonda para ≤ 0, `purchases` ausente/negativo.
/// A saída vem ordenada por `ts` ascendente, sem repetir `ts` (vale o último).
pub fn parse_history(data: &Value) -> Result<Vec<PricePoint>> {
    let cur = data
        .get("ecurrency")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Parse("ecurrency ausente".into()))?;
    if cur != i64::from(CURRENCY_BRL) {
        return Err(Error::WrongCurrency { got: cur as i32 });
    }
    let prices = data
        .get("prices")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("prices ausente".into()))?;
    let mut out: Vec<PricePoint> = prices.iter().filter_map(point).collect();
    out.sort_by_key(|p| p.ts);
    out.dedup_by(|b, a| {
        let same = a.ts == b.ts;
        if same {
            *a = *b;
        }
        same
    });
    if out.len() < prices.len() {
        tracing::debug!(
            dropped = prices.len() - out.len(),
            "histórico: pontos inválidos descartados"
        );
    }
    Ok(out)
}

fn point(p: &Value) -> Option<PricePoint> {
    let ts = p.get("time").and_then(Value::as_i64).filter(|t| *t > 0)?;
    let median = p
        .get("price_median")
        .and_then(Value::as_f64)
        .filter(|m| m.is_finite() && *m < 1e9)?;
    let median_cents = (median * 100.0).round() as i64;
    let purchases = u32::try_from(p.get("purchases").and_then(Value::as_i64)?).ok()?;
    (median_cents > 0).then_some(PricePoint {
        ts,
        median_cents,
        purchases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_capture() {
        let s = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/steam_pricehistory_moonrise_ft.json"
        ))
        .unwrap();
        let data = serde_json::from_str::<Value>(&s).unwrap()["data"].take();
        let pts = parse_history(&data).unwrap();
        assert_eq!(pts.len(), 304); // 4 diários antigos + 300 horários recentes da captura (recortada)
        assert!(pts.windows(2).all(|w| w[0].ts < w[1].ts));
        let last = pts.last().unwrap();
        assert_eq!(last.ts, 1793754000); // price_median 6.038858890533447, purchases 8
        assert_eq!((last.median_cents, last.purchases), (604, 8));
        assert_eq!(pts[0].ts, 1518739200); // 48.70029830932617 → R$ 48,70
        assert_eq!(pts[0].median_cents, 4870);
    }

    #[test]
    fn rounds_not_truncates_and_drops_invalid() {
        let data = json!({"ecurrency": 7, "prices": [
            {"time": 30, "price_median": 0.29, "purchases": 4},
            {"time": 10, "price_median": 5.9996, "purchases": 3},
            {"time": 20, "price_median": 0.001, "purchases": 3},
            {"time": 0, "price_median": 1.0, "purchases": 3},
            {"time": 40, "price_median": -1.0, "purchases": 3},
            {"time": 50, "price_median": 1.0, "purchases": -2},
            {"time": 60, "price_median": 1.0},
            {"time": 70, "purchases": 1},
            {"time": 30, "price_median": 0.31, "purchases": 5},
        ]});
        let pts = parse_history(&data).unwrap();
        assert_eq!(
            pts,
            vec![
                PricePoint {
                    ts: 10,
                    median_cents: 600,
                    purchases: 3
                },
                PricePoint {
                    ts: 30,
                    median_cents: 31,
                    purchases: 5
                }, // repetido: vale o último
            ]
        );
        // 0.29 não vira 28
        assert_eq!(
            point(&json!({"time": 1, "price_median": 0.29, "purchases": 1}))
                .unwrap()
                .median_cents,
            29
        );
    }

    #[test]
    fn wrong_currency_or_shape_is_error() {
        assert!(matches!(
            parse_history(&json!({"ecurrency": 1, "prices": []})),
            Err(Error::WrongCurrency { got: 1 })
        ));
        assert!(parse_history(&json!({"prices": []})).is_err());
        assert!(parse_history(&json!({"ecurrency": 7})).is_err());
        assert!(
            parse_history(&json!({"ecurrency": 7, "prices": []}))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    #[ignore = "ao vivo: 1 consulta à Steam"]
    async fn live_history() {
        let c = SteamClient::new().unwrap();
        let pts = fetch_history(&c, "Glock-18 | Moonrise (Field-Tested)")
            .await
            .unwrap();
        assert!(pts.len() > 100);
    }
}
