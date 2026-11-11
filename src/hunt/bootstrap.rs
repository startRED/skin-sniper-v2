//! Catálogo público: metadados e preço de referência, sem inventar medições da Steam.

use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;

use crate::db::{Db, now_unix};
use crate::steam::listings::{is_item_code, paint_index_from_code};

pub const SCHEMA: &str = "CREATE TABLE catalog_references (
    name TEXT PRIMARY KEY, usd_cents INTEGER, observed_at INTEGER NOT NULL, rank INTEGER NOT NULL
);";

/// Preço público dos chaveiros que venderam na última semana (`detect::charm_gain`).
pub const CHARM_SCHEMA: &str = "CREATE TABLE charm_prices (
    name TEXT PRIMARY KEY, usd_cents INTEGER NOT NULL, observed_at INTEGER NOT NULL
);";

#[derive(Deserialize)]
struct Catalog {
    version: u32,
    generated_at: i64,
    reference_observed_at: Option<i64>,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    name: String,
    code: String,
    paint_index: u32,
    reference_usd_cents: Option<i64>,
    rank: i64,
    icon_url: String,
}

fn weapon_name(name: &str) -> bool {
    let name = name
        .strip_prefix("StatTrak™ ")
        .or_else(|| name.strip_prefix("Souvenir "))
        .unwrap_or(name);
    let Some((skin, wear)) = name.rsplit_once(" (") else {
        return false;
    };
    let Some((weapon, finish)) = skin.split_once(" | ") else {
        return false;
    };
    !finish.trim().is_empty()
        && matches!(
            wear,
            "Factory New)" | "Minimal Wear)" | "Field-Tested)" | "Well-Worn)" | "Battle-Scarred)"
        )
        && matches!(
            weapon,
            "AK-47"
                | "AUG"
                | "AWP"
                | "CZ75-Auto"
                | "Desert Eagle"
                | "Dual Berettas"
                | "FAMAS"
                | "Five-SeveN"
                | "G3SG1"
                | "Galil AR"
                | "Glock-18"
                | "M249"
                | "M4A1-S"
                | "M4A4"
                | "MAC-10"
                | "MAG-7"
                | "MP5-SD"
                | "MP7"
                | "MP9"
                | "Negev"
                | "Nova"
                | "P2000"
                | "P250"
                | "P90"
                | "PP-Bizon"
                | "R8 Revolver"
                | "Sawed-Off"
                | "SCAR-20"
                | "SG 553"
                | "SSG 08"
                | "Tec-9"
                | "UMP-45"
                | "USP-S"
                | "XM1014"
                | "Zeus x27"
        )
}

fn parse(json: &str, now: i64) -> Result<Catalog> {
    let catalog: Catalog = serde_json::from_str(json).context("catálogo público inválido")?;
    ensure!(catalog.version == 1, "versão do catálogo público inválida");
    ensure!(!catalog.items.is_empty(), "catálogo público vazio");
    ensure!(
        catalog.generated_at > 0 && catalog.generated_at <= now,
        "data do catálogo público inválida"
    );
    let reference_at = catalog
        .reference_observed_at
        .unwrap_or(catalog.generated_at);
    ensure!(
        reference_at > 0 && reference_at <= catalog.generated_at,
        "data da referência pública inválida"
    );
    let mut names = HashSet::new();
    for item in &catalog.items {
        ensure!(
            weapon_name(&item.name),
            "nome inválido no catálogo: {}",
            item.name
        );
        ensure!(
            names.insert(&item.name),
            "nome duplicado no catálogo: {}",
            item.name
        );
        ensure!(
            is_item_code(&item.code) && paint_index_from_code(&item.code) == Some(item.paint_index),
            "código inválido no catálogo: {}",
            item.name
        );
        ensure!(
            item.paint_index > 0
                && item.rank >= 0
                && item.reference_usd_cents.is_none_or(|p| p > 0),
            "número inválido no catálogo: {}",
            item.name
        );
    }
    Ok(catalog)
}

async fn import(db: &Db, catalog: Catalog) -> Result<()> {
    let count = catalog.items.len();
    db.call(move |c| {
        let tx = c.transaction()?;
        let installed: Option<i64> = tx.query_row("SELECT CAST(value AS INTEGER) FROM settings WHERE key='curated_catalog_at'", [], |r| r.get(0)).optional()?;
        if installed.is_some_and(|at| catalog.generated_at < at) {
            return Ok(());
        }
        let names: HashSet<String> = catalog.items.iter().map(|i| i.name.clone()).collect();
        let existing: Vec<String> = tx.prepare("SELECT name FROM catalog_references")?.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for name in existing {
            if !names.contains(&name) {
                tx.execute("DELETE FROM catalog_references WHERE name=?1", [name])?;
            }
        }
        let reference_at = catalog.reference_observed_at.unwrap_or(catalog.generated_at);
        for item in catalog.items {
            tx.execute(
                "INSERT INTO items(name, g_code, paint_index, icon_url, rank, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                   g_code = COALESCE(items.g_code, excluded.g_code),
                   paint_index = COALESCE(items.paint_index, excluded.paint_index),
                   icon_url = CASE WHEN COALESCE(items.icon_url, '') = '' THEN excluded.icon_url ELSE items.icon_url END
                 WHERE items.g_code IS NULL OR items.paint_index IS NULL
                    OR (COALESCE(items.icon_url, '') = '' AND excluded.icon_url <> '')",
                params![item.name, item.code, item.paint_index, item.icon_url, item.rank, catalog.generated_at],
            )?;
            tx.execute(
                "INSERT INTO catalog_references(name, usd_cents, observed_at, rank) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(name) DO UPDATE SET
                   usd_cents = CASE WHEN excluded.observed_at >= catalog_references.observed_at THEN excluded.usd_cents ELSE catalog_references.usd_cents END,
                   observed_at = MAX(excluded.observed_at, catalog_references.observed_at), rank = excluded.rank
                 WHERE excluded.observed_at > catalog_references.observed_at OR excluded.rank <> catalog_references.rank
                    OR (excluded.observed_at = catalog_references.observed_at AND excluded.usd_cents IS NOT catalog_references.usd_cents)",
                params![item.name, item.reference_usd_cents, reference_at, item.rank],
            )?;
        }
        tx.execute("INSERT INTO settings(key,value) VALUES ('curated_catalog_at',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [catalog.generated_at])?;
        tx.commit()
    }).await?;
    tracing::info!(items = count, "catálogo público carregado");
    Ok(())
}

pub async fn load(db: &Db) -> Result<()> {
    let catalog = parse(
        include_str!("../../assets/catalog/curated.json"),
        now_unix(),
    )?;
    import(db, catalog).await
}

const PRICE_URL: &str = "https://prices.csgotrader.app/latest/steam.json";
const MAX_PRICE_BYTES: usize = 8 * 1024 * 1024;
const DAY: i64 = 86_400;

fn source_time(value: &str, now: i64) -> Result<i64> {
    let at = chrono::DateTime::parse_from_rfc2822(value)
        .context("data da referência pública inválida")?
        .timestamp();
    ensure!(
        at > 0 && at <= now && now - at <= 2 * DAY,
        "data da referência pública fora do prazo"
    );
    Ok(at)
}

fn exact_cents(value: &serde_json::Value) -> Result<i64> {
    let number = value
        .as_number()
        .context("preço público não numérico")?
        .to_string();
    let (whole, fraction) = number.split_once('.').unwrap_or((&number, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= 2
            && fraction.bytes().all(|b| b.is_ascii_digit()),
        "preço público inválido"
    );
    let whole: i64 = whole.parse()?;
    let fraction: i64 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>()? * 10,
        _ => fraction.parse()?,
    };
    let cents = whole
        .checked_mul(100)
        .and_then(|v| v.checked_add(fraction))
        .context("preço público grande demais")?;
    ensure!(cents > 0, "preço público não positivo");
    Ok(cents)
}

fn parse_prices(bytes: &[u8], names: &HashSet<String>) -> Result<Vec<(String, i64)>> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("preços públicos inválidos")?;
    let prices = value.as_object().context("preços públicos sem objeto")?;
    ensure!(!prices.is_empty(), "preços públicos vazios");
    let mut result = Vec::new();
    for name in names {
        let Some(row) = prices.get(name) else {
            continue;
        };
        let row = row.as_object().context("referência pública inválida")?;
        for key in ["last_24h", "last_7d", "last_30d"] {
            if let Some(value) = row.get(key).filter(|v| !v.is_null()) {
                result.push((name.clone(), exact_cents(value)?));
                break;
            }
        }
    }
    ensure!(!result.is_empty(), "nenhum preço público para o catálogo");
    Ok(result)
}

/// Chaveiros (comuns, de adesivo e de souvenir) com venda na última semana, pelo menor preço médio entre 24 h,
/// 7 dias e 30 dias: quem não vendeu na semana fica de fora, porque preço sem venda não é lucro.
fn parse_charms(bytes: &[u8]) -> Result<Vec<(String, i64)>> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("preços públicos inválidos")?;
    let prices = value.as_object().context("preços públicos sem objeto")?;
    Ok(prices
        .iter()
        .filter(|(name, _)| {
            ["Charm | ", "Sticker Slab | ", "Souvenir Charm | "]
                .iter()
                .any(|p| name.starts_with(p))
        })
        .filter(|(_, row)| row.get("last_7d").is_some_and(|v| !v.is_null()))
        .filter_map(|(name, row)| {
            let cents = ["last_24h", "last_7d", "last_30d"]
                .iter()
                .filter_map(|k| row.get(k).filter(|v| !v.is_null()))
                .filter_map(|v| exact_cents(v).ok())
                .min()?;
            Some((name.clone(), cents))
        })
        .collect())
}

async fn refresh(db: &Db, client: &reqwest::Client) -> Result<()> {
    let mut response = client.get(PRICE_URL).send().await?.error_for_status()?;
    let at = source_time(
        response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .context("referência pública sem Last-Modified")?
            .to_str()?,
        now_unix(),
    )?;
    ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= MAX_PRICE_BYTES as u64),
        "resposta pública grande demais"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= MAX_PRICE_BYTES - bytes.len(),
            "resposta pública grande demais"
        );
        bytes.extend_from_slice(&chunk);
    }
    let names: HashSet<String> = db
        .call(|c| {
            c.prepare("SELECT name FROM catalog_references")?
                .query_map([], |r| r.get(0))?
                .collect()
        })
        .await?;
    let prices = parse_prices(&bytes, &names)?;
    let charms = parse_charms(&bytes)?;
    let (count, charm_count) = (prices.len(), charms.len());
    db.call(move |c| {
        let tx = c.transaction()?;
        for (name, cents) in prices {
            tx.execute("UPDATE catalog_references SET usd_cents=?1, observed_at=?2 WHERE name=?3 AND observed_at<?2", params![cents, at, name])?;
        }
        // troca inteira: o chaveiro que parou de vender sai
        tx.execute("DELETE FROM charm_prices", [])?;
        for (name, cents) in charms {
            tx.execute("INSERT INTO charm_prices(name, usd_cents, observed_at) VALUES (?1, ?2, ?3)", params![name, cents, at])?;
        }
        tx.commit()
    }).await?;
    tracing::info!(
        items = count,
        charms = charm_count,
        observed_at = at,
        "referências públicas conferidas"
    );
    Ok(())
}

pub fn spawn_refresh(db: Db) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                return tracing::warn!("cliente da referência pública indisponível: {error}");
            }
        };
        loop {
            let wait = match refresh(&db, &client).await {
                Ok(()) => DAY as u64,
                Err(error) => {
                    tracing::warn!("referência pública não atualizada: {error:#}");
                    3600
                }
            };
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charms_need_a_sale_in_the_week_and_take_the_lowest_average() {
        let body = br#"{
          "Charm | Hot Hands": {"last_24h": 5.5, "last_7d": 5.0, "last_30d": 6.25, "last_90d": 1.0},
          "Sticker Slab | Crown (Foil)": {"last_24h": null, "last_7d": 288.75, "last_30d": 442.03},
          "Souvenir Charm | Austin 2025 Highlight | X": {"last_24h": null, "last_7d": null, "last_30d": 9.0},
          "Sticker | Crown (Foil)": {"last_24h": 400.0, "last_7d": 400.0, "last_30d": 400.0}
        }"#;
        let mut charms = parse_charms(body).unwrap();
        charms.sort();
        assert_eq!(
            charms,
            vec![
                ("Charm | Hot Hands".to_string(), 500),
                ("Sticker Slab | Crown (Foil)".to_string(), 28875)
            ]
        );
    }

    const SAMPLE: &str = r#"{"version":1,"generated_at":100,"items":[
      {"name":"Glock-18 | Moonrise (Field-Tested)","code":"G180420B6053004","paint_index":694,"reference_usd_cents":300,"rank":4,"icon_url":"icon"},
      {"name":"StatTrak™ AK-47 | Redline (Field-Tested)","code":"G1807209A023004","paint_index":282,"reference_usd_cents":null,"rank":5,"icon_url":""}]}"#;

    #[test]
    fn validates_snapshot_before_import() {
        assert_eq!(parse(SAMPLE, 100).unwrap().items.len(), 2);
        assert!(parse(SAMPLE, 99).is_err());
        assert!(parse(&SAMPLE.replace("\"version\":1", "\"version\":2"), 100).is_err());
        assert!(
            parse(
                &SAMPLE.replace("\"paint_index\":694", "\"paint_index\":695"),
                100
            )
            .is_err()
        );
        assert!(
            parse(
                &SAMPLE.replace("Glock-18 | Moonrise", "★ Bayonet | Moonrise"),
                100
            )
            .is_err()
        );
        assert!(parse(&SAMPLE.replace("\"rank\":4", "\"rank\":-1"), 100).is_err());
        let mut duplicate: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        let items = duplicate["items"].as_array_mut().unwrap();
        items.push(items[0].clone());
        assert!(parse(&duplicate.to_string(), 100).is_err());
    }

    #[test]
    fn embedded_catalog_is_valid() {
        assert!(
            !parse(
                include_str!("../../assets/catalog/curated.json"),
                now_unix()
            )
            .unwrap()
            .items
            .is_empty()
        );
    }

    #[test]
    fn parses_real_price_fixture_and_checks_time_and_invalid_values() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../testdata/catalog_sources_sample.json"))
                .unwrap();
        let names = HashSet::from([
            "Glock-18 | Moonrise (Field-Tested)".to_string(),
            "missing".to_string(),
        ]);
        let bytes = serde_json::to_vec(&fixture["prices"]).unwrap();
        assert_eq!(
            parse_prices(&bytes, &names).unwrap(),
            vec![("Glock-18 | Moonrise (Field-Tested)".into(), 127)]
        );
        assert!(exact_cents(&serde_json::json!(-1)).is_err());
        assert!(exact_cents(&serde_json::json!(1.234)).is_err());
        assert!(exact_cents(&serde_json::json!("1.23")).is_err());
        let at = source_time("Sun, 06 Nov 1994 08:49:37 GMT", 784111777).unwrap();
        assert_eq!(at, 784111777);
        assert!(source_time("Sun, 06 Nov 1994 08:49:37 GMT", at - 1).is_err());
        assert!(source_time("Sun, 06 Nov 1994 08:49:37 GMT", at + 2 * DAY + 1).is_err());
        assert!(source_time("invalid", at).is_err());
    }

    #[tokio::test]
    async fn public_prices_decode_gzip_even_when_identity_was_requested() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(|| async {
                (
                    [
                        (reqwest::header::CONTENT_ENCODING, "gzip"),
                        (reqwest::header::CONTENT_TYPE, "application/json"),
                    ],
                    include_bytes!("../../testdata/catalog_prices_sample.json.gz").as_slice(),
                )
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = response.bytes().await.unwrap();
        let name = "Glock-18 | Moonrise (Field-Tested)";
        assert_eq!(
            parse_prices(&bytes, &HashSet::from([name.into()])).unwrap(),
            vec![(name.into(), 127)]
        );
        server.abort();
    }

    #[tokio::test]
    async fn new_curation_reconciles_membership_without_losing_newer_prices() {
        let (db, _dir) = crate::db::temp_db();
        db.call(|c| {
            c.execute_batch(&SCHEMA.replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS "))
        })
        .await
        .unwrap();
        import(&db, parse(SAMPLE, 100).unwrap()).await.unwrap();
        db.call(|c| {
            c.execute("UPDATE catalog_references SET usd_cents=900,observed_at=110 WHERE name LIKE 'Glock%'", [])?;
            Ok(())
        }).await.unwrap();
        let mut v: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        v["generated_at"] = 120.into();
        v["reference_observed_at"] = 100.into();
        v["items"].as_array_mut().unwrap().truncate(1);
        v["items"][0]["rank"] = 9.into();
        import(&db, parse(&v.to_string(), 120).unwrap())
            .await
            .unwrap();
        import(&db, parse(SAMPLE, 120).unwrap()).await.unwrap();
        db.call(|c| {
            let refs: (i64, i64, i64, i64) = c.query_row(
                "SELECT COUNT(*),usd_cents,observed_at,rank FROM catalog_references",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            assert_eq!(refs, (1, 900, 110, 9));
            let items: i64 = c.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
            assert_eq!(items, 2);
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn import_is_idempotent_and_preserves_measurements() {
        let (db, _dir) = crate::db::temp_db();
        db.call(|c| {
            c.execute_batch(&SCHEMA.replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS "))?;
            c.execute("INSERT INTO items(name,g_code,paint_index,icon_url,rank,updated_at,floor_cents,floor_usd_cents,sales_per_day,dispersion,history_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params!["Glock-18 | Moonrise (Field-Tested)", "G1807209A023004", 282, "steam-icon", 1, 90, 500, 100, 12.0, 0.2, 80])?;
            c.execute("INSERT INTO items(name,icon_url,rank,updated_at) VALUES (?1,'',19,90)", ["StatTrak™ AK-47 | Redline (Field-Tested)"])?;
            Ok(())
        }).await.unwrap();
        for _ in 0..2 {
            import(&db, parse(SAMPLE, 100).unwrap()).await.unwrap();
        }
        let newer = SAMPLE
            .replace("\"generated_at\":100", "\"generated_at\":101")
            .replace("\"reference_usd_cents\":300", "\"reference_usd_cents\":777");
        import(&db, parse(&newer, 101).unwrap()).await.unwrap();
        import(&db, parse(SAMPLE, 101).unwrap()).await.unwrap();
        db.call(|c| {
            let old: (String, i64, String, i64, i64, i64, i64, f64, f64, i64) = c.query_row("SELECT g_code, paint_index, icon_url, rank, updated_at, floor_cents, floor_usd_cents, sales_per_day, dispersion, history_at FROM items WHERE name LIKE 'Glock%'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
            assert_eq!(old, ("G1807209A023004".into(),282,"steam-icon".into(),1,90,500,100,12.0,0.2,80));
            let measured: bool = c.query_row("SELECT floor_cents IS NOT NULL OR floor_usd_cents IS NOT NULL OR sales_per_day IS NOT NULL OR dispersion IS NOT NULL OR history_at IS NOT NULL FROM items WHERE name LIKE 'StatTrak%'", [], |r| r.get(0))?;
            assert!(!measured);
            let metadata: (String,i64,i64,i64) = c.query_row("SELECT g_code,paint_index,rank,updated_at FROM items WHERE name LIKE 'StatTrak%'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            assert_eq!(metadata, ("G1807209A023004".into(),282,19,90));
            let count: i64 = c.query_row("SELECT count(*) FROM catalog_references", [], |r| r.get(0))?;
            assert_eq!(count, 2);
            let reference: (i64,i64) = c.query_row("SELECT usd_cents,observed_at FROM catalog_references WHERE name LIKE 'Glock%'", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
            assert_eq!(reference, (777,101));
            Ok(())
        }).await.unwrap();
    }
}
