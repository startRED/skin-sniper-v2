//! Livro de pedidos: `GET /market/orderbook?q=Load&qp=[730,"<nome>"]` (pesquisa-web 2.2). Só o topo importa:
//! um pedido de compra 1 centavo acima do maior fica na frente da fila.

use serde_json::{Value, json};

use super::client::{CURRENCY_BRL, Endpoint, Error, Result, SteamClient};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Book {
    /// Maior pedido de compra em centavos; `None` sem nenhum pedido aberto.
    pub max_buy_cents: Option<i64>,
    pub buy_orders: i64,
    /// Menor anúncio à venda em centavos; `None` sem anúncio.
    pub min_sell_cents: Option<i64>,
}

pub async fn fetch_book(c: &SteamClient, market_hash_name: &str) -> Result<Book> {
    let data = c
        .action_at(
            Endpoint::Book,
            "/market/orderbook",
            "Load",
            &json!([730, market_hash_name]),
        )
        .await?;
    parse_book(&data)
}

/// `data` = `{"success":true,"data":{"amtMaxBuyOrder":158,"eCurrency":7,"cBuyOrders":12556,…}}`, valores em centavos.
pub fn parse_book(data: &Value) -> Result<Book> {
    if data.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Parse("orderbook: success ausente ou falso".into()));
    }
    let d = &data["data"];
    let cur = d
        .get("eCurrency")
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Parse("orderbook: eCurrency ausente".into()))?;
    if cur != i64::from(CURRENCY_BRL) {
        return Err(Error::WrongCurrency { got: cur as i32 });
    }
    let count = |k: &str| {
        d.get(k)
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::Parse(format!("orderbook: {k} ausente")))
    };
    let buy_orders = count("cBuyOrders")?;
    let max_buy_cents = d
        .get("amtMaxBuyOrder")
        .and_then(Value::as_i64)
        .filter(|&c| c > 0 && buy_orders > 0);
    let min_sell_cents = d
        .get("amtMinSellOrder")
        .and_then(Value::as_i64)
        .filter(|&c| c > 0);
    // com anúncios à venda, o menor preço tem de vir: sem ele um pedido acima do anúncio passaria
    if count("cSellOrders")? > 0 && min_sell_cents.is_none() {
        return Err(Error::Parse(
            "orderbook: anúncios sem amtMinSellOrder".into(),
        ));
    }
    Ok(Book {
        max_buy_cents,
        buy_orders,
        min_sell_cents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_capture_top_of_book() {
        let s = std::fs::read_to_string(format!(
            "{}/testdata/steam_orderbook_negev_2026-11.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        let b = parse_book(&v["data"]).unwrap();
        assert_eq!(b.max_buy_cents, Some(158));
        assert_eq!((b.buy_orders, b.min_sell_cents), (12556, Some(167)));
    }

    #[test]
    fn empty_wrong_currency_and_failure() {
        let ok = |d: Value| parse_book(&json!({"success": true, "data": d}));
        let none =
            ok(json!({"amtMaxBuyOrder": 0, "eCurrency": 7, "cBuyOrders": 0, "cSellOrders": 0}))
                .unwrap();
        assert_eq!((none.max_buy_cents, none.min_sell_cents), (None, None));
        assert!(matches!(
            ok(json!({"amtMaxBuyOrder": 10, "eCurrency": 1, "cBuyOrders": 3})),
            Err(Error::WrongCurrency { got: 1 })
        ));
        assert!(parse_book(&json!({"success": false})).is_err());
        assert!(
            ok(json!({"amtMaxBuyOrder": 10, "eCurrency": 7, "cBuyOrders": 3, "cSellOrders": 2}))
                .is_err()
        );
        // contagem ausente é erro, não zero
        assert!(ok(json!({"amtMaxBuyOrder": 10, "eCurrency": 7, "cBuyOrders": 3})).is_err());
    }
}
