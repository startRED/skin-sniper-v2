//! Varredura de `search/render` para descobrir o catálogo de armas (conhecimento 1.7 e 3).
//! Preços em USD (a Steam ignora a moeda aqui); 10 resultados por chamada.
#![allow(dead_code)]

use serde_json::Value;

use super::client::{Endpoint, Error, Result, SteamClient};

const ICON_BASE: &str = "https://community.fastly.steamstatic.com/economy/image/";

/// Categorias de arma. Faca e luva ficam de fora de propósito: nunca cabem numa banca pequena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Pistol,
    Rifle,
    Smg,
    Shotgun,
    SniperRifle,
    Machinegun,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Self::Pistol,
        Self::Rifle,
        Self::Smg,
        Self::Shotgun,
        Self::SniperRifle,
        Self::Machinegun,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            Self::Pistol => "tag_CSGO_Type_Pistol",
            Self::Rifle => "tag_CSGO_Type_Rifle",
            Self::Smg => "tag_CSGO_Type_SMG",
            Self::Shotgun => "tag_CSGO_Type_Shotgun",
            Self::SniperRifle => "tag_CSGO_Type_SniperRifle",
            Self::Machinegun => "tag_CSGO_Type_Machinegun",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// `market_hash_name`.
    pub name: String,
    /// Menor ask em USD (centavos).
    pub sell_price_usd_cents: i64,
    pub sell_listings: u32,
    pub icon_url: String,
    /// `market_bucket_group_id` (o código G): dispensa o 302 para nomes que vêm da busca.
    pub item_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub start: u32,
    pub total_count: u32,
    pub entries: Vec<CatalogEntry>,
}

/// Uma página de 10, ordenada por popularidade (`sort_column=popular`, decrescente).
pub async fn fetch_page(c: &SteamClient, cat: Category, start: u32) -> Result<SearchPage> {
    let q = [
        ("query", String::new()),
        ("start", start.to_string()),
        ("count", "10".into()),
        ("search_descriptions", "0".into()),
        ("sort_column", "popular".into()),
        ("sort_dir", "desc".into()),
        ("appid", "730".into()),
        ("category_730_Type[]", cat.tag().into()),
        ("norender", "1".into()),
    ];
    let v = c
        .get_json(Endpoint::Search, "/market/search/render/", &q)
        .await?;
    parse_page(&v)
}

/// Varre as categorias em ordem, página a página. `on_page` recebe as entradas de cada página e devolve
/// `false` para parar. O cursor avança pelo número de resultados recebidos; página vazia ou fim do
/// `total_count` encerra a categoria. Um erro devolve o que já foi entregue ao `on_page`.
pub async fn scan(
    c: &SteamClient,
    cats: &[Category],
    mut on_page: impl FnMut(Category, Vec<CatalogEntry>) -> bool,
) -> Result<()> {
    for &cat in cats {
        let mut start = 0u32;
        loop {
            let page = fetch_page(c, cat, start).await?;
            let got = page.entries.len() as u32;
            let total = page.total_count;
            if got == 0 {
                break;
            }
            if !on_page(cat, page.entries) {
                return Ok(());
            }
            start += got;
            if start >= total {
                break;
            }
        }
    }
    Ok(())
}

/// `sell_price` e `sell_listings` ausentes/inválidos descartam o resultado; `★` (faca/luva) também.
pub fn parse_page(v: &Value) -> Result<SearchPage> {
    if v.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Parse("search/render sem success:true".into()));
    }
    let start = v
        .get("start")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Parse("start ausente".into()))? as u32;
    let total_count = v.get("total_count").and_then(Value::as_u64).unwrap_or(0) as u32;
    let results = v
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Parse("results ausente".into()))?;
    let entries = results.iter().filter_map(entry).collect();
    Ok(SearchPage {
        start,
        total_count,
        entries,
    })
}

fn entry(r: &Value) -> Option<CatalogEntry> {
    let name = r
        .get("hash_name")
        .or_else(|| r.get("name"))
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty() && !n.starts_with('★'))?;
    let price = r
        .get("sell_price")
        .and_then(Value::as_i64)
        .filter(|p| *p > 0)?;
    let listings = u32::try_from(r.get("sell_listings").and_then(Value::as_i64)?).ok()?;
    let ad = r.get("asset_description");
    let icon = ad
        .and_then(|a| a.get("icon_url"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(CatalogEntry {
        name: name.to_string(),
        sell_price_usd_cents: price,
        sell_listings: listings,
        icon_url: if icon.is_empty() {
            String::new()
        } else {
            format!("{ICON_BASE}{icon}")
        },
        item_code: ad
            .and_then(|a| a.get("market_bucket_group_id"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_page() {
        let s = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/steam_search_render_pistol_p1900.json"
        ))
        .unwrap();
        let p = parse_page(&serde_json::from_str(&s).unwrap()).unwrap();
        assert_eq!((p.start, p.total_count, p.entries.len()), (1900, 3690, 10));
        let e = &p.entries[0];
        assert_eq!(e.name, "R8 Revolver | Canal Spray (Minimal Wear)");
        assert_eq!((e.sell_price_usd_cents, e.sell_listings), (461, 40));
        assert_eq!(e.item_code.as_deref(), Some("G184020E2063004"));
        assert!(e.icon_url.starts_with(ICON_BASE));
    }

    #[test]
    fn drops_knives_and_invalid_rows() {
        let v = serde_json::json!({"success": true, "start": 0, "total_count": 4, "results": [
            {"hash_name": "★ Karambit | Fade (Factory New)", "sell_price": 100000, "sell_listings": 3},
            {"hash_name": "AK-47 | Redline (Field-Tested)", "sell_price": 0, "sell_listings": 3},
            {"hash_name": "AK-47 | Slate (Field-Tested)", "sell_price": 50},
            {"hash_name": "AK-47 | Slate (Minimal Wear)", "sell_price": 50, "sell_listings": 0},
        ]});
        let p = parse_page(&v).unwrap();
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].sell_listings, 0);
        assert!(parse_page(&serde_json::json!({"success": false})).is_err());
    }

    #[test]
    fn categories_are_weapons_only() {
        assert!(
            Category::ALL
                .iter()
                .all(|c| !c.tag().contains("Knife") && !c.tag().contains("Gloves"))
        );
        assert_eq!(Category::ALL.len(), 6);
    }

    #[tokio::test]
    #[ignore = "ao vivo: 2 consultas à Steam (gap de 3 s)"]
    async fn live_scan_stops_when_asked() {
        let c = SteamClient::new().unwrap();
        let mut pages = 0;
        scan(&c, &[Category::Pistol], |_, e| {
            assert!(!e.is_empty());
            pages += 1;
            pages < 2
        })
        .await
        .unwrap();
        assert_eq!(pages, 2);
    }
}
