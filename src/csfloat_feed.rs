//! Caça no CSFloat: lê os anúncios "compre já" mais novos do site inteiro (`/listings?sort_by=most_recent`).
//! Em 12/11/2026, 50 anúncios cobriam ~35–45 s e a cota da chave era 200 consultas por janela de 1 h.
//!
//! Medido no mesmo dia: a API entrega o anúncio ~60 s depois de criado (o mais novo da página tinha 61,8 s pelo
//! relógio do servidor), e os 3 achados da régua antiga (preço abaixo de `reference.predicted_price`) seguiam à
//! venda 40 min depois. Quem compra só pelo preço usa ordem de compra, que casa no servidor antes de o anúncio
//! aparecer aqui. Por isso o achado precisa de uma prova que o preço previsto não dá:
//! - **pattern**: o seed cai num grupo raro com vendas guardadas (`csfloat_sales`) e o anúncio sai abaixo do
//!   quartil de baixo delas. A referência do CSFloat tem fator de float e de adesivo, não de pattern.
//! - **price**: além de abaixo do previsto, abaixo da mediana das vendas reais de float mais próximo.
//!
//! Só avisa e grava (`csfloat_deals`, `csfloat_feed`): comprar exige saldo no CSFloat, decisão do Erick depois de
//! medir quantos achados aparecem e quanto tempo duram (`checks`, `state`, `gone_at`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::csfloat::{CsfloatClient, CsfloatSale};
use crate::db::Db;
use crate::pricing::groups::{group_of, skin_base};
use crate::pricing::sell::{MAX_RARE_MULTIPLE, RARE_MIN_SALES};
use crate::settings::SettingsStore;
use crate::steam::client::Error;

/// 180 leituras/h, abaixo das 200 da janela.
const EVERY: Duration = Duration::from_secs(20);
const PAGE: u32 = 50;
/// Taxa de venda do CSFloat.
const SELL_FEE: f64 = 0.02;
/// Lucro mínimo revendendo no CSFloat: US$ 0,50 e 10 % do preço.
// shortcut: régua igual à da Steam (R$ 1 / 10 %) convertida a olho; ajustar pelos achados gravados.
const MIN_PROFIT_CENTS: i64 = 50;
const MIN_MARGIN: f64 = 0.10;
/// Abaixo de US$ 1 a previsão é ruído de centavos.
const MIN_PRICE_CENTS: i64 = 100;
/// Referência feita com poucos anúncios da skin não é referência.
const MIN_QUANTITY: i64 = 10;
/// Vendas de float mais próximo que dão o preço de revenda, e a idade máxima delas.
// shortcut: 5 vendas e 30 dias são hipótese; acertar pelo que os achados `price` fizerem (`gone_at`).
const NEAR_SALES: usize = 5;
const SALES_MAX_AGE: i64 = 30 * 86_400;
/// As vendas de um nome valem por este tempo; os grupos raros são relidos do banco neste intervalo.
const SALES_TTL: i64 = 6 * 3600;
const GROUPS_TTL: i64 = 600;
const SEEN_MAX: usize = 2000;
/// Conferências do achado, em segundos depois do aviso: mede quanto tempo ele ficou à venda. Usam `/listings/<id>`,
/// que tem cota própria (50 000 na sonda de 12/11/2026), fora das 200 do feed.
const RECHECKS: [i64; 5] = [60, 600, 3600, 6 * 3600, 24 * 3600];
const REPORT_SECS: i64 = 600;

/// Grupo raro → `(nome, preço ÷ preço-base)` das vendas guardadas do CSFloat. Float fica de fora: o previsto já
/// o conta, e a régua de preço confere com as vendas de float próximo.
pub type Groups = HashMap<String, Vec<(String, f64)>>;

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
    pub paint_index: Option<u32>,
    pub stickers: usize,
}

/// Por que o anúncio é achado e por quanto dá para revender (centavos de USD).
#[derive(Debug, Clone, PartialEq)]
pub struct Deal {
    pub kind: &'static str,
    pub sell: i64,
    pub evidence: String,
}

fn usd(c: i64) -> String {
    format!("US$ {:.2}", c as f64 / 100.0)
}

impl Listing {
    /// Centavos de USD que sobram revendendo no CSFloat por `sell`.
    pub fn profit_at(&self, sell: i64) -> i64 {
        (sell as f64 * (1.0 - SELL_FEE)).floor() as i64 - self.price
    }

    /// Revender por `sell` paga o lucro mínimo.
    pub fn clears(&self, sell: i64) -> bool {
        let min = MIN_PROFIT_CENTS.max((self.price as f64 * MIN_MARGIN).ceil() as i64);
        self.price >= MIN_PRICE_CENTS
            && self.quantity >= MIN_QUANTITY
            && self.base > 0
            && self.profit_at(sell) >= min
    }

    pub fn url(&self) -> String {
        format!("https://csfloat.com/item/{}", self.id)
    }
}

/// Grupo raro do anúncio que tem venda guardada: `(rótulo, vendas)`. Tabela embutida primeiro, depois o seed
/// aprendido nas vendas.
fn rare_group<'a>(l: &Listing, groups: &'a Groups) -> Option<(String, &'a [(String, f64)])> {
    let seed = u32::try_from(l.seed?).ok()?;
    let table = group_of(&l.name, Some(seed), l.paint_index, None).map(|g| (g.key, g.label));
    let learned = (
        format!("learned:{}:{seed}", skin_base(&l.name)),
        format!("Pattern #{seed}"),
    );
    [table, Some(learned)]
        .into_iter()
        .flatten()
        .find_map(|(key, label)| Some((label, groups.get(&key)?.as_slice())))
}

/// Pattern raro abaixo do que o grupo já vendeu: revenda pelo quartil de baixo dos múltiplos do grupo (os do
/// mesmo nome quando bastam, senão os de todos os desgastes) × o preço-base do anúncio.
pub fn pattern_deal(l: &Listing, groups: &Groups) -> Option<Deal> {
    let (label, sales) = rare_group(l, groups)?;
    let same: Vec<f64> = sales
        .iter()
        .filter(|s| s.0 == l.name)
        .map(|s| s.1)
        .collect();
    let mut m = if same.len() >= RARE_MIN_SALES {
        same
    } else {
        sales.iter().map(|s| s.1).collect()
    };
    m.retain(|x| x.is_finite() && *x > 0.0);
    if m.len() < RARE_MIN_SALES {
        return None;
    }
    m.sort_by(f64::total_cmp);
    let low = m[(m.len() - 1) / 4].min(MAX_RARE_MULTIPLE);
    let sell = (l.base as f64 * low) as i64;
    l.clears(sell).then(|| Deal {
        kind: "pattern",
        sell,
        evidence: format!(
            "{label}: {} vendas do grupo, quartil de baixo a {low:.2}× o preço comum",
            m.len()
        ),
    })
}

/// Mediana das `NEAR_SALES` vendas recentes de float mais próximo (as últimas, em item sem float). Anúncio sem
/// adesivo só se compara com venda sem adesivo. `None` com menos vendas que isso.
pub fn sold_price(l: &Listing, sales: &[CsfloatSale], now: i64) -> Option<i64> {
    let mut near: Vec<&CsfloatSale> = sales
        .iter()
        .filter(|s| now - s.sold_at <= SALES_MAX_AGE && (l.stickers > 0 || !s.has_stickers))
        .collect();
    match l.float {
        Some(f) => {
            let far = |s: &CsfloatSale| s.float.map_or(f64::MAX, |x| (x - f).abs());
            near.sort_by(|a, b| far(a).total_cmp(&far(b)));
        }
        None => near.sort_by_key(|s| std::cmp::Reverse(s.sold_at)),
    }
    near.truncate(NEAR_SALES);
    if near.len() < NEAR_SALES {
        return None;
    }
    let mut prices: Vec<i64> = near.iter().map(|s| s.price_usd_cents).collect();
    prices.sort_unstable();
    Some(prices[prices.len() / 2])
}

/// Abaixo do previsto E abaixo do que vendeu de verdade: revenda pelo menor dos dois.
pub fn price_deal(l: &Listing, sales: &[CsfloatSale], now: i64) -> Option<Deal> {
    let sold = sold_price(l, sales, now)?;
    let sell = sold.min(l.predicted);
    let which = if l.float.is_some() {
        "de float próximo"
    } else {
        "mais recentes"
    };
    l.clears(sell).then(|| Deal {
        kind: "price",
        sell,
        evidence: format!(
            "as {NEAR_SALES} vendas {which} saíram a {} (mediana); previsto {}",
            usd(sold),
            usd(l.predicted)
        ),
    })
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
                paint_index: item
                    .get("paint_index")
                    .and_then(Value::as_u64)
                    .and_then(|i| u32::try_from(i).ok()),
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

async fn load_groups(db: &Db) -> Groups {
    let rows: Vec<(String, String, f64)> = db
        .call(|c| {
            c.prepare(
                "SELECT grp, name, multiple FROM csfloat_sales
                 WHERE grp IS NOT NULL AND grp NOT LIKE 'float:%' AND grp NOT LIKE 'highfloat:%'",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect()
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("grupos raros do CSFloat não carregaram: {e:#}");
            vec![]
        });
    let mut groups = Groups::new();
    for (grp, name, multiple) in rows {
        groups.entry(grp).or_default().push((name, multiple));
    }
    groups
}

type SalesCache = HashMap<String, (i64, Vec<CsfloatSale>)>;

/// Vendas do nome, lidas no máximo uma vez a cada `SALES_TTL`. `None` se o CSFloat não respondeu.
async fn sales_of<'a>(
    api: &CsfloatClient,
    cache: &'a mut SalesCache,
    name: &str,
) -> Option<&'a [CsfloatSale]> {
    let at = now();
    if cache.get(name).is_none_or(|(t, _)| at - t > SALES_TTL) {
        if cache.len() >= 500 {
            cache.clear();
        }
        match api.sales(name).await {
            Ok(v) => cache.insert(name.to_string(), (at, v)),
            Err(e) => {
                tracing::warn!(
                    "CSFloat: vendas de {name} não vieram, achado sem prova fica de fora: {e}"
                );
                return None;
            }
        };
    }
    cache.get(name).map(|(_, v)| v.as_slice())
}

async fn record_deal(db: &Db, l: &Listing, d: &Deal) {
    let (l, d) = (l.clone(), d.clone());
    let r = db
        .call(move |c| {
            c.execute(
                r#"INSERT OR IGNORE INTO csfloat_deals (id, at, created_at, name, price_cents, predicted_cents,
                   base_cents, quantity, "float", seed, stickers, profit_cents, kind, sell_cents, evidence)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,
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
                    l.profit_at(d.sell),
                    d.kind,
                    d.sell,
                    d.evidence
                ],
            )
        })
        .await;
    if let Err(e) = r {
        tracing::warn!("achado do CSFloat não gravou: {e:#}");
    }
}

/// O achado `(id, visto em, conferências feitas)` mais antigo cuja próxima conferência já venceu.
fn due(rows: &[(String, i64, i64)], now: i64) -> Option<&str> {
    rows.iter()
        .find(|(_, at, checks)| {
            RECHECKS
                .get(*checks as usize)
                .is_some_and(|after| now - at >= *after)
        })
        .map(|r| r.0.as_str())
}

/// Confere um achado vencido por volta: ainda à venda? Mede quanto tempo dá para comprar na mão e se alguém
/// comprou pelo preço (prova de que era achado).
async fn recheck(db: &Db, api: &CsfloatClient) {
    let rows: Vec<(String, i64, i64)> = db
        .call(|c| {
            c.prepare(
                "SELECT id, at, checks FROM csfloat_deals WHERE gone_at IS NULL AND checks < ?1 ORDER BY at",
            )?
            .query_map([RECHECKS.len() as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect()
        })
        .await
        .unwrap_or_default();
    let Some(id) = due(&rows, now()).map(str::to_string) else {
        return;
    };
    let state = match api.listing(&id).await {
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
    tracing::info!("CSFloat: achado {id} conferido: {state}");
    let at = now();
    let _ = db
        .call(move |c| {
            c.execute(
                "UPDATE csfloat_deals SET state = ?2, checks = checks + 1,
                 state_60s = CASE WHEN checks = 0 THEN ?2 ELSE state_60s END,
                 gone_at = CASE WHEN ?2 = 'listed' THEN NULL ELSE ?3 END WHERE id = ?1",
                rusqlite::params![id, state, at],
            )
        })
        .await;
}

/// `api` é o cliente do resto do app (vendas e conferência por id); o feed tem o seu, para a cota de 200 da
/// listagem não se misturar com as outras nos headers.
pub async fn run(db: Db, settings: SettingsStore, api: Arc<CsfloatClient>) {
    let client = match CsfloatClient::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("caça no CSFloat não sobe: {e:#}");
            return;
        }
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut order: VecDeque<String> = VecDeque::new();
    let mut newest: Option<i64> = None;
    let (mut groups, mut groups_at) = (Groups::new(), 0i64);
    let mut sales = SalesCache::new();
    let (mut pages, mut fresh_total, mut gaps, mut deals, mut rares) =
        (0u32, 0u32, 0u32, 0u32, 0u32);
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
        if now() - groups_at >= GROUPS_TTL {
            (groups, groups_at) = (load_groups(&db).await, now());
        }
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
        let (mut fresh, mut found, mut rare) = (0u32, 0u32, 0u32);
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
            rare += u32::from(rare_group(&l, &groups).is_some());
            let deal = match pattern_deal(&l, &groups) {
                Some(d) => Some(d),
                // só o candidato pelo previsto gasta uma leitura de vendas
                None if l.clears(l.predicted) => sales_of(&api, &mut sales, &l.name)
                    .await
                    .and_then(|s| price_deal(&l, s, now())),
                None => None,
            };
            let Some(d) = deal else { continue };
            found += 1;
            tracing::info!(
                "CSFloat: achado [{}] {} a {} (revenda {}, lucro {}; {}) float={:?} seed={:?} {}",
                d.kind,
                l.name,
                usd(l.price),
                usd(d.sell),
                usd(l.profit_at(d.sell)),
                d.evidence,
                l.float,
                l.seed,
                l.url()
            );
            record_deal(&db, &l, &d).await;
            let body = format!(
                "{} · lucro {} revendendo no CSFloat ({})",
                usd(l.price),
                usd(l.profit_at(d.sell)),
                d.evidence
            );
            if let Err(e) = crate::notify::desktop(&l.name, &body, &l.url()).await {
                tracing::warn!("aviso do achado do CSFloat falhou: {e}");
            }
        }
        fresh_total += fresh;
        deals += found;
        rares += rare;
        let at = now();
        let _ = db
            .call(move |c| {
                c.execute(
                    "INSERT INTO csfloat_feed (at, fresh, gap, deals, rare) VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![at, fresh, gap, found, rare],
                )
            })
            .await;
        recheck(&db, &api).await;
        if at - report_at >= REPORT_SECS {
            let _ = db
                .call(move |c| {
                    c.execute("DELETE FROM csfloat_feed WHERE at < ?1", [at - 30 * 86_400])
                })
                .await;
            tracing::info!(
                "CSFloat em 10 min: leituras={pages} anúncios novos={fresh_total} buracos={gaps} de grupo raro={rares} achados={deals} cota={:?}",
                client.rate_limit().remaining
            );
            (pages, fresh_total, gaps, deals, rares, report_at) = (0, 0, 0, 0, 0, at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(price: i64, predicted: i64, base: i64, quantity: i64) -> Listing {
        Listing {
            id: "1".into(),
            created_at: 0,
            price,
            predicted,
            base,
            quantity,
            name: "Glock-18 | Moonrise (Field-Tested)".into(),
            float: Some(0.20),
            seed: Some(694),
            paint_index: None,
            stickers: 0,
        }
    }

    fn sale(price: i64, float: f64, sold_at: i64, has_stickers: bool) -> CsfloatSale {
        CsfloatSale {
            price_usd_cents: price,
            base_price_usd_cents: 100,
            multiple: price as f64 / 100.0,
            seed: None,
            float: Some(float),
            paint_index: None,
            sold_at,
            has_stickers,
            sticker_overpay_cents: 0,
        }
    }

    #[test]
    fn parses_a_real_feed_page() {
        let v: Value =
            serde_json::from_str(include_str!("../testdata/csfloat_feed_recent.json")).unwrap();
        let ls = parse(&v);
        assert_eq!(ls.len(), 6);
        let l = &ls[1];
        assert_eq!(
            (l.name.as_str(), l.price, l.predicted, l.base, l.quantity),
            ("M4A4 | Asiimov (Well-Worn)", 10125, 9625, 9847, 138)
        );
        assert_eq!(
            (l.seed, l.paint_index, l.stickers, l.created_at > 0),
            (Some(949), Some(255), 3, true)
        );
        assert_eq!(l.url(), "https://csfloat.com/item/1041072318358622117");
        // adesivo solto não tem float nem seed
        assert_eq!((ls[2].float, ls[2].seed), (None, None));
        // nada nesta página pagava 10 % nem pelo previsto
        assert!(ls.iter().all(|l| !l.clears(l.predicted)));
    }

    #[test]
    fn minimum_profit_is_half_a_dollar_and_ten_percent() {
        // 1300 × 0,98 − 1000 = 274 ≥ 100
        assert_eq!(listing(1000, 0, 1000, 150).profit_at(1300), 274);
        assert!(listing(1000, 0, 1000, 150).clears(1300));
        // 1100 × 0,98 − 1000 = 78 < 100
        assert!(!listing(1000, 0, 1000, 150).clears(1100));
        // referência com 3 anúncios, abaixo de US$ 1, sem preço-base
        assert!(!listing(1000, 0, 1000, 3).clears(1300));
        assert!(!listing(80, 0, 80, 150).clears(300));
        assert!(!listing(1000, 0, 0, 150).clears(1300));
    }

    #[test]
    fn rare_pattern_sells_at_the_low_quartile_of_its_group() {
        let name = "Glock-18 | Moonrise (Field-Tested)";
        let key = "cs:Glock-18 | Moonrise:t1";
        let mut groups = Groups::new();
        groups.insert(
            key.into(),
            [1.4, 1.6, 2.0, 2.4, 3.0]
                .map(|m| (name.to_string(), m))
                .into(),
        );
        // Center Star (694) barato demais para o mínimo: 100 × 1,6 × 0,98 − 110 = 46 < 50
        assert_eq!(pattern_deal(&listing(110, 100, 100, 150), &groups), None);
        let d = pattern_deal(&listing(1100, 1000, 1000, 150), &groups).unwrap();
        assert_eq!((d.kind, d.sell), ("pattern", 1600));
        assert!(d.evidence.contains("5 vendas"), "{}", d.evidence);
        // seed comum não tem grupo; grupo com 2 vendas não prova
        let mut common = listing(1100, 1000, 1000, 150);
        common.seed = Some(5);
        assert_eq!(pattern_deal(&common, &groups), None);
        groups.get_mut(key).unwrap().truncate(2);
        assert_eq!(pattern_deal(&listing(1100, 1000, 1000, 150), &groups), None);
        // seed aprendido nas vendas, com as vendas em outro desgaste
        let mut learned = listing(1000, 1000, 1000, 150);
        learned.name = "MP7 | Amberline (Minimal Wear)".into();
        learned.seed = Some(890);
        groups.insert(
            "learned:MP7 | Amberline:890".into(),
            vec![("MP7 | Amberline (Field-Tested)".into(), 5.0); 3],
        );
        assert_eq!(pattern_deal(&learned, &groups).unwrap().sell, 5000);
    }

    #[test]
    fn predicted_price_needs_real_sales_of_close_float() {
        let now = 100 * 86_400;
        let mut l = listing(1000, 1400, 1000, 150);
        l.seed = None;
        // float 0,20: as 5 mais próximas saíram a ~1300; as de float distante, mais baratas, não contam
        let mut sales: Vec<CsfloatSale> = [1290, 1300, 1310, 1320, 1280]
            .iter()
            .enumerate()
            .map(|(i, p)| sale(*p, 0.20 + i as f64 * 0.001, now - 3600, false))
            .collect();
        sales.extend((0..10).map(|_| sale(900, 0.35, now - 3600, false)));
        assert_eq!(sold_price(&l, &sales, now), Some(1300));
        let d = price_deal(&l, &sales, now).unwrap();
        assert_eq!((d.kind, d.sell), ("price", 1300));
        // previsto menor que as vendas: vale o previsto
        l.predicted = 1200;
        assert_eq!(price_deal(&l, &sales, now).unwrap().sell, 1200);
        // previsto inflado: as vendas reais de float próximo saíram pelo preço pedido
        l.predicted = 1400;
        let flat: Vec<CsfloatSale> = (0..8)
            .map(|_| sale(1010, 0.20, now - 3600, false))
            .collect();
        assert_eq!(price_deal(&l, &flat, now), None);
        // venda velha ou com adesivo não prova; anúncio com adesivo aceita venda com adesivo
        let old: Vec<CsfloatSale> = (0..8)
            .map(|_| sale(1300, 0.20, now - 31 * 86_400, false))
            .collect();
        assert_eq!(sold_price(&l, &old, now), None);
        let stickered: Vec<CsfloatSale> =
            (0..8).map(|_| sale(1300, 0.20, now - 3600, true)).collect();
        assert_eq!(sold_price(&l, &stickered, now), None);
        l.stickers = 2;
        assert_eq!(sold_price(&l, &stickered, now), Some(1300));
    }

    #[test]
    fn rechecks_follow_the_schedule_until_the_last_one() {
        let rows = vec![
            ("feito".to_string(), 0, 5),
            ("cedo".to_string(), 1000, 1),
            ("vencido".to_string(), 1000, 0),
        ];
        // 1000 + 60 venceu para quem não foi conferido; 1000 + 600 ainda não
        assert_eq!(due(&rows, 1100), Some("vencido"));
        assert_eq!(due(&rows, 1600), Some("cedo"));
        assert_eq!(due(&rows[..2], 1100), None);
    }
}
