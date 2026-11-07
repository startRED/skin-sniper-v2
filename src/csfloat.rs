//! CSFloat: vendas reais por nome (`/history/<nome>/sales`) para medir o múltiplo `price / base_price`
//! (docs/pesquisa-web.md 5.1; conhecimento 2). Funciona sem chave; com chave o header é
//! `Authorization: <chave>` (sem "Bearer"). Limite medido: 500 por janela, com `x-ratelimit-reset` em
//! segundos Unix.
#![allow(dead_code)]

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use serde_json::Value;

use crate::steam::client::{Error, Result, path_escape};

const BASE: &str = "https://csfloat.com/api/v1";
/// Intervalo mínimo entre chamadas (`csfloat.gap_ms` do projeto antigo).
const MIN_GAP: Duration = Duration::from_millis(1200);
/// Sem `x-ratelimit-reset` num 429, espera 60 s.
const DEFAULT_PAUSE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CsfloatSale {
    /// Preço pago, em centavos de USD.
    pub price_usd_cents: i64,
    /// Preço-base da skin comum no momento da venda (`reference.base_price`).
    pub base_price_usd_cents: i64,
    /// `price / base_price`. Inclui o efeito de float, adesivos e padrão: quem usa decide o que filtrar.
    pub multiple: f64,
    pub seed: Option<u16>,
    pub float: Option<f64>,
    pub paint_index: Option<u32>,
    /// Segundos Unix (`sold_at`, ou `created_at` se faltar).
    pub sold_at: i64,
    /// Itens com adesivo vendem acima da base por causa do adesivo, não do padrão.
    pub has_stickers: bool,
    /// `reference.sticker_overpay.total_price`, em centavos (0 se ausente).
    pub sticker_overpay_cents: i64,
}

/// Janela de limite lida dos headers da última resposta.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RateLimit {
    pub limit: Option<u32>,
    pub remaining: Option<u32>,
    /// Segundos Unix em que a janela zera.
    pub reset: Option<i64>,
}

struct State {
    key: Option<String>,
    rl: RateLimit,
    blocked_until: Option<i64>,
    next_at: Instant,
    last_result: Option<(bool, String)>,
}

pub struct CsfloatClient {
    http: reqwest::Client,
    base: String,
    state: Mutex<State>,
    research_db: Mutex<Option<crate::db::Db>>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl CsfloatClient {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_base(BASE)
    }

    pub(crate) fn with_base(base: &str) -> anyhow::Result<Self> {
        Ok(Self {
            research_db: Mutex::new(None),
            http: reqwest::Client::builder()
                .user_agent("skin-sniper/2")
                .timeout(Duration::from_secs(30))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            state: Mutex::new(State {
                key: None,
                rl: RateLimit::default(),
                blocked_until: None,
                next_at: Instant::now(),
                last_result: None,
            }),
        })
    }

    pub fn set_research_db(&self, db: crate::db::Db) {
        *self.research_db.lock().unwrap() = Some(db);
    }

    /// Troca a chave em tempo de execução (`None` volta ao modo anônimo).
    pub fn set_key(&self, key: Option<String>) {
        let key = key
            .filter(|k| !k.trim().is_empty())
            .map(|k| k.trim().to_string());
        let mut s = self.state.lock().unwrap();
        if s.key != key {
            s.last_result = None;
            s.key = key;
        }
    }

    pub fn status(&self) -> Option<(bool, String)> {
        self.state.lock().unwrap().last_result.clone()
    }

    pub fn rate_limit(&self) -> RateLimit {
        self.state.lock().unwrap().rl
    }

    /// Vendas recentes (até 40) da skin pelo `market_hash_name`. Recusa sem chamar a rede enquanto a
    /// janela estiver esgotada (`remaining == 0` antes do `reset`) ou depois de um 429.
    pub async fn sales(&self, market_hash_name: &str) -> Result<Vec<CsfloatSale>> {
        // Bloqueio e cota são reconferidos a cada volta, inclusive ao acordar: outra chamada pode ter
        // tomado 429 enquanto esta dormia.
        loop {
            let wait = {
                let mut s = self.state.lock().unwrap();
                let now = now_unix();
                if let Some(until) = s.blocked_until.filter(|u| *u > now) {
                    return Err(Error::RateLimited { until });
                }
                if s.rl.remaining == Some(0)
                    && let Some(reset) = s.rl.reset.filter(|r| *r > now)
                {
                    return Err(Error::RateLimited { until: reset });
                }
                let t = Instant::now();
                if t >= s.next_at {
                    s.next_at = t + MIN_GAP;
                    break;
                }
                s.next_at - t
            };
            tokio::time::sleep(wait).await;
        }

        let mut req = self.http.get(format!(
            "{}/history/{}/sales",
            self.base,
            path_escape(market_hash_name)
        ));
        let key = self.state.lock().unwrap().key.clone();
        if let Some(k) = &key {
            req = req.header("Authorization", k);
        }
        let request = req
            .build()
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let db = self.research_db.lock().unwrap().clone();
        let attempt = if let Some(db) = db {
            Some(
                crate::research::record_external(db)
                    .await
                    .map_err(|e| Error::Transport(format!("registro de pesquisa: {e:#}")))?,
            )
        } else {
            None
        };
        let result = self.read_sales(request).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(Error::RateLimited { .. }) => "rate_limited",
            Err(Error::Transport(_)) => "network_error",
            Err(_) => "error",
        };
        crate::research::finish(attempt, outcome).await;
        let mut s = self.state.lock().unwrap();
        if s.key == key {
            s.last_result = Some(match &result {
                Ok(v) => (
                    true,
                    format!("vendas consultadas: {} na última resposta", v.len()),
                ),
                Err(e) => (false, e.to_string()),
            });
        }
        result
    }

    async fn read_sales(&self, request: reqwest::Request) -> Result<Vec<CsfloatSale>> {
        let resp = self
            .http
            .execute(request)
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let status = resp.status();
        let h = resp.headers();
        let num = |n: &str| {
            h.get(n)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
        };
        let rl = RateLimit {
            limit: num("x-ratelimit-limit").map(|v| v as u32),
            remaining: num("x-ratelimit-remaining").map(|v| v as u32),
            reset: num("x-ratelimit-reset"),
        };
        {
            let mut s = self.state.lock().unwrap();
            if rl != RateLimit::default() {
                s.rl = rl;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                let until = rl
                    .reset
                    .filter(|r| *r > now_unix())
                    .unwrap_or(now_unix() + DEFAULT_PAUSE.as_secs() as i64);
                s.blocked_until = Some(until);
                return Err(Error::RateLimited { until });
            }
            s.blocked_until = None;
        }
        if !status.is_success() {
            return Err(Error::Csfloat {
                status: status.as_u16(),
            });
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let v: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::Parse(format!("csfloat sales: {e}")))?;
        parse_sales(&v)
    }
}

/// Só entram vendas fechadas (`state == "sold"`) com preço e base positivos. Seed fora de 0..=1000 e
/// float fora de [0,1] viram `None`, nunca zero.
pub fn parse_sales(v: &Value) -> Result<Vec<CsfloatSale>> {
    let arr = v
        .as_array()
        .ok_or_else(|| Error::Parse("csfloat sales: esperava uma lista".into()))?;
    Ok(arr.iter().filter_map(sale).collect())
}

fn sale(s: &Value) -> Option<CsfloatSale> {
    if s.get("state")
        .and_then(Value::as_str)
        .is_some_and(|st| st != "sold")
    {
        return None;
    }
    let price = s.get("price").and_then(Value::as_i64).filter(|p| *p > 0)?;
    let base = s
        .pointer("/reference/base_price")
        .and_then(Value::as_i64)
        .filter(|p| *p > 0)?;
    let when = s
        .get("sold_at")
        .or_else(|| s.get("created_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339)?;
    let item = s.get("item").unwrap_or(&Value::Null);
    Some(CsfloatSale {
        price_usd_cents: price,
        base_price_usd_cents: base,
        multiple: price as f64 / base as f64,
        seed: item
            .get("paint_seed")
            .and_then(Value::as_i64)
            .filter(|n| (0..=1000).contains(n))
            .map(|n| n as u16),
        float: item
            .get("float_value")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite() && (0.0..=1.0).contains(f)),
        paint_index: item
            .get("paint_index")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .map(|n| n as u32),
        sold_at: when,
        has_stickers: item
            .get("stickers")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty()),
        sticker_overpay_cents: s
            .pointer("/reference/sticker_overpay/total_price")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

/// `2026-11-03T19:38:15.684938Z` → segundos Unix. Só aceita UTC (`Z`), que é o que o CSFloat manda.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let (hh, mm, ss): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // dias desde 1970-01-01 (algoritmo de Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode as Ax, routing::get};

    fn fixture() -> Value {
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/csfloat_sales_moonrise_ft.json"
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-11-03T19:38:15.684938Z"),
            Some(1793734695)
        );
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1709208000)); // bissexto
        assert_eq!(parse_rfc3339("2026-11-03T19:38:15+02:00"), None);
        assert_eq!(parse_rfc3339("lixo"), None);
    }

    #[test]
    fn real_capture() {
        let sales = parse_sales(&fixture()).unwrap();
        assert_eq!(sales.len(), 12);
        let a = &sales[0];
        assert_eq!((a.price_usd_cents, a.base_price_usd_cents), (74, 73));
        assert!((a.multiple - 74.0 / 73.0).abs() < 1e-12);
        assert_eq!((a.seed, a.paint_index), (Some(529), Some(694)));
        assert_eq!(a.float, Some(0.1846812665462494));
        assert_eq!(a.sold_at, 1793734695);
        assert!(!a.has_stickers && a.sticker_overpay_cents == 0);
        let st = &sales[1]; // 83 contra base 73, com 1 adesivo (overpay 33)
        assert!(st.has_stickers && st.sticker_overpay_cents == 33);
        assert!(sales.windows(2).all(|w| w[0].sold_at >= w[1].sold_at));
    }

    #[test]
    fn filters_unsold_and_invalid() {
        let v = serde_json::json!([
            {"state": "listed", "price": 100, "reference": {"base_price": 80}, "sold_at": "2026-11-03T00:00:00Z", "item": {}},
            {"state": "sold", "price": 100, "reference": {"base_price": 0}, "sold_at": "2026-11-03T00:00:00Z", "item": {}},
            {"state": "sold", "price": 100, "reference": {"base_price": 80}, "created_at": "2026-11-03T00:00:00Z",
             "item": {"paint_seed": 0, "float_value": 0.0, "paint_index": 0}},
            {"state": "sold", "price": 100, "reference": {"base_price": 80}, "sold_at": "2026-11-03T00:00:00Z",
             "item": {"paint_seed": 5000, "float_value": 3.0}},
        ]);
        let s = parse_sales(&v).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].seed, s[0].float, s[0].paint_index),
            (Some(0), Some(0.0), None)
        ); // zero ≠ ausente
        assert_eq!((s[1].seed, s[1].float), (None, None));
        assert!(parse_sales(&serde_json::json!({"a": 1})).is_err());
    }

    async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn old_key_response_does_not_validate_a_new_key() {
        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let (s, r) = (started.clone(), release.clone());
        let base = serve(Router::new().fallback(get(move || {
            let (s, r) = (s.clone(), r.clone());
            async move {
                s.notify_one();
                r.notified().await;
                "[]"
            }
        })))
        .await;
        let c = std::sync::Arc::new(CsfloatClient::with_base(&base).unwrap());
        c.set_key(Some("old-test-key".into()));
        let client = c.clone();
        let pending = tokio::spawn(async move { client.sales("sample").await });
        started.notified().await;
        c.set_key(Some("new-test-key".into()));
        release.notify_one();
        pending.await.unwrap().unwrap();
        assert!(c.status().is_none(), "a resposta pertence à chave enviada");
    }

    #[tokio::test]
    async fn research_records_completed_outcomes_and_health_for_the_current_key() {
        let base = serve(Router::new().route(
            "/history/{name}/sales",
            get(
                |axum::extract::Path(name): axum::extract::Path<String>| async move {
                    match name.as_str() {
                        "limited" => (Ax::TOO_MANY_REQUESTS, "[]"),
                        "refused" => (Ax::FORBIDDEN, "[]"),
                        _ => (Ax::OK, "[]"),
                    }
                },
            ),
        ))
        .await;
        let (db, _dir) = crate::db::temp_db();
        for (name, ok) in [("ok", true), ("refused", false), ("limited", false)] {
            let c = CsfloatClient::with_base(&base).unwrap();
            c.set_key(Some("test-key".into()));
            c.set_research_db(db.clone());
            assert_eq!(c.sales(name).await.is_ok(), ok);
            assert_eq!(c.status().unwrap().0, ok);
            c.set_key(Some("new-test-key".into()));
            assert!(
                c.status().is_none(),
                "não reaproveita a saúde da chave anterior"
            );
        }
        let outcomes: Vec<String> = db
            .call(|c| {
                c.prepare("SELECT outcome FROM research_attempts ORDER BY id")?
                    .query_map([], |r| r.get(0))?
                    .collect()
            })
            .await
            .unwrap();
        assert_eq!(outcomes, ["ok", "error", "rate_limited"]);
    }

    #[tokio::test]
    async fn key_header_headers_and_reset_are_respected() {
        let reset = now_unix() + 3600;
        let app = Router::new().route(
            "/history/{name}/sales",
            get(
                move |axum::extract::Path(name): axum::extract::Path<String>,
                      h: axum::http::HeaderMap| async move {
                    assert_eq!(name, "Glock-18 | Moonrise (Field-Tested)");
                    assert_eq!(h.get("authorization").unwrap(), "CHAVE");
                    (
                        Ax::OK,
                        [
                            ("x-ratelimit-limit", "500".to_string()),
                            ("x-ratelimit-remaining", "0".to_string()),
                            ("x-ratelimit-reset", reset.to_string()),
                        ],
                        "[]",
                    )
                },
            ),
        );
        let c = CsfloatClient::with_base(&serve(app).await).unwrap();
        c.set_key(Some(" CHAVE ".into()));
        assert!(
            c.sales("Glock-18 | Moonrise (Field-Tested)")
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            c.rate_limit(),
            RateLimit {
                limit: Some(500),
                remaining: Some(0),
                reset: Some(reset)
            }
        );
        // janela esgotada: nem chega à rede
        match c.sales("x").await {
            Err(Error::RateLimited { until }) => assert_eq!(until, reset),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn status_429_blocks_until_reset() {
        let reset = now_unix() + 120;
        let app = Router::new().route(
            "/history/{name}/sales",
            get(move || async move {
                (
                    Ax::TOO_MANY_REQUESTS,
                    [("x-ratelimit-reset", reset.to_string())],
                    "",
                )
            }),
        );
        let c = CsfloatClient::with_base(&serve(app).await).unwrap();
        assert!(matches!(c.sales("x").await, Err(Error::RateLimited { until }) if until == reset));
        assert!(matches!(c.sales("x").await, Err(Error::RateLimited { until }) if until == reset));
    }

    #[tokio::test]
    async fn concurrent_call_rechecks_block_after_waking() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        let reset = now_unix() + 120;
        let app = Router::new().route(
            "/history/{name}/sales",
            get(move || {
                let h = h2.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    (
                        Ax::TOO_MANY_REQUESTS,
                        [("x-ratelimit-reset", reset.to_string())],
                        "",
                    )
                }
            }),
        );
        let c = CsfloatClient::with_base(&serve(app).await).unwrap();
        // A toma 429 enquanto B dorme no intervalo mínimo; B não pode enviar durante a pausa.
        let (a, b) = tokio::join!(c.sales("x"), c.sales("y"));
        assert!(matches!(a, Err(Error::RateLimited { .. })));
        assert!(matches!(b, Err(Error::RateLimited { until }) if until == reset));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn forbidden_is_typed_error() {
        let app = Router::new().route("/history/{name}/sales", get(|| async { Ax::FORBIDDEN }));
        let c = CsfloatClient::with_base(&serve(app).await).unwrap();
        assert!(matches!(
            c.sales("x").await,
            Err(Error::Csfloat { status: 403 })
        ));
    }

    #[tokio::test]
    #[ignore = "ao vivo: 1 consulta ao CSFloat (cota de 500); chave opcional em CSFLOAT_KEY"]
    async fn live_sales() {
        let c = CsfloatClient::new().unwrap();
        c.set_key(std::env::var("CSFLOAT_KEY").ok());
        let s = c.sales("Glock-18 | Moonrise (Field-Tested)").await.unwrap();
        assert!(!s.is_empty() && s.iter().all(|x| x.multiple > 0.0));
        assert!(c.rate_limit().limit.is_some());
    }
}
