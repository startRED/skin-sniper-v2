//! HTTP único da Steam: cookie trocável, limitador global adaptativo e breaker por endpoint.
//!
//! Ritmo (docs/projeto.md, "Caça → Ritmo"): começa em 1 consulta a cada 3 s; cada 30 min sem 429 encurta
//! 5 % (piso 1,5 s); um 429 pausa 60 s (dobrando até 10 min) e alonga o intervalo em 50 %.
//! O limitador é um só para todos os endpoints; os breakers são um por endpoint (conhecimento 2): falhas
//! repetidas que não são 429 (5xx, rede) abrem só o breaker daquele endpoint.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{COOKIE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, StatusCode, redirect};
use serde_json::Value;

pub const BASE: &str = "https://steamcommunity.com";
/// Moeda que o painel espera (BRL).
pub const CURRENCY_BRL: i32 = 7;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

const BASE_GAP: Duration = Duration::from_secs(3);
const MIN_GAP: Duration = Duration::from_millis(1500);
// Teto do intervalo depois de vários 429 seguidos (acréscimo meu: o desenho não limita o alongamento).
const MAX_GAP: Duration = Duration::from_secs(60);
const SHORTEN_EVERY: Duration = Duration::from_secs(30 * 60);
const PAUSE_START: Duration = Duration::from_secs(60);
const PAUSE_MAX: Duration = Duration::from_secs(10 * 60);

const BREAKER_THRESHOLD: u32 = 3;
const BREAKER_OPEN_START: Duration = Duration::from_secs(30);
const BREAKER_OPEN_MAX: Duration = Duration::from_secs(10 * 60);

/// Tipos de endpoint com saúde independente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    Listings,
    History,
    Search,
    Session,
    Buy,
}

impl Endpoint {
    const ALL: [Endpoint; 5] = [
        Self::Listings,
        Self::History,
        Self::Search,
        Self::Session,
        Self::Buy,
    ];
    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Debug)]
pub enum Error {
    /// 429 da Steam: tudo pausado até `until` (segundos Unix).
    RateLimited {
        until: i64,
    },
    /// Breaker do endpoint aberto até `until` (segundos Unix); nenhuma requisição foi feita.
    BreakerOpen {
        endpoint: Endpoint,
        until: i64,
    },
    Http {
        endpoint: Endpoint,
        status: u16,
    },
    /// CSFloat recusou (ex.: 401/403/5xx).
    Csfloat {
        status: u16,
    },
    Transport(String),
    /// Resposta fora do formato esperado.
    Parse(String),
    /// Cookie ausente, expirado ou recusado.
    SessionExpired,
    /// A conta/resposta está em outra moeda que não BRL.
    WrongCurrency {
        got: i32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited { until } => write!(f, "Steam respondeu 429; pausado até {until}"),
            Self::BreakerOpen { endpoint, until } => write!(f, "{endpoint:?} em pausa até {until}"),
            Self::Http { endpoint, status } => write!(f, "{endpoint:?}: HTTP {status}"),
            Self::Csfloat { status } => write!(f, "CSFloat: HTTP {status}"),
            Self::Transport(e) => write!(f, "falha de rede: {e}"),
            Self::Parse(e) => write!(f, "resposta inesperada: {e}"),
            Self::SessionExpired => write!(f, "sessão da Steam ausente ou expirada"),
            Self::WrongCurrency { got } => write!(f, "moeda {got} em vez de BRL (7)"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Cookie de login. Nunca aparece em log nem em `Debug`.
#[derive(Clone)]
pub struct Cookie {
    pub steam_login_secure: String,
    pub sessionid: Option<String>,
}

impl fmt::Debug for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cookie(<oculto>)")
    }
}

impl Cookie {
    fn header(&self) -> String {
        let mut s = format!("steamLoginSecure={}", self.steam_login_secure);
        if let Some(id) = &self.sessionid {
            s.push_str(&format!("; sessionid={id}"));
        }
        s
    }
}

/// Números para o painel (`State.hunt`). Datas em segundos Unix.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub queries_per_min: u32,
    pub last_query_at: Option<i64>,
    pub paused_until: Option<i64>,
    pub gap_ms: u64,
}

// ---------------------------------------------------------------------------------------------
// Limitador: lógica pura, com o relógio injetado para testar sem dormir.

pub(crate) struct Limiter {
    gap: Duration,
    next_at: Instant,
    paused_until: Option<Instant>,
    pause_len: Duration,
    /// Último 429 ou último encurtamento; o próximo encurta 30 min depois.
    last_adjust: Instant,
    last_query: Option<Instant>,
    recent: VecDeque<Instant>,
}

impl Limiter {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            gap: BASE_GAP,
            next_at: now,
            paused_until: None,
            pause_len: PAUSE_START,
            last_adjust: now,
            last_query: None,
            recent: VecDeque::new(),
        }
    }

    fn shorten(&mut self, now: Instant) {
        while now.duration_since(self.last_adjust) >= SHORTEN_EVERY {
            self.gap = self.gap.mul_f64(0.95).max(MIN_GAP);
            self.last_adjust += SHORTEN_EVERY;
        }
    }

    /// `Ok` toma a vaga (e conta a consulta); `Err(espera)` diz quanto falta.
    pub(crate) fn try_acquire(&mut self, now: Instant) -> std::result::Result<(), Duration> {
        self.shorten(now);
        let free_at = self
            .paused_until
            .map_or(self.next_at, |p| p.max(self.next_at));
        if now < free_at {
            return Err(free_at - now);
        }
        self.paused_until = None;
        self.next_at = now + self.gap;
        self.last_query = Some(now);
        self.recent.push_back(now);
        Ok(())
    }

    /// 429: pausa e alonga 50 %. Um 429 dentro de uma pausa já ativa não avança o degrau (idempotente).
    pub(crate) fn on_429(&mut self, now: Instant, retry_after: Option<Duration>) {
        if self.paused_until.is_some_and(|p| now < p) {
            return;
        }
        let pause = self.pause_len.max(retry_after.unwrap_or_default());
        self.paused_until = Some(now + pause);
        self.pause_len = (self.pause_len * 2).min(PAUSE_MAX);
        self.gap = self.gap.mul_f64(1.5).min(MAX_GAP);
        self.last_adjust = now;
    }

    /// Resposta que não foi 429: o degrau da pausa volta ao início.
    pub(crate) fn on_ok(&mut self) {
        self.pause_len = PAUSE_START;
    }

    pub(crate) fn stats(&mut self, now: Instant) -> Stats {
        self.shorten(now);
        while self
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            self.recent.pop_front();
        }
        let unix_of = |t: Instant| {
            let wall = if t >= now {
                SystemTime::now() + (t - now)
            } else {
                SystemTime::now() - (now - t)
            };
            wall.duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64)
        };
        Stats {
            queries_per_min: self.recent.len() as u32,
            last_query_at: self.last_query.map(unix_of),
            paused_until: self.paused_until.filter(|p| *p > now).map(unix_of),
            gap_ms: self.gap.as_millis() as u64,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Breaker: abre depois de 3 falhas seguidas (5xx/rede) e dobra o tempo aberto a cada reabertura.

#[derive(Clone, Copy)]
pub(crate) struct Breaker {
    fails: u32,
    open_until: Option<Instant>,
    open_len: Duration,
}

impl Breaker {
    const fn new() -> Self {
        Self {
            fails: 0,
            open_until: None,
            open_len: BREAKER_OPEN_START,
        }
    }

    /// `Some(até)` enquanto aberto. Passado o prazo, deixa passar uma tentativa de prova.
    pub(crate) fn check(&mut self, now: Instant) -> Option<Instant> {
        match self.open_until {
            Some(t) if now < t => Some(t),
            Some(_) => {
                self.open_until = None;
                self.fails = BREAKER_THRESHOLD - 1; // uma falha na prova reabre de imediato
                None
            }
            None => None,
        }
    }

    pub(crate) fn on_ok(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn on_fail(&mut self, now: Instant) {
        self.fails += 1;
        if self.fails >= BREAKER_THRESHOLD && self.open_until.is_none() {
            self.open_until = Some(now + self.open_len);
            self.open_len = (self.open_len * 2).min(BREAKER_OPEN_MAX);
        }
    }
}

// ---------------------------------------------------------------------------------------------

struct Inner {
    limiter: Limiter,
    breakers: [Breaker; 5],
    cookie: Option<Cookie>,
}

pub struct SteamClient {
    http: reqwest::Client,
    base: String,
    inner: Mutex<Inner>,
}

/// Escapa tudo que não é seguro num segmento de caminho.
pub(crate) fn path_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unix_in(d: Duration) -> i64 {
    (SystemTime::now() + d)
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl SteamClient {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_base(BASE)
    }

    pub(crate) fn with_base(base: &str) -> anyhow::Result<Self> {
        let mut h = HeaderMap::new();
        h.insert(
            "accept-language",
            HeaderValue::from_static("en-US,en;q=0.9,pt-BR;q=0.8"),
        );
        h.insert(
            "referer",
            HeaderValue::from_static("https://steamcommunity.com/market/"),
        );
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(h)
            .redirect(redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            inner: Mutex::new(Inner {
                limiter: Limiter::new(Instant::now()),
                breakers: [Breaker::new(); 5],
                cookie: None,
            }),
        })
    }

    /// Troca (ou remove) o cookie em tempo de execução. Vale para as próximas requisições `auth`.
    pub fn set_cookie(&self, cookie: Option<Cookie>) {
        self.inner.lock().unwrap().cookie = cookie;
    }

    pub fn cookie(&self) -> Option<Cookie> {
        self.inner.lock().unwrap().cookie.clone()
    }

    pub fn stats(&self) -> Stats {
        self.inner.lock().unwrap().limiter.stats(Instant::now())
    }

    /// Fim da pausa do breaker do endpoint (segundos Unix), se estiver aberto.
    pub fn breaker_open_until(&self, ep: Endpoint) -> Option<i64> {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        g.breakers[ep.idx()].check(now).map(|t| unix_in(t - now))
    }

    pub fn open_breakers(&self) -> Vec<(Endpoint, i64)> {
        Endpoint::ALL
            .iter()
            .filter_map(|e| self.breaker_open_until(*e).map(|t| (*e, t)))
            .collect()
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Consulta `GET /market/actions?q=<método>&qp=<json>` (sem login) e devolve o campo `data`.
    pub async fn action(&self, ep: Endpoint, method: &str, qp: &Value) -> Result<Value> {
        let req = self
            .http
            .get(self.url("/market/actions"))
            .query(&[("q", method), ("qp", &qp.to_string())])
            .header("x-valve-request-type", "queryAction")
            .header("accept", "application/json, text/javascript, */*");
        let (_, body) = self.send(ep, req, false).await?;
        let mut v: Value =
            serde_json::from_slice(&body).map_err(|e| Error::Parse(format!("{method}: {e}")))?;
        match v.get_mut("data") {
            Some(d) if !d.is_null() => Ok(d.take()),
            _ => Err(Error::Parse(format!("{method}: resposta sem `data`"))),
        }
    }

    /// GET que devolve o corpo como JSON (ex.: `search/render`).
    pub async fn get_json(
        &self,
        ep: Endpoint,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value> {
        let req = self
            .http
            .get(self.url(path))
            .query(query)
            .header("accept", "application/json, text/javascript, */*");
        let (_, body) = self.send(ep, req, false).await?;
        serde_json::from_slice(&body).map_err(|e| Error::Parse(format!("{path}: {e}")))
    }

    /// GET que não segue redirecionamento: devolve status e `Location` (para o 302 do código G).
    pub async fn get_location(&self, ep: Endpoint, path: &str) -> Result<(u16, Option<String>)> {
        let req = self
            .http
            .get(self.url(path))
            .header("accept", "text/html,application/xhtml+xml");
        let (parts, _) = self.send(ep, req, false).await?;
        Ok((parts.status, parts.location))
    }

    /// GET de página HTML com o cookie de login (se houver). Devolve status, `Location` e o texto.
    pub async fn get_page_auth(
        &self,
        ep: Endpoint,
        path: &str,
    ) -> Result<(u16, Option<String>, String)> {
        let req = self
            .http
            .get(self.url(path))
            .header("accept", "text/html,application/xhtml+xml");
        let (parts, body) = self.send(ep, req, true).await?;
        Ok((
            parts.status,
            parts.location,
            String::from_utf8_lossy(&body).into_owned(),
        ))
    }

    /// Núcleo: respeita breaker e limitador, anexa cookie se `auth`, classifica a resposta.
    pub(crate) async fn send(
        &self,
        ep: Endpoint,
        mut req: RequestBuilder,
        auth: bool,
    ) -> Result<(Parts, Vec<u8>)> {
        loop {
            let wait = {
                let now = Instant::now();
                let mut g = self.inner.lock().unwrap();
                if let Some(t) = g.breakers[ep.idx()].check(now) {
                    return Err(Error::BreakerOpen {
                        endpoint: ep,
                        until: unix_in(t - now),
                    });
                }
                g.limiter.try_acquire(now).err()
            };
            match wait {
                None => break,
                Some(w) => tokio::time::sleep(w).await,
            }
        }
        if auth && let Some(c) = self.cookie() {
            req = req.header(COOKIE, c.header());
        }
        let result = req.send().await;
        let now = Instant::now();
        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                self.inner.lock().unwrap().breakers[ep.idx()].on_fail(now);
                return Err(Error::Transport(e.without_url().to_string()));
            }
        };
        let status = resp.status();
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut g = self.inner.lock().unwrap();
            g.limiter.on_429(now, retry);
            let until = g
                .limiter
                .stats(now)
                .paused_until
                .unwrap_or_else(|| unix_in(PAUSE_START));
            return Err(Error::RateLimited { until });
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        {
            let mut g = self.inner.lock().unwrap();
            g.limiter.on_ok();
            if status.is_server_error() || status == StatusCode::FORBIDDEN {
                g.breakers[ep.idx()].on_fail(now);
            } else {
                g.breakers[ep.idx()].on_ok();
            }
        }
        if status.is_server_error() || status == StatusCode::FORBIDDEN {
            return Err(Error::Http {
                endpoint: ep,
                status: status.as_u16(),
            });
        }
        Ok((
            Parts {
                status: status.as_u16(),
                location,
            },
            body.to_vec(),
        ))
    }

    /// Para o `buy.rs` (outra etapa): POST com cookie, pelo mesmo limitador e breaker.
    pub(crate) fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, self.url(path))
    }
}

pub(crate) struct Parts {
    pub status: u16,
    pub location: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode as AxStatus, routing::get};

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn limiter_spaces_queries_by_gap() {
        let t = t0();
        let mut l = Limiter::new(t);
        assert!(l.try_acquire(t).is_ok());
        assert_eq!(
            l.try_acquire(t + Duration::from_secs(1)),
            Err(Duration::from_secs(2))
        );
        assert!(l.try_acquire(t + Duration::from_secs(3)).is_ok());
    }

    #[test]
    fn limiter_shortens_5pct_per_30min_with_floor() {
        let t = t0();
        let mut l = Limiter::new(t);
        assert_eq!(l.stats(t + Duration::from_secs(30 * 60)).gap_ms, 2850);
        assert_eq!(l.stats(t + Duration::from_secs(60 * 60)).gap_ms, 2707); // 2850 × 0,95 = 2707,5
        // depois de muitas horas fica no piso de 1,5 s
        assert_eq!(l.stats(t + Duration::from_secs(48 * 3600)).gap_ms, 1500);
    }

    #[test]
    fn limiter_429_pauses_doubles_and_lengthens() {
        let t = t0();
        let mut l = Limiter::new(t);
        l.on_429(t, None);
        let s = l.stats(t);
        assert_eq!(s.gap_ms, 4500);
        assert!(s.paused_until.is_some());
        assert_eq!(
            l.try_acquire(t + Duration::from_secs(30)),
            Err(Duration::from_secs(30))
        );
        // outro 429 dentro da pausa não avança o degrau
        l.on_429(t + Duration::from_secs(10), None);
        assert_eq!(l.stats(t).gap_ms, 4500);
        // fim da pausa; novo 429 dobra: 120 s
        let t2 = t + Duration::from_secs(61);
        assert!(l.try_acquire(t2).is_ok());
        l.on_429(t2, None);
        assert_eq!(
            l.try_acquire(t2 + Duration::from_secs(119)),
            Err(Duration::from_secs(1))
        );
        // degraus: 60, 120, 240, 480, 600, 600
        let mut now = t2;
        let mut lens = vec![];
        for _ in 0..5 {
            now += Duration::from_secs(700);
            l.on_429(now, None);
            lens.push(l.paused_until.unwrap().duration_since(now).as_secs());
        }
        assert_eq!(lens, vec![240, 480, 600, 600, 600]);
        // resposta boa volta o degrau a 60 s
        l.on_ok();
        now += Duration::from_secs(700);
        l.on_429(now, None);
        assert_eq!(l.paused_until.unwrap().duration_since(now).as_secs(), 60);
    }

    #[test]
    fn limiter_counts_queries_per_minute() {
        let t = t0();
        let mut l = Limiter::new(t);
        for i in 0..5 {
            assert!(l.try_acquire(t + Duration::from_secs(3 * i)).is_ok());
        }
        assert_eq!(l.stats(t + Duration::from_secs(14)).queries_per_min, 5);
        assert_eq!(l.stats(t + Duration::from_secs(63)).queries_per_min, 4); // t=0 saiu da janela de 60 s
    }

    #[test]
    fn breaker_opens_after_three_and_doubles() {
        let t = t0();
        let mut b = Breaker::new();
        b.on_fail(t);
        b.on_fail(t);
        assert!(b.check(t).is_none());
        b.on_fail(t);
        assert_eq!(
            b.check(t + Duration::from_secs(1)),
            Some(t + Duration::from_secs(30))
        );
        // passado o prazo, deixa uma prova; falhar de novo reabre com o dobro
        let t2 = t + Duration::from_secs(31);
        assert!(b.check(t2).is_none());
        b.on_fail(t2);
        assert_eq!(b.check(t2), Some(t2 + Duration::from_secs(60)));
        b.on_ok();
        assert!(b.check(t2).is_none());
    }

    #[test]
    fn cookie_debug_does_not_leak() {
        let c = Cookie {
            steam_login_secure: "segredo123".into(),
            sessionid: None,
        };
        assert!(!format!("{c:?}").contains("segredo"));
    }

    // gap de 100 ms para os testes de rede não dormirem 3 s por consulta
    fn fast(base: &str) -> SteamClient {
        let c = SteamClient::with_base(base).unwrap();
        c.inner.lock().unwrap().limiter.gap = Duration::from_millis(100);
        c
    }

    async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn action_sends_header_and_unwraps_data_then_429_pauses() {
        let app = Router::new()
            .route(
                "/market/actions",
                get(|h: axum::http::HeaderMap, q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    assert_eq!(h.get("x-valve-request-type").unwrap(), "queryAction");
                    assert!(h.get("cookie").is_none(), "leitura anônima não leva cookie");
                    if q.get("q").map(String::as_str) == Some("Limit") {
                        return (AxStatus::TOO_MANY_REQUESTS, String::new());
                    }
                    (AxStatus::OK, r#"{"data":{"ok":true},"metrics":{}}"#.to_string())
                }),
            );
        let base = serve(app).await;
        let c = fast(&base);
        c.set_cookie(Some(Cookie {
            steam_login_secure: "x".into(),
            sessionid: None,
        }));
        let d = c
            .action(Endpoint::Listings, "Ok", &serde_json::json!([1]))
            .await
            .unwrap();
        assert_eq!(d["ok"], true);
        assert!(c.stats().last_query_at.is_some());
        let e = c
            .action(Endpoint::History, "Limit", &serde_json::json!([1]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::RateLimited { .. }));
        let s = c.stats();
        assert!(s.paused_until.is_some());
        assert_eq!(s.gap_ms, 150);
    }

    #[tokio::test]
    async fn repeated_5xx_opens_only_that_endpoint() {
        let app = Router::new().route("/boom", get(|| async { AxStatus::BAD_GATEWAY }));
        let base = serve(app).await;
        let c = fast(&base);
        for _ in 0..3 {
            let e = c
                .get_json(Endpoint::History, "/boom", &[])
                .await
                .unwrap_err();
            assert!(matches!(e, Error::Http { status: 502, .. }));
        }
        let e = c
            .get_json(Endpoint::History, "/boom", &[])
            .await
            .unwrap_err();
        assert!(matches!(
            e,
            Error::BreakerOpen {
                endpoint: Endpoint::History,
                ..
            }
        ));
        assert!(c.breaker_open_until(Endpoint::Listings).is_none());
    }
}
