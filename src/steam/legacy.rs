//! Leitura do formato antigo de `/render`, com preços convertidos para BRL.

use serde_json::{Value, json};

use super::client::{Error, Result};
use super::history::{self, PricePoint};
use super::listings::{self, ListingsPage};

fn bad(message: &str) -> Error {
    Error::Parse(format!("render: {message}"))
}

/// Histórico legado: só aceita a moeda e o fuso explicitamente informados pela Steam.
pub fn parse_history(data: &Value) -> Result<Vec<PricePoint>> {
    if data.get("success").and_then(Value::as_bool) != Some(true)
        || data
            .get("price_prefix")
            .and_then(Value::as_str)
            .map(str::trim)
            != Some("R$")
        || data
            .get("price_suffix")
            .and_then(Value::as_str)
            .map(str::trim)
            != Some("")
    {
        return Err(bad("histórico sem sucesso ou moeda BRL"));
    }
    let rows = data
        .get("prices")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("histórico sem prices"))?;
    let mut prices = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row
            .as_array()
            .filter(|r| r.len() == 3)
            .ok_or_else(|| bad("linha de histórico inválida"))?;
        let date = row[0]
            .as_str()
            .and_then(|s| s.strip_suffix(": +0"))
            .ok_or_else(|| bad("fuso do histórico inválido"))?;
        let (date, hour) = date
            .rsplit_once(' ')
            .ok_or_else(|| bad("data do histórico inválida"))?;
        let date = chrono::NaiveDate::parse_from_str(date, "%b %e %Y")
            .map_err(|_| bad("data do histórico inválida"))?;
        let hour = hour
            .parse::<u32>()
            .map_err(|_| bad("hora do histórico inválida"))?;
        let ts = date
            .and_hms_opt(hour, 0, 0)
            .ok_or_else(|| bad("hora do histórico inválida"))?
            .and_utc()
            .timestamp();
        let price = row[1]
            .as_f64()
            .filter(|p| p.is_finite() && *p >= 0.005 && *p < 1e9)
            .ok_or_else(|| bad("preço do histórico inválido"))?;
        let purchases = row[2]
            .as_str()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| bad("quantidade do histórico inválida"))?;
        if ts <= 0 {
            return Err(bad("data do histórico inválida"));
        }
        prices.push(json!({"time": ts, "price_median": price, "purchases": purchases}));
    }
    history::parse_history(&json!({"ecurrency": 7, "prices": prices}))
}

/// Converte uma página inteira; qualquer inconsistência invalida todos os anúncios.
pub fn parse_page(
    data: &Value,
    expected_name: &str,
    expected_code: &str,
    expected_start: u32,
) -> Result<ListingsPage> {
    if !listings::is_item_code(expected_code) || expected_name.is_empty() {
        return Err(bad("identidade esperada inválida"));
    }
    if data.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(bad("resposta sem sucesso"));
    }
    let start = data
        .get("start")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("start ausente"))?;
    let total = data
        .get("total_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("total_count ausente"))?;
    let size = data
        .get("pagesize")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| bad("pagesize inválido"))?;
    let raw = data
        .get("listinginfo")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("listinginfo ausente"))?;
    let count = raw.len() as u64;
    if start != u64::from(expected_start)
        || count > size
        || start.checked_add(count).is_none_or(|end| end > total)
        || (total > 0 && count == 0)
    {
        return Err(bad("paginação inconsistente"));
    }
    let mut converted = Vec::with_capacity(raw.len());
    for (id, item) in raw {
        if id.is_empty() || item.get("listingid").and_then(Value::as_str) != Some(id) {
            return Err(bad("listingid inconsistente"));
        }
        if item.get("converted_currencyid").and_then(Value::as_i64) != Some(2007) {
            return Err(bad("moeda convertida não é BRL"));
        }
        let price = item
            .get("converted_price")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or_else(|| bad("preço convertido inválido"))?;
        let fee = item
            .get("converted_fee")
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0)
            .ok_or_else(|| bad("taxa convertida inválida"))?;
        price
            .checked_add(fee)
            .ok_or_else(|| bad("preço excede limite"))?;
        let reference = item.get("asset").ok_or_else(|| bad("asset ausente"))?;
        let asset_id = reference
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad("asset id ausente"))?;
        if reference.get("appid").and_then(Value::as_u64) != Some(730)
            || reference.get("contextid").and_then(Value::as_str) != Some("2")
        {
            return Err(bad("app ou contexto inválido"));
        }
        let mut asset = data
            .pointer("/assets/730/2")
            .and_then(|v| v.get(asset_id))
            .cloned()
            .ok_or_else(|| bad("asset não encontrado"))?;
        if asset.get("id").and_then(Value::as_str) != Some(asset_id)
            || asset.get("appid").and_then(Value::as_u64) != Some(730)
            || asset.get("contextid").and_then(Value::as_str) != Some("2")
            || asset.get("market_hash_name").and_then(Value::as_str) != Some(expected_name)
            || asset.get("market_bucket_group_id").and_then(Value::as_str) != Some(expected_code)
        {
            return Err(bad("identidade do asset inconsistente"));
        }
        if let Some(props) = asset
            .get_mut("asset_properties")
            .and_then(Value::as_array_mut)
        {
            for prop in props {
                if let Some(number) = prop
                    .get("float_value")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<f64>().ok())
                    .and_then(serde_json::Number::from_f64)
                {
                    prop["float_value"] = Value::Number(number);
                }
            }
        }
        converted.push(json!({"listingid": id, "unPrice": price, "unFee": fee, "eCurrency": 7, "asset": asset, "description": asset}));
    }
    let mut page = listings::parse_page(
        &json!({"start": start, "total_count": total, "more": start + count < total, "listings": converted}),
        expected_start,
    )?;
    page.listings.sort_by_key(|l| l.price_cents);
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "Glock-18 | Moonrise (Field-Tested)";
    const CODE: &str = "G180420B6053004";
    const ID: &str = "5074569024067185966";

    fn fixture() -> Value {
        // Captura real: apenas três anúncios e seus assets; total e tamanho da página originais.
        serde_json::from_str(include_str!(
            "../../testdata/steam_legacy_render_moonrise_2026-11.json"
        ))
        .unwrap()
    }

    #[test]
    fn real_history_and_invalid_rows() {
        // Captura real: três pontos iniciais e três finais, com valores originais.
        let v: Value = serde_json::from_str(include_str!(
            "../../testdata/steam_legacy_history_moonrise_2026-11.json"
        ))
        .unwrap();
        let points = parse_history(&v).unwrap();
        assert_eq!(points.len(), 6);
        assert_eq!(
            (points[0].ts, points[0].median_cents, points[0].purchases),
            (1_518_742_800, 4819, 522)
        );
        assert_eq!(
            (points[5].ts, points[5].median_cents, points[5].purchases),
            (1_793_959_200, 575, 9)
        );
        let check = |v: &Value| assert!(parse_history(v).is_err());
        let mut x = v.clone();
        x["price_prefix"] = json!("$");
        check(&x);
        let mut x = v.clone();
        x["price_suffix"] = json!("USD");
        check(&x);
        let mut x = v.clone();
        x["prices"][0][0] = json!("Feb 16 2018 01: +1");
        check(&x);
        let mut x = v.clone();
        x["prices"][0][0] = json!("invalid: +0");
        check(&x);
        let mut x = v.clone();
        x["prices"][0][1] = json!("48.186");
        check(&x);
        let mut x = v.clone();
        x["prices"][0][2] = json!("-1");
        check(&x);
        let mut x = v.clone();
        x["prices"][0] = json!([]);
        check(&x);
    }

    #[test]
    fn real_converted_price_and_properties() {
        let v = fixture();
        assert_eq!(v["listinginfo"][ID]["currencyid"], 2006);
        let page = parse_page(&v, NAME, CODE, 0).unwrap();
        assert!(page.more);
        assert!(
            page.listings
                .windows(2)
                .all(|w| w[0].price_cents <= w[1].price_cents)
        );
        let l = page.listings.iter().find(|l| l.listing_id == ID).unwrap();
        assert_eq!((l.seller_cents, l.fee_cents, l.price_cents), (461, 69, 530));
        assert_eq!(l.seed, Some(773));
        assert_eq!(l.float, Some(0.306_838_929_653_167_7_f64));
        assert!(l.inspect_url.as_deref().is_some_and(|url| !url.is_empty()));
    }

    #[test]
    fn rejects_inconsistent_pages() {
        let v = fixture();
        let check = |v: &Value| assert!(parse_page(v, NAME, CODE, 0).is_err());
        let mut x = v.clone();
        x["success"] = json!(false);
        check(&x);
        let mut x = v.clone();
        x["start"] = json!(1);
        check(&x);
        let mut x = v.clone();
        x["pagesize"] = json!(2);
        check(&x);
        let mut x = v.clone();
        x["listinginfo"] = json!({});
        check(&x);
        let mut x = v.clone();
        x["listinginfo"][ID]["converted_currencyid"] = json!(2006);
        check(&x);
        let mut x = v.clone();
        x["listinginfo"][ID]
            .as_object_mut()
            .unwrap()
            .remove("converted_fee");
        check(&x);
        let mut x = v.clone();
        x["listinginfo"][ID]["converted_price"] = json!(i64::MAX);
        check(&x);
        let mut x = v.clone();
        x["assets"]["730"]["2"].as_object_mut().unwrap().clear();
        check(&x);
        let mut x = v.clone();
        x["assets"]["730"]["2"]["54241046707"]["market_hash_name"] = json!("wrong");
        check(&x);
        let mut x = v.clone();
        x["assets"]["730"]["2"]["54241046707"]["market_bucket_group_id"] = json!("G1234");
        check(&x);
        let mut x = v.clone();
        x["listinginfo"][ID]["asset"]["contextid"] = json!("3");
        check(&x);
        assert!(parse_page(&v, NAME, "G1234", 0).is_err());
    }
}
