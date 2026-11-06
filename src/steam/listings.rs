//! Código G do item e `QueryListingsForItem` (docs/conhecimento.md 1.2, 1.3, 4; pesquisa-web 1.3–1.4).
#![allow(dead_code)]

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::client::{CURRENCY_BRL, Endpoint, Error, Result, SteamClient, path_escape};

const ICON_BASE: &str = "https://community.fastly.steamstatic.com/economy/image/";

// propertyid do esquema de ativos (pesquisa-web 1.4)
const PROP_SEED: i64 = 1;
const PROP_WEAR: i64 = 2;
const PROP_CHARM_TEMPLATE: i64 = 3;
const PROP_STICKER_WEAR: i64 = 4;
const PROP_PREFAB: i64 = 7;

// ---------------------------------------------------------------------------------------------
// Código G

/// `G` + protobuf em hex, tamanho total ímpar ≥ 5.
pub fn is_item_code(s: &str) -> bool {
    s.len() >= 5
        && s.len() % 2 == 1
        && s.starts_with('G')
        && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extrai o código do `Location` do 302 (absoluto ou relativo, com ou sem `?query`).
pub fn parse_item_code_location(location: &str) -> Result<String> {
    let path = location.split(['?', '#']).next().unwrap_or_default();
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let code = percent_decode(last);
    if is_item_code(&code) {
        Ok(code)
    } else {
        Err(Error::Parse(format!(
            "Location sem código G válido: {location}"
        )))
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve o código G pelo 302 de `/market/listings/730/<nome>`. Persistir o resultado é de outra etapa
/// (o código nunca muda, então vale cache permanente).
pub async fn resolve_item_code(c: &SteamClient, market_hash_name: &str) -> Result<String> {
    let path = format!("/market/listings/730/{}", path_escape(market_hash_name));
    let (status, loc) = c.get_location(Endpoint::Listings, &path).await?;
    match (status, loc) {
        (301 | 302 | 303 | 307 | 308, Some(l)) => parse_item_code_location(&l),
        (s, _) => Err(Error::Parse(format!(
            "esperava 302 para {market_hash_name}, veio HTTP {s}"
        ))),
    }
}

/// Paint index nominal do item, lido do código (campo 4 do protobuf em varints).
pub fn paint_index_from_code(code: &str) -> Option<u32> {
    if !is_item_code(code) {
        return None;
    }
    let hex = &code[1..];
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect::<Option<_>>()?;
    let mut i = 0;
    while i < bytes.len() {
        let (tag, n) = varint(&bytes[i..])?;
        i += n;
        if tag & 7 != 0 {
            return None; // o código só tem varints
        }
        let (val, n) = varint(&bytes[i..])?;
        i += n;
        if tag >> 3 == 4 {
            return u32::try_from(val).ok();
        }
    }
    None
}

fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, byte) in b.iter().take(10).enumerate() {
        v |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Consulta

/// Filtro do lado da Valve por propriedade do ativo.
#[derive(Debug, Clone, PartialEq)]
pub enum PropFilter {
    /// Inteiro (seed = propriedade 1). Faixa inclusiva; vai como STRING (pesquisa-web 1.3).
    Int {
        property_id: u32,
        min: i64,
        max: i64,
    },
    /// Float (propriedade 2). Faixa inclusiva; vai como NÚMERO (`int_min` na prop 2 quebra a resposta).
    Float { min: f64, max: f64 },
}

impl PropFilter {
    pub fn seed(min: u16, max: u16) -> Self {
        Self::Int {
            property_id: PROP_SEED as u32,
            min: min.into(),
            max: max.into(),
        }
    }

    pub fn float(min: f64, max: f64) -> Self {
        Self::Float { min, max }
    }

    fn id(&self) -> u32 {
        match self {
            Self::Int { property_id, .. } => *property_id,
            Self::Float { .. } => PROP_WEAR as u32,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Int {
                property_id,
                min,
                max,
            } => {
                json!({"property_id": property_id, "int_min": min.to_string(), "int_max": max.to_string()})
            }
            Self::Float { min, max } => {
                json!({"property_id": PROP_WEAR, "float_min": min, "float_max": max})
            }
        }
    }
}

/// Monta o `qp` de `QueryListingsForItem`: preço ascendente (`sort.field` 0, `direction` 0). A ordem é
/// aproximada: a captura de 2026-11-03 mostra inversões de ~1 % em BRL (a Steam ordena por outro preço-base).
/// Uma faixa por propriedade: dois filtros na mesma propriedade viram um erro, não uma sobrescrita silenciosa.
pub fn build_qp(code: &str, start: u32, filters: &[PropFilter]) -> Result<Value> {
    build_qp_wear(code, start, None, filters)
}

/// Como `build_qp`, com o filtro de desgaste (`Exterior`: 0 = FN … 4 = BS) do lado da Valve.
pub fn build_qp_wear(
    code: &str,
    start: u32,
    wear: Option<u8>,
    filters: &[PropFilter],
) -> Result<Value> {
    if !is_item_code(code) {
        return Err(Error::Parse(format!("código G inválido: {code}")));
    }
    let mut props = serde_json::Map::new();
    for f in filters {
        if props.insert(f.id().to_string(), f.to_json()).is_some() {
            return Err(Error::Parse(format!(
                "dois filtros na propriedade {}",
                f.id()
            )));
        }
    }
    Ok(json!([{
        "appid": 730,
        "strItemName": code,
        "filters": wear.map_or(json!({}), |w| json!({"Exterior": [format!("WearCategory{w}")]})),
        "accessoryFilters": {},
        "propertyFilters": props,
        "sort": {"field": 0, "direction": 0},
        "start": start,
    }]))
}

/// Uma página (até 20 anúncios). Avance `start` pelo número de anúncios recebidos.
pub async fn query_listings(
    c: &SteamClient,
    code: &str,
    start: u32,
    filters: &[PropFilter],
) -> Result<ListingsPage> {
    query_listings_wear(c, code, start, None, filters).await
}

/// Como `query_listings`, restrita a um desgaste (o código cobre todos eles).
pub async fn query_listings_wear(
    c: &SteamClient,
    code: &str,
    start: u32,
    wear: Option<u8>,
    filters: &[PropFilter],
) -> Result<ListingsPage> {
    let qp = build_qp_wear(code, start, wear, filters)?;
    let data = c
        .action(Endpoint::Listings, "QueryListingsForItem", &qp)
        .await?;
    parse_page(&data, start)
}

/// Busca por nome na interface antiga quando as actions estão indisponíveis ou a saída pública tem vaga. Ela cobre só o item
/// comum completo: nunca simula um filtro de seed/float recortando os primeiros anúncios.
/// O booleano indica que a página cobre somente o nome solicitado.
pub async fn query_listings_named(
    c: &SteamClient,
    name: &str,
    code: &str,
    start: u32,
    wear: Option<u8>,
) -> Result<(ListingsPage, bool)> {
    if c.prefer_public_listings() {
        match query_legacy_named(c, name, code, start).await {
            Ok(page) => return Ok((page, true)),
            Err(Error::BudgetExhausted) => return Err(Error::BudgetExhausted),
            Err(e) => tracing::warn!("leitura pública alternativa falhou; tenta a principal: {e}"),
        }
    }
    match query_listings_wear(c, code, start, wear, &[]).await {
        Ok(page) => Ok((page, false)),
        Err(
            Error::RateLimited { .. } | Error::BreakerOpen { .. } | Error::ActionUnavailable { .. },
        ) => Ok((query_legacy_named(c, name, code, start).await?, true)),
        Err(e) => Err(e),
    }
}

async fn query_legacy_named(
    c: &SteamClient,
    name: &str,
    code: &str,
    start: u32,
) -> Result<ListingsPage> {
    let path = format!("/market/listings/730/{}/render/", path_escape(name));
    let q = [
        ("query", String::new()),
        ("start", start.to_string()),
        ("count", "20".into()),
        ("country", "BR".into()),
        ("language", "english".into()),
        ("currency", CURRENCY_BRL.to_string()),
    ];
    let data = c.legacy_json(&path, &q, false).await?;
    let page = super::legacy::parse_page(&data, name, code, start)?;
    c.note_legacy_ok();
    Ok(page)
}

// ---------------------------------------------------------------------------------------------
// Resultado

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accessory {
    /// `market_hash_name` que a Valve manda (nunca reconstruído).
    pub name: String,
    /// Raspagem do adesivo (propriedade 4; 0 = intacto).
    pub scrape: Option<f64>,
    /// Template do chaveiro (propriedade 3).
    pub template: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub listing_id: String,
    /// `asset.assetid`: o item em si. Volta igual quando o vendedor tira e põe de novo à venda (relistagem).
    #[serde(default)]
    pub asset_id: String,
    /// `market_hash_name`.
    pub name: String,
    /// O que o comprador paga = `unPrice + unFee`, em centavos.
    pub price_cents: i64,
    /// `unPrice`: o que o vendedor recebe.
    pub seller_cents: i64,
    /// `unFee`.
    pub fee_cents: i64,
    pub currency: i32,
    pub float: Option<f64>,
    pub seed: Option<u16>,
    /// Propriedade 7; se faltar, o paint do código do balde.
    pub paint_index: Option<u32>,
    /// URL completa da imagem.
    pub icon_url: String,
    pub inspect_url: Option<String>,
    pub stickers: Vec<Accessory>,
    pub keychains: Vec<Accessory>,
    /// Dias de trava de troca / de revenda no Mercado.
    pub market_tradable_restriction: Option<u32>,
    pub market_marketable_restriction: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListingsPage {
    pub start: u32,
    pub total_count: u64,
    pub more: bool,
    pub listings: Vec<Listing>,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Parse(msg.into())
}

/// Lê `data` de `QueryListingsForItem`. Contrato mínimo (conhecimento 1.3): cada anúncio tem id, assetid,
/// nome, preço > 0 e moeda > 0; sem id repetido nem moedas misturadas. Qualquer falha invalida a página
/// inteira, porque uma página parcial esconderia anúncios.
pub fn parse_page(data: &Value, expected_start: u32) -> Result<ListingsPage> {
    let start = data
        .get("start")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad("data.start ausente"))?;
    if start != u64::from(expected_start) {
        return Err(bad(format!("start {start} em vez de {expected_start}")));
    }
    let total_count = data.get("total_count").and_then(Value::as_u64).unwrap_or(0);
    let more = data.get("more").and_then(Value::as_bool).unwrap_or(false);
    let raw = data
        .get("listings")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("data.listings ausente"))?;

    let mut seen = HashSet::new();
    let mut listings = Vec::with_capacity(raw.len());
    let mut currency = None;
    for r in raw {
        let l = parse_listing(r)?;
        if !seen.insert(l.listing_id.clone()) {
            return Err(bad(format!("listingid repetido: {}", l.listing_id)));
        }
        if l.currency != CURRENCY_BRL {
            return Err(Error::WrongCurrency { got: l.currency });
        }
        match currency {
            None => currency = Some(l.currency),
            Some(cur) if cur != l.currency => return Err(bad("moedas misturadas na página")),
            _ => {}
        }
        listings.push(l);
    }
    Ok(ListingsPage {
        start: expected_start,
        total_count,
        more,
        listings,
    })
}

fn int_of(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn prop(props: &[Value], id: i64) -> Option<&Value> {
    props
        .iter()
        .find(|p| p.get("propertyid").and_then(Value::as_i64) == Some(id))
}

fn prop_int(props: &[Value], id: i64) -> Option<i64> {
    prop(props, id)
        .and_then(|p| p.get("int_value"))
        .and_then(int_of)
}

fn prop_float(props: &[Value], id: i64) -> Option<f64> {
    prop(props, id)
        .and_then(|p| p.get("float_value"))
        .and_then(Value::as_f64)
}

fn parse_listing(r: &Value) -> Result<Listing> {
    let listing_id = r
        .get("listingid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("listingid ausente"))?;
    let asset = r
        .get("asset")
        .ok_or_else(|| bad(format!("{listing_id}: sem asset")))?;
    let asset_id = asset
        .get("assetid")
        .or_else(|| asset.get("id"))
        .and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("{listing_id}: sem assetid")))?;
    let seller = r.get("unPrice").and_then(Value::as_i64).unwrap_or(0);
    // Taxa ausente, nula ou não inteira invalida o anúncio: assumir 0 inventaria um total menor.
    let fee = r
        .get("unFee")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad(format!("{listing_id}: unFee ausente ou inválido")))?;
    let currency = r.get("eCurrency").and_then(Value::as_i64).unwrap_or(0);
    if seller <= 0 || fee < 0 || currency <= 0 {
        return Err(bad(format!(
            "{listing_id}: preço ou moeda inválidos ({seller}+{fee}, moeda {currency})"
        )));
    }
    let desc = r.get("description").unwrap_or(&Value::Null);
    let name = desc
        .get("market_hash_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("{listing_id}: sem market_hash_name")))?
        .to_string();

    let props: &[Value] = asset
        .get("asset_properties")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);

    // Zero conhecido ≠ ausente: seed 0 e float 0 são válidos. Fora da faixa vira desconhecido.
    let float = prop_float(props, PROP_WEAR).filter(|f| f.is_finite() && (0.0..=1.0).contains(f));
    let seed = prop_int(props, PROP_SEED)
        .filter(|s| (0..=1000).contains(s))
        .map(|s| s as u16);
    let paint_index = prop_int(props, PROP_PREFAB)
        .and_then(|p| u32::try_from(p).ok())
        .filter(|p| *p > 0)
        .or_else(|| {
            desc.get("market_bucket_group_id")
                .and_then(Value::as_str)
                .and_then(paint_index_from_code)
        });

    let icon = desc
        .get("icon_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let icon_url = if icon.is_empty() {
        String::new()
    } else {
        format!("{ICON_BASE}{icon}")
    };

    let (mut stickers, mut keychains) = (vec![], vec![]);
    for a in asset
        .get("asset_accessories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (acc, is_charm) = parse_accessory(a)?;
        if is_charm {
            keychains.push(acc)
        } else {
            stickers.push(acc)
        }
    }

    Ok(Listing {
        listing_id: listing_id.to_string(),
        asset_id,
        name,
        price_cents: seller + fee,
        seller_cents: seller,
        fee_cents: fee,
        currency: currency as i32,
        float,
        seed,
        paint_index,
        icon_url,
        inspect_url: inspect_url(desc, props),
        stickers,
        keychains,
        market_tradable_restriction: desc
            .get("market_tradable_restriction")
            .and_then(Value::as_u64)
            .map(|d| d as u32),
        market_marketable_restriction: desc
            .get("market_marketable_restriction")
            .and_then(Value::as_u64)
            .map(|d| d as u32),
    })
}

/// Chaveiro = tipo com "Charm" ou nome `Charm |`. Sticker Slab tem tipo de chaveiro, então cai aqui também.
fn parse_accessory(a: &Value) -> Result<(Accessory, bool)> {
    let d = a.get("description").unwrap_or(&Value::Null);
    let name = d
        .get("market_hash_name")
        .or_else(|| d.get("name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("acessório sem nome"))?
        .to_string();
    let ty = d.get("type").and_then(Value::as_str).unwrap_or_default();
    let is_charm = ty.contains("Charm") || name.starts_with("Charm |");
    let list = |k: &str| {
        a.get(k)
            .and_then(Value::as_array)
            .map_or(Vec::new(), Clone::clone)
    };
    let standalone = list("standalone_properties");
    let parent = list("parent_relationship_properties");
    let scrape = prop_float(&parent, PROP_STICKER_WEAR).filter(|f| f.is_finite() && *f >= 0.0);
    let template = prop_int(&standalone, PROP_CHARM_TEMPLATE);
    Ok((
        Accessory {
            name,
            scrape,
            template,
        },
        is_charm,
    ))
}

/// O link vem como template (`...%20%propid:6%`): troca cada `%propid:N%` pelo valor da propriedade N.
/// Se sobrar placeholder (propriedade ausente), o link não serve e volta `None`.
fn inspect_url(desc: &Value, props: &[Value]) -> Option<String> {
    let link = desc
        .get("market_actions")
        .or_else(|| desc.get("actions"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|a| a.get("link"))
        .and_then(Value::as_str)?;
    let mut out = String::with_capacity(link.len() + 64);
    let mut rest = link;
    while let Some(i) = rest.find("%propid:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + "%propid:".len()..];
        let end = after.find('%')?;
        let id: i64 = after[..end].parse().ok()?;
        let p = prop(props, id)?;
        let v = p
            .get("string_value")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| p.get("int_value").and_then(int_of).map(|n| n.to_string()))?;
        out.push_str(&v);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    (!out.is_empty() && !out.contains("%propid:")).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let s = std::fs::read_to_string(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        serde_json::from_str::<Value>(&s).unwrap()["data"].take()
    }

    #[test]
    fn code_from_location_absolute_and_relative() {
        for l in [
            "https://steamcommunity.com/market/listings/730/G180420B6053004",
            "/market/listings/730/G180420B6053004?start=0&count=2&currency=7&format=json",
        ] {
            assert_eq!(
                parse_item_code_location(l).unwrap(),
                "G180420B6053004",
                "{l}"
            );
        }
        assert!(parse_item_code_location("/market/listings/730/Glock").is_err());
        assert!(parse_item_code_location("/login/home").is_err());
        assert!(!is_item_code("G18042")); // tamanho par
    }

    #[test]
    fn paint_from_code() {
        assert_eq!(paint_index_from_code("G180420B6053004"), Some(694)); // Moonrise
        assert_eq!(paint_index_from_code("G1807209A023004"), Some(282)); // Redline
        assert_eq!(paint_index_from_code("G18F403209F033003"), Some(415)); // Bayonet Doppler
        assert_eq!(paint_index_from_code("lixo"), None);
    }

    #[test]
    fn path_escape_name() {
        assert_eq!(
            path_escape("Glock-18 | Moonrise (Field-Tested)"),
            "Glock-18%20%7C%20Moonrise%20%28Field-Tested%29"
        );
        assert_eq!(path_escape("★ Bayonet"), "%E2%98%85%20Bayonet");
    }

    #[test]
    fn qp_has_string_ints_and_price_sort() {
        let qp = build_qp("G180420B6053004", 20, &[PropFilter::seed(58, 66)]).unwrap();
        let q = &qp[0];
        assert_eq!(
            q["propertyFilters"]["1"],
            json!({"property_id": 1, "int_min": "58", "int_max": "66"})
        );
        assert_eq!(q["sort"], json!({"field": 0, "direction": 0}));
        assert_eq!(q["start"], 20);
        assert_eq!(q["strItemName"], "G180420B6053004");
        assert!(
            build_qp(
                "G180420B6053004",
                0,
                &[PropFilter::seed(1, 2), PropFilter::seed(3, 4)]
            )
            .is_err()
        );
        assert!(build_qp("Glock-18 | Moonrise (Field-Tested)", 0, &[]).is_err());
    }

    #[test]
    fn live_capture_sorted_by_price() {
        // captura real de 2026-11-03 com sort.field 0 / direction 0
        let p = parse_page(&fixture("steam_listings_moonrise_sorted_price.json"), 0).unwrap();
        assert_eq!(p.listings.len(), 20);
        assert!(p.more && p.total_count > 20);
        // A Steam ordena por um preço-base, não pelo `unPrice` em BRL: há inversões de ~1 % na captura.
        // Quem calcula o piso deve usar o mínimo da página, não o primeiro anúncio.
        let mut max = 0;
        for l in &p.listings {
            assert!(
                l.seller_cents * 100 >= max * 98,
                "fora de ordem além de 2 %: {} depois de {max}",
                l.seller_cents
            );
            max = max.max(l.seller_cents);
        }
        for l in &p.listings {
            assert_eq!(l.price_cents, l.seller_cents + l.fee_cents);
            assert_eq!(l.currency, 7);
            assert!(l.seed.is_some() && l.float.is_some());
            assert_eq!(l.paint_index, Some(694));
            assert!(l.icon_url.starts_with(ICON_BASE));
            let u = l.inspect_url.as_deref().unwrap();
            assert!(
                u.starts_with("steam://run/730//+csgo_econ_action_preview%20")
                    && !u.contains("propid")
            );
        }
    }

    #[test]
    fn asset_id_comes_from_the_asset() {
        let p = parse_page(&fixture("steam_listings_moonrise_sorted_price.json"), 0).unwrap();
        assert_eq!(p.listings[0].asset_id, "54226282359");
        // anúncio guardado antes do campo existir (opportunities.listing_json) continua legível
        let mut old = serde_json::to_value(&p.listings[0]).unwrap();
        old.as_object_mut().unwrap().remove("asset_id");
        let back: Listing = serde_json::from_value(old).unwrap();
        assert_eq!(back.asset_id, "");
    }

    #[test]
    fn redline_fixture_values() {
        let d = fixture("steam_AK_47___Redline__Field_Tested_.json");
        let p = parse_page(&d, 0).unwrap();
        let first = &p.listings[0];
        assert_eq!(first.name, "AK-47 | Redline (Field-Tested)");
        assert_eq!(first.price_cents, first.seller_cents + first.fee_cents);
        assert_eq!(first.market_tradable_restriction, Some(7));
        assert_eq!(first.market_marketable_restriction, Some(7));
        assert_eq!(first.paint_index, Some(282));
        assert!(
            p.listings
                .iter()
                .all(|l| l.float.is_some_and(|f| (0.0..=1.0).contains(&f)))
        );
    }

    #[test]
    fn money_matches_documented_example() {
        // pesquisa-web 1.3: 512 + 76 = R$5,88 — conferido no texto "strSubtotal" de uma listagem real
        let d = fixture("steam_AK_47___Redline__Field_Tested_.json");
        let raw = &d["listings"][0];
        let l = parse_listing(raw).unwrap();
        let text = raw["strSubtotal"]
            .as_str()
            .unwrap()
            .replace(['R', '$', ',', '.'], "");
        assert_eq!(l.price_cents, text.parse::<i64>().unwrap());
    }

    #[test]
    fn stickers_with_scrape() {
        let p = parse_page(&fixture("steam_actions_ak_redline_stickers.json"), 0).unwrap();
        let l = p.listings.iter().find(|l| !l.stickers.is_empty()).unwrap();
        assert!(l.keychains.is_empty());
        assert!(
            l.stickers
                .iter()
                .any(|s| s.name.starts_with("Sticker | ") && s.scrape == Some(0.0))
        );
    }

    #[test]
    fn charm_goes_to_keychains_with_template() {
        let p = parse_page(&fixture("steam_actions_charm_sample.json"), 0).unwrap();
        let l = &p.listings[0];
        assert!(l.stickers.is_empty());
        assert_eq!(l.keychains.len(), 1);
        assert_eq!(l.keychains[0].name, "Charm | Hot Hands");
        assert_eq!(l.keychains[0].template, Some(41268));
        assert_eq!(l.keychains[0].scrape, None);
    }

    #[test]
    fn sticker_slab_is_keychain_typed() {
        let p = parse_page(&fixture("steam_actions_mac10_sticker_slab.json"), 0).unwrap();
        let acc: Vec<_> = p
            .listings
            .iter()
            .flat_map(|l| l.keychains.iter().chain(&l.stickers))
            .collect();
        assert!(!acc.is_empty());
    }

    #[test]
    fn doppler_phase_comes_from_prop7() {
        let p = parse_page(&fixture("steam_actions_bayonet_doppler.json"), 0).unwrap();
        let phases: HashSet<_> = p.listings.iter().filter_map(|l| l.paint_index).collect();
        assert!(phases.iter().all(|p| (415..=421).contains(p)), "{phases:?}");
        assert!(
            phases.iter().any(|p| *p != 415),
            "prop 7 deve ganhar do código (415)"
        );
    }

    #[test]
    fn seed_filter_and_bs_fixtures() {
        let p = parse_page(&fixture("steam_actions_fiveseven_seed904.json"), 0).unwrap();
        assert!(!p.listings.is_empty() && p.listings.iter().all(|l| l.seed == Some(904)));
        let p = parse_page(
            &fixture("steam_Glock_18___Moonrise__Battle_Scarred_.json"),
            0,
        )
        .unwrap();
        assert_eq!((p.listings.len(), p.total_count), (20, 251));
        assert!(p.listings.iter().all(
            |l| l.name == "Glock-18 | Moonrise (Battle-Scarred)" && l.paint_index == Some(694)
        ));
    }

    #[test]
    fn empty_page_is_valid() {
        let p = parse_page(&fixture("steam_actions_karambit_doppler.json"), 0).unwrap();
        assert!(p.listings.is_empty() && !p.more && p.total_count == 0);
    }

    #[test]
    fn start_mismatch_is_error() {
        assert!(parse_page(&fixture("steam_actions_karambit_doppler.json"), 20).is_err());
    }

    fn page(listings: Value) -> Value {
        json!({"start": 0, "total_count": 9, "more": false, "listings": listings})
    }

    fn raw(id: &str, props: Value) -> Value {
        json!({"listingid": id, "unPrice": 100, "unFee": 15, "eCurrency": 7,
               "description": {"market_hash_name": "X (Factory New)"},
               "asset": {"assetid": "a", "asset_properties": props}})
    }

    #[test]
    fn zero_is_not_absent_and_bad_values_are_unknown() {
        let p = parse_page(&page(json!([
            raw("1", json!([{"propertyid": 1, "int_value": "0"}, {"propertyid": 2, "float_value": 0.0}])),
            raw("2", json!([{"propertyid": 1, "int_value": "1001"}, {"propertyid": 2, "float_value": 1.5}])),
            raw("3", json!([{"propertyid": 2}])),
            raw("4", json!([])),
        ])), 0)
        .unwrap();
        let g = |i: usize| (p.listings[i].seed, p.listings[i].float);
        assert_eq!(g(0), (Some(0), Some(0.0)));
        assert_eq!(g(1), (None, None));
        assert_eq!(g(2), (None, None));
        assert_eq!(g(3), (None, None));
        assert!(p.listings[3].inspect_url.is_none()); // sem descrição de ação
    }

    #[test]
    fn inspect_url_rejects_leftover_placeholder() {
        let desc = json!({"market_actions": [{"link": "steam://run/730//+csgo_econ_action_preview%20%propid:6%"}]});
        assert_eq!(inspect_url(&desc, &[]), None);
        let props = vec![json!({"propertyid": 6, "string_value": "ABCD"})];
        assert_eq!(
            inspect_url(&desc, &props).as_deref(),
            Some("steam://run/730//+csgo_econ_action_preview%20ABCD")
        );
    }

    #[test]
    fn missing_or_invalid_fee_discards_listing() {
        for bad_fee in [json!(null), json!("15"), json!(1.5)] {
            let mut r = raw("1", json!([]));
            r["unFee"] = bad_fee.clone();
            assert!(parse_page(&page(json!([r])), 0).is_err(), "{bad_fee}");
        }
        let mut no_fee = raw("1", json!([]));
        no_fee.as_object_mut().unwrap().remove("unFee");
        assert!(parse_page(&page(json!([no_fee])), 0).is_err());
        let mut zero = raw("1", json!([]));
        zero["unFee"] = json!(0);
        assert_eq!(
            parse_page(&page(json!([zero])), 0).unwrap().listings[0].price_cents,
            100
        );
    }

    #[test]
    fn non_brl_page_is_wrong_currency() {
        let mut a = raw("1", json!([]));
        let mut b = raw("2", json!([]));
        a["eCurrency"] = json!(1);
        b["eCurrency"] = json!(1);
        assert!(matches!(
            parse_page(&page(json!([a, b])), 0),
            Err(Error::WrongCurrency { got: 1 })
        ));
    }

    #[test]
    fn page_contract_failures() {
        let dup = page(json!([raw("same", json!([])), raw("same", json!([]))]));
        assert!(parse_page(&dup, 0).is_err());
        let mut broken = raw("2", json!([]));
        broken["unPrice"] = json!(0);
        assert!(parse_page(&page(json!([raw("1", json!([])), broken])), 0).is_err());
        let mut usd = raw("2", json!([]));
        usd["eCurrency"] = json!(1);
        assert!(parse_page(&page(json!([raw("1", json!([])), usd])), 0).is_err());
        let mut noname = raw("3", json!([]));
        noname["description"] = json!({});
        assert!(parse_page(&page(json!([noname])), 0).is_err());
    }

    #[tokio::test]
    #[ignore = "ao vivo: 2 consultas à Steam (gap de 3 s)"]
    async fn live_resolve_and_query() {
        let c = SteamClient::new().unwrap();
        let code = resolve_item_code(&c, "Glock-18 | Moonrise (Field-Tested)")
            .await
            .unwrap();
        assert_eq!(code, "G180420B6053004");
        let p = query_listings(&c, &code, 0, &[PropFilter::seed(58, 66)])
            .await
            .unwrap();
        assert!(
            p.listings
                .iter()
                .all(|l| l.seed.is_none_or(|s| (58..=66).contains(&s)))
        );
    }
}
