//! HTTP da Steam: cookie trocável, ritmo por grupo de leitura e breaker por endpoint.
//!
//! Ritmo (docs/projeto.md, "Caça → Ritmo"): começa em 1 consulta a cada 6 s (também o piso); cada 10 min sem
//! 429 encurta 15 %; um 429 pausa 60 s (dobrando até 30 min) e alonga o intervalo em 50 %.
//! Actions, interface antiga/busca/livro e sessão/ações do usuário têm pausas independentes, com espaçamento
//! global de pelo menos 3 s. Os breakers são por endpoint (conhecimento 2): falhas
//! repetidas que não são 429 (5xx, rede) abrem só o breaker daquele endpoint.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{COOKIE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Method, RequestBuilder, StatusCode, redirect};
use serde_json::Value;

pub const BASE: &str = "https://steamcommunity.com";
/// Moeda que o painel espera (BRL).
pub const CURRENCY_BRL: i32 = 7;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

// 10 consultas/min: com 3 s (20/min) e 4 s (15/min) a Steam deu 429 por horas ao vivo em 04/11; o robô
// antigo leu anúncios a ~6–12/min por 4–10 h sem nenhum 429 (logs de run*.log).
const BASE_GAP: Duration = Duration::from_secs(6);
// Piso = base (acima da regra de ≥ 3 s): encurtar só desfaz o alongamento depois de 429.
const MIN_GAP: Duration = BASE_GAP;
// Teto do intervalo depois de vários 429 seguidos (acréscimo meu: o desenho não limita o alongamento).
const MAX_GAP: Duration = Duration::from_secs(60);
// Volta da pausa: cada 10 min sem 429 encurta 15 % (de 60 s a 6 s em ~2,5 h). Com 5 % a cada 30 min, depois
// de uma série de 429 o intervalo ficava perto de 60 s por quase um dia (visto ao vivo em 04/11).
const SHORTEN_EVERY: Duration = Duration::from_secs(10 * 60);
const SHORTEN_FACTOR: f64 = 0.85;
// Ritmo aprendido: o 1º 429 de uma série sobe o piso para 1,25× o intervalo em uso, e cada 6 h sem 429 desce
// um degrau. Em 04–05/11, a 6 s fixos a Steam bloqueava a cada 1–2 h e cada série custava até 2 h paradas; o
// encurtamento voltava aos 6 s em ~1,5 h e batia no mesmo limite de novo.
const FLOOR_STEP: f64 = 1.25;
const FLOOR_RELAX_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const FLOOR_MAX: Duration = Duration::from_secs(20);
const PAUSE_START: Duration = Duration::from_secs(60);
// 30 min: em 04/11 a Steam seguiu dando 429 por horas com uma tentativa a cada 10 min; uma consulta depois de
// minutos quietos passou. Tentar menos deixa a penalidade esfriar.
const PAUSE_MAX: Duration = Duration::from_secs(30 * 60);
// O degrau da pausa só volta a 60 s depois de tanto tempo sem 429: uma resposta boa de outro
// endpoint entre dois 429 não pode zerar a escalada.
const PAUSE_RESET_AFTER: Duration = Duration::from_secs(60 * 60); // maior que PAUSE_MAX, senão a escalada zera sozinha
// Leitura logada levou 429: anúncios e histórico vão sem login por este tempo. Em 04/11 o logado tomou 429
// por horas enquanto a mesma consulta sem login, do mesmo IP, voltava 200 (a Steam segura a conta).
/// Espaço mínimo entre duas consultas quaisquer pela principal, somando os baldes (nunca rajada).
const MIN_SPACING: Duration = Duration::from_secs(3);
const ANON_AFTER_429: Duration = Duration::from_secs(6 * 60 * 60);

/// Saída extra: proxy local que sai por outro IP público (o `saida-celular` deste PC, pela Starlink). Leva só
/// leitura de mercado, sem login, com limitador próprio. Desligado, ele recusa na hora e a saída descansa
/// `ALT_DOWN`; ligado fora da rede certa, responde 503 e dá no mesmo.
pub const ALT_PROXY: &str = "http://127.0.0.1:1056";
const ALT_DOWN: Duration = Duration::from_secs(5 * 60);
const MAX_BODY: usize = 16 * 1024 * 1024;

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
    /// api.steampowered.com (login por QR e renovação): ritmo de controle, breaker próprio.
    Auth,
    /// Livro de pedidos de compra (`/market/orderbook`).
    Book,
    /// Leituras pela interface antiga, selecionada pelo cookie público `bMarketOptOut`.
    Legacy,
}

impl Endpoint {
    const ALL: [Endpoint; 8] = [
        Self::Listings,
        Self::History,
        Self::Search,
        Self::Session,
        Self::Buy,
        Self::Auth,
        Self::Book,
        Self::Legacy,
    ];
    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Debug)]
pub enum Error {
    BudgetExhausted,
    /// 429 da Steam: o grupo da consulta está pausado até `until` (segundos Unix).
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
    /// A action respondeu 200 com data nula; não significa página vazia.
    ActionUnavailable {
        endpoint: Endpoint,
    },
    /// Resposta fora do formato esperado.
    Parse(String),
    /// Cookie ausente, expirado ou recusado.
    SessionExpired,
    /// A Steam mandou o /market/ para fora da verificação de elegibilidade (conta sem acesso ao Mercado).
    MarketBlocked,
    /// A conta/resposta está em outra moeda que não BRL.
    WrongCurrency {
        got: i32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetExhausted => write!(f, "cota de exploração esgotada"),
            Self::RateLimited { until } => write!(f, "Steam respondeu 429; pausado até {until}"),
            Self::BreakerOpen { endpoint, until } => write!(f, "{endpoint:?} em pausa até {until}"),
            Self::Http { endpoint, status } => write!(f, "{endpoint:?}: HTTP {status}"),
            Self::Csfloat { status } => write!(f, "CSFloat: HTTP {status}"),
            Self::Transport(e) => write!(f, "falha de rede: {e}"),
            Self::ActionUnavailable { endpoint } => write!(
                f,
                "{endpoint:?}: Steam devolveu data nula após tentativa de recuperação"
            ),
            Self::Parse(e) => write!(f, "resposta inesperada: {e}"),
            Self::SessionExpired => write!(f, "sessão da Steam ausente ou expirada"),
            Self::MarketBlocked => write!(f, "a Steam não liberou o Mercado para esta conta"),
            Self::WrongCurrency { got } => write!(f, "moeda {got} em vez de BRL (7)"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn research_outcome<T>(result: &Result<T>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(Error::RateLimited { .. }) => "rate_limited",
        Err(_) => "error",
    }
}

/// Cookie de login. Nunca aparece em log nem em `Debug`.
#[derive(Clone, PartialEq)]
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
    /// Última resposta que não foi 429: diz há quanto tempo a Steam barra a caça.
    pub last_ok_at: Option<i64>,
    /// Última resposta boa do livro de ofertas ou da busca (o outro balde).
    pub book_ok_at: Option<i64>,
}

/// Respostas por saída desde a última leitura (`take_calls`): `ok` é tudo que não foi 429.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Calls {
    pub ok: u32,
    pub limited: u32,
}

// ---------------------------------------------------------------------------------------------
// Limitador: lógica pura, com o relógio injetado para testar sem dormir.

pub(crate) struct Limiter {
    gap: Duration,
    next_at: Instant,
    paused_until: Option<Instant>,
    pause_len: Duration,
    last_429: Option<Instant>,
    /// Último 429 ou último encurtamento; o próximo encurta 10 min depois.
    last_adjust: Instant,
    last_query: Option<Instant>,
    recent: VecDeque<Instant>,
    /// Piso aprendido no 1º 429 da última série e a hora do último 429 (ver `FLOOR_STEP`).
    floor_at_429: Duration,
    floor_since: Option<Instant>,
    last_ok: Option<Instant>,
}

impl Limiter {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            gap: BASE_GAP,
            next_at: now,
            paused_until: None,
            pause_len: PAUSE_START,
            last_429: None,
            last_adjust: now,
            last_query: None,
            recent: VecDeque::new(),
            floor_at_429: MIN_GAP,
            floor_since: None,
            last_ok: None,
        }
    }

    /// Menor intervalo agora: o aprendido no último 429, um degrau abaixo a cada 6 h quietas.
    fn floor(&self, now: Instant) -> Duration {
        let Some(t) = self.floor_since else {
            return MIN_GAP;
        };
        let steps = now.saturating_duration_since(t).as_secs() / FLOOR_RELAX_EVERY.as_secs();
        self.floor_at_429
            .div_f64(FLOOR_STEP.powi(steps.min(20) as i32))
            .max(MIN_GAP)
    }

    fn shorten(&mut self, now: Instant) {
        let floor = self.floor(now);
        while now.duration_since(self.last_adjust) >= SHORTEN_EVERY {
            self.gap = self.gap.mul_f64(SHORTEN_FACTOR).max(floor);
            self.last_adjust += SHORTEN_EVERY;
        }
    }

    /// Fim da pausa por 429 em curso.
    fn pause_end(&self, now: Instant) -> Option<Instant> {
        self.paused_until.filter(|p| *p > now)
    }

    /// `Ok` toma a vaga (e conta a consulta); `Err(espera)` diz quanto falta.
    pub(crate) fn try_acquire(&mut self, now: Instant) -> std::result::Result<(), Duration> {
        self.try_reserve(now)?;
        self.note_query(now);
        Ok(())
    }

    fn try_reserve(&mut self, now: Instant) -> std::result::Result<(), Duration> {
        self.shorten(now);
        let free_at = self
            .paused_until
            .map_or(self.next_at, |p| p.max(self.next_at));
        if now < free_at {
            return Err(free_at - now);
        }
        self.paused_until = None;
        self.next_at = now + self.gap;
        Ok(())
    }

    fn note_query(&mut self, now: Instant) {
        self.last_query = Some(now);
        self.recent.push_back(now);
    }

    /// 429: pausa e alonga 50 %. Durante a pausa, só estende um `Retry-After` maior, sem avançar o degrau.
    pub(crate) fn on_429(&mut self, now: Instant, retry_after: Option<Duration>) {
        if let Some(end) = self.pause_end(now) {
            let requested = now + retry_after.unwrap_or_default();
            self.paused_until = Some(end.max(requested));
            return;
        }
        // 60 min sem 429 (mesmo sem nenhuma resposta no meio): a escalada recomeça.
        if self
            .last_429
            .is_some_and(|t| now.duration_since(t) >= PAUSE_RESET_AFTER)
        {
            self.pause_len = PAUSE_START;
        }
        // 1º 429 da série: o intervalo em uso passou do que a Steam aguenta agora. Todo 429 recomeça as 6 h.
        if self
            .last_429
            .is_none_or(|t| now.duration_since(t) >= PAUSE_RESET_AFTER)
        {
            self.floor_at_429 = self.gap.mul_f64(FLOOR_STEP).clamp(MIN_GAP, FLOOR_MAX);
        }
        self.floor_since = Some(now);
        let pause = self.pause_len.max(retry_after.unwrap_or_default());
        self.paused_until = Some(now + pause);
        self.pause_len = (self.pause_len * 2).min(PAUSE_MAX);
        self.last_429 = Some(now);
        self.gap = self.gap.mul_f64(1.5).min(MAX_GAP);
        self.last_adjust = now;
    }

    /// Resposta que não foi 429: o degrau da pausa volta ao início se o último 429 já tem 60 min.
    pub(crate) fn on_ok(&mut self, now: Instant) {
        self.last_ok = Some(now);
        if self
            .last_429
            .is_none_or(|t| now.duration_since(t) >= PAUSE_RESET_AFTER)
        {
            self.pause_len = PAUSE_START;
        }
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
            last_ok_at: self.last_ok.map(unix_of),
            book_ok_at: None,
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

type PaceHook = Arc<dyn Fn(String) + Send + Sync>;

struct Inner {
    limiter: Limiter,
    /// Sessão e ações do usuário não herdam a pausa das consultas de anúncios.
    control: Limiter,
    breakers: [Breaker; Endpoint::ALL.len()],
    cookie: Option<Cookie>,
    /// Cookies que a Steam setou nas respostas autenticadas (`webTradeEligibility` etc.); valem enquanto
    /// o login for o mesmo e vão em toda requisição autenticada, inclusive a compra.
    extra: Vec<(String, String)>,
    on_pace: Option<PaceHook>,
    /// Até quando `action` lê sem login (depois de um 429 logado).
    anon_reads_until: Option<Instant>,
    /// Ritmo da saída extra; outro IP não garante outra cota. Fica só em memória.
    // shortcut: reiniciar volta a 6 s nessa saída; se a Steam castigar esse IP por horas, salvar como `steam_pace`
    alt_limiter: Limiter,
    /// Saída extra recusou ou falhou na rede: fica de lado até aqui.
    alt_down_until: Option<Instant>,
    /// Interface antiga, livro de ofertas e busca: medidos disponíveis com actions em 429 (06/11; com
    /// `/market/actions` em 429 havia horas, `orderbook`, `search/render` e `priceoverview` respondiam 200).
    // shortcut: ritmo só em memória (reiniciar volta a 6 s); salvar como `steam_pace` se esse balde também levar horas
    side: Limiter,
    /// Última página legada validada: anúncios e histórico seguem mesmo com `/actions` em pausa.
    legacy_ok: Option<Instant>,
    /// Próxima vaga de qualquer consulta pela principal, de qualquer balde (`MIN_SPACING`).
    next_any: Instant,
    spacing: Duration,
    /// [principal, extra, livro e busca], para o balanço de 10 min no log.
    calls: [Calls; 3],
    sent: u64,
}

/// Endpoints do balde do livro de ofertas e da busca (`Inner::side`).
fn side(ep: Endpoint) -> bool {
    matches!(ep, Endpoint::Book | Endpoint::Search | Endpoint::Legacy)
}

impl Inner {
    fn bucket(&mut self, ep: Endpoint) -> &mut Limiter {
        if matches!(ep, Endpoint::Session | Endpoint::Buy | Endpoint::Auth) {
            &mut self.control
        } else if side(ep) {
            &mut self.side
        } else {
            &mut self.limiter
        }
    }

    fn alt_held(&self, now: Instant) -> bool {
        self.alt_down_until.is_some_and(|t| now < t) || self.alt_limiter.pause_end(now).is_some()
    }
}

pub struct SteamClient {
    http: reqwest::Client,
    /// Saída extra (`ALT_PROXY`); `None` nos testes e enquanto ninguém liga.
    alt: Option<reqwest::Client>,
    alt_public_only: bool,
    base: String,
    inner: Mutex<Inner>,
    research_db: Mutex<Option<crate::db::Db>>,
    main_send_pace: tokio::sync::Mutex<SendPace>,
    alt_send_pace: tokio::sync::Mutex<SendPace>,
}

#[derive(Default)]
struct SendPace {
    last_any: Option<Instant>,
    last_bucket: [Option<Instant>; 3],
}

enum Slot {
    Main,
    Alt,
    Wait(Duration),
}

fn http_builder() -> reqwest::ClientBuilder {
    // Sem `accept-language`: em 04/11 o mesmo `/market/actions`, do mesmo IP e no mesmo minuto, deu 429
    // com `en-US,…` e 200 sem o cabeçalho, repetido em itens diferentes. Sem ele a Steam respondeu em inglês.
    let mut h = HeaderMap::new();
    h.insert(
        "referer",
        HeaderValue::from_static("https://steamcommunity.com/market/"),
    );
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(h)
        .redirect(redirect::Policy::none())
        .timeout(Duration::from_secs(30))
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
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
        Ok(Self {
            research_db: Mutex::new(None),
            main_send_pace: tokio::sync::Mutex::new(SendPace::default()),
            alt_send_pace: tokio::sync::Mutex::new(SendPace::default()),
            http: http_builder().build()?,
            alt: None,
            alt_public_only: false,
            base: base.trim_end_matches('/').to_string(),
            inner: Mutex::new(Inner {
                limiter: Limiter::new(Instant::now()),
                control: Limiter::new(Instant::now()),
                breakers: [Breaker::new(); Endpoint::ALL.len()],
                cookie: None,
                extra: Vec::new(),
                on_pace: None,
                anon_reads_until: None,
                alt_limiter: Limiter::new(Instant::now()),
                side: Limiter::new(Instant::now()),
                legacy_ok: None,
                next_any: Instant::now(),
                spacing: MIN_SPACING,
                calls: [Calls::default(); 3],
                sent: 0,
                alt_down_until: None,
            }),
        })
    }

    pub fn set_research_db(&self, db: crate::db::Db) {
        *self.research_db.lock().unwrap() = Some(db);
    }

    // O banco pode atrasar reservas diferentes; o intervalo precisa valer também no envio real.
    async fn pace_send(&self, ep: Endpoint, alt: bool) -> Result<()> {
        let gate = if alt {
            &self.alt_send_pace
        } else {
            &self.main_send_pace
        };
        let bucket = if matches!(ep, Endpoint::Session | Endpoint::Buy | Endpoint::Auth) {
            2
        } else if side(ep) {
            1
        } else {
            0
        };
        loop {
            let at = {
                let mut pace = gate.lock().await;
                let mut inner = self.inner.lock().unwrap();
                let now = Instant::now();
                if let Some(t) = inner.breakers[ep.idx()].check(now) {
                    return Err(Error::BreakerOpen {
                        endpoint: ep,
                        until: unix_in(t - now),
                    });
                }
                let (spacing, gap) = if alt {
                    if let Some(t) = inner
                        .alt_limiter
                        .pause_end(now)
                        .into_iter()
                        .chain(inner.alt_down_until.filter(|t| *t > now))
                        .max()
                    {
                        return Err(Error::RateLimited {
                            until: unix_in(t - now),
                        });
                    }
                    (inner.spacing, inner.alt_limiter.gap)
                } else {
                    if let Some(t) = inner.bucket(ep).pause_end(now) {
                        return Err(Error::RateLimited {
                            until: unix_in(t - now),
                        });
                    }
                    let spacing = inner.spacing;
                    (spacing, inner.bucket(ep).gap)
                };
                let any = pace.last_any.map_or(now, |last| last + spacing);
                let bucket_at = pace.last_bucket[bucket].map_or(now, |last| last + gap);
                let at = any.max(bucket_at);
                if at <= now {
                    // O contador representa transporte iniciado, não vaga reservada.
                    inner.sent += 1;
                    if alt {
                        inner.alt_limiter.note_query(now);
                    }
                    pace.last_any = Some(now);
                    pace.last_bucket[bucket] = Some(now);
                    return Ok(());
                }
                at
            };
            tokio::time::sleep_until(at.into()).await;
        }
    }

    async fn reserve_read(
        &self,
        ep: Endpoint,
        route: &'static str,
    ) -> Result<Option<(crate::db::Db, i64)>> {
        if matches!(ep, Endpoint::Session | Endpoint::Buy | Endpoint::Auth) {
            return Ok(None);
        }
        let db = self.research_db.lock().unwrap().clone();
        let endpoint = match ep {
            Endpoint::Listings => "listings",
            Endpoint::History => "history",
            Endpoint::Search => "search",
            Endpoint::Book => "book",
            Endpoint::Legacy => "legacy",
            _ => unreachable!(),
        };
        crate::research::reserve(db, endpoint, route)
            .await
            .map_err(|e| match e {
                crate::research::ReserveError::Budget => Error::BudgetExhausted,
                crate::research::ReserveError::Database(e) => {
                    Error::Transport(format!("registro de pesquisa: {e:#}"))
                }
            })
    }

    /// Liga a saída extra pelo proxy HTTP em `url` (ver `ALT_PROXY`).
    pub fn with_alt_proxy(self, url: &str) -> anyhow::Result<Self> {
        Ok(self.with_alt(http_builder().proxy(reqwest::Proxy::all(url)?).build()?))
    }

    /// Saída extra somente para livro e anúncios públicos da interface antiga.
    pub fn with_public_alt_proxy(self, url: &str) -> anyhow::Result<Self> {
        let mut c = self.with_alt_proxy(url)?;
        c.alt_public_only = true;
        Ok(c)
    }

    /// A saída pública aceita uma reserva para os próximos 6 s, sem pausa nem falha de rede.
    pub fn public_alt_available(&self) -> bool {
        if !self.alt_public_only || self.alt.is_none() {
            return false;
        }
        let now = Instant::now();
        let g = self.inner.lock().unwrap();
        g.alt_down_until.is_none_or(|t| now >= t)
            && g.alt_limiter.pause_end(now).is_none()
            && g.alt_limiter.next_at <= now + BASE_GAP
            && g.breakers[Endpoint::Legacy.idx()]
                .open_until
                .is_none_or(|t| now >= t)
    }

    /// Há uma vaga imediata na saída pública enquanto a leitura nativa espera na principal.
    pub fn prefer_public_listings(&self) -> bool {
        if !self.alt_public_only || self.alt.is_none() {
            return false;
        }
        let Ok(pace) = self.alt_send_pace.try_lock() else {
            return false;
        };
        let now = Instant::now();
        let g = self.inner.lock().unwrap();
        !g.alt_held(now)
            && g.alt_limiter.next_at <= now
            && pace.last_any.is_none_or(|t| t + g.spacing <= now)
            && pace.last_bucket[1].is_none_or(|t| t + g.alt_limiter.gap <= now)
            && (g.next_any > now || g.limiter.next_at > now || g.limiter.pause_end(now).is_some())
    }

    fn with_alt(mut self, http: reqwest::Client) -> Self {
        self.alt = Some(http);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_public_alt_for_test(self, http: reqwest::Client) -> Self {
        let mut c = self.with_alt(http);
        c.alt_public_only = true;
        c
    }

    /// Troca (ou remove) o cookie em tempo de execução. Vale para as próximas requisições `auth`.
    pub fn set_cookie(&self, cookie: Option<Cookie>) {
        let mut g = self.inner.lock().unwrap();
        // Outra conta: o prazo sem login era da anterior. Ao subir (sem cookie antes) o prazo salvo fica.
        let login = |c: &Option<Cookie>| c.as_ref().map(|c| c.steam_login_secure.clone());
        if g.cookie.is_some() && login(&g.cookie) != login(&cookie) {
            g.anon_reads_until = None;
            Self::notify_pace(&mut g, Instant::now()); // senão o prazo velho volta do banco ao reiniciar
        }
        g.cookie = cookie;
        g.extra.clear();
    }

    /// Grava o `sessionid` só se a sessão atual ainda for a `queried` (mesmo cookie), sob o mesmo lock:
    /// quem trocou ou apagou a sessão durante a consulta não é desfeito.
    pub(crate) fn adopt_sessionid(&self, queried: &Cookie, sessionid: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        match g.cookie.as_mut() {
            Some(c) if c == queried => {
                c.sessionid = Some(sessionid.to_string());
                true
            }
            _ => false,
        }
    }

    /// Guarda os `Set-Cookie` de uma resposta autenticada, menos o login e o `sessionid` (esses mandam
    /// em outro lugar). Cookie apagado (`Max-Age=0` ou valor vazio) sai do jar. Teto de 16 nomes.
    fn keep_cookies(&self, set_cookie: &[String], sent_by: Option<&str>) {
        let mut g = self.inner.lock().unwrap();
        if sent_by.is_none() || g.cookie.as_ref().map(|c| c.steam_login_secure.as_str()) != sent_by
        {
            return;
        }
        for line in set_cookie {
            let mut parts = line.split(';');
            let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if name.is_empty()
                || name.eq_ignore_ascii_case("steamLoginSecure")
                || name == "sessionid"
            {
                continue;
            }
            let deleted =
                value.is_empty() || parts.any(|a| a.trim().eq_ignore_ascii_case("max-age=0"));
            g.extra.retain(|(k, _)| k != name);
            if !deleted && g.extra.len() < 16 {
                g.extra.push((name.to_string(), value.to_string()));
            }
        }
    }

    /// Caminho (com query) de um `Location` que aponta para a própria Steam Community; `None` se for
    /// de outro host.
    pub(crate) fn local_path(&self, location: &str) -> Option<String> {
        if location.starts_with('/') {
            return Some(location.to_string());
        }
        [self.base.as_str(), BASE]
            .iter()
            .find_map(|b| location.strip_prefix(*b))
            .filter(|p| p.starts_with('/'))
            .map(str::to_string)
    }

    pub fn cookie(&self) -> Option<Cookie> {
        self.inner.lock().unwrap().cookie.clone()
    }

    /// Só para teste: encurta o intervalo do limitador para não dormir 3 s por consulta.
    #[cfg(test)]
    pub(crate) fn set_gap_for_test(&self, gap: Duration) {
        let mut g = self.inner.lock().unwrap();
        g.limiter.gap = gap;
        g.side.gap = gap;
        g.control.gap = gap;
        g.spacing = gap;
    }

    /// `true` = uma consulta desse endpoint agora falharia sem ir à Steam: o balde dele está em pausa por 429 e a
    /// saída extra não pode ler. A caça usa isto para não esperar pausas de até 30 min no meio de um passo.
    pub fn blocked(&self, ep: Endpoint) -> bool {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        g.bucket(ep).pause_end(now).is_some() && !(self.alt_reads(ep) && !g.alt_held(now))
    }

    /// Pausa/breaker da principal; leitura autenticada não pode contar com a saída extra.
    pub fn main_blocked(&self, ep: Endpoint) -> bool {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        g.bucket(ep).pause_end(now).is_some() || g.breakers[ep.idx()].check(now).is_some()
    }

    /// Consultas enviadas desde que o cliente subiu (todas as saídas e baldes).
    pub fn sent(&self) -> u64 {
        self.inner.lock().unwrap().sent
    }

    /// Consultas somam as duas saídas; a caça só está em pausa se a extra também não pode ler.
    pub fn stats(&self) -> Stats {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap();
        let mut s = g.limiter.stats(now);
        let side = g.side.stats(now);
        let control = g.control.stats(now);
        s.queries_per_min += side.queries_per_min;
        s.queries_per_min += control.queries_per_min;
        s.last_query_at = s.last_query_at.max(side.last_query_at);
        s.last_query_at = s.last_query_at.max(control.last_query_at);
        s.book_ok_at = side.last_ok_at;
        if self.alt.is_some() {
            let a = g.alt_limiter.stats(now);
            s.queries_per_min += a.queries_per_min;
            s.last_query_at = s.last_query_at.max(a.last_query_at);
            if self.alt_public_only {
                s.book_ok_at = s.book_ok_at.max(a.last_ok_at);
            } else {
                s.last_ok_at = s.last_ok_at.max(a.last_ok_at);
            }
            if !self.alt_public_only && g.alt_down_until.is_none_or(|t| now >= t) {
                s.paused_until = s.paused_until.zip(a.paused_until).map(|(m, x)| m.min(x));
            }
        }
        if let Some(t) = g.legacy_ok {
            let age = now.saturating_duration_since(t);
            s.last_ok_at = s
                .last_ok_at
                .max(Some(unix_in(Duration::ZERO) - age.as_secs() as i64));
            if age < Duration::from_secs(600)
                && (g.side.pause_end(now).is_none() || (self.alt_public_only && !g.alt_held(now)))
                && g.breakers[Endpoint::Legacy.idx()].check(now).is_none()
            {
                s.paused_until = None;
            }
        }
        s
    }

    /// Respostas de cada saída (e do balde do livro) desde a última chamada; zera a contagem.
    pub fn take_calls(&self) -> [Calls; 3] {
        std::mem::take(&mut self.inner.lock().unwrap().calls)
    }

    /// Avisa o gancho de ritmo ainda sob o lock: a ordem dos avisos é a ordem dos 429 (o gancho não pode
    /// chamar o cliente).
    fn notify_pace(g: &mut Inner, now: Instant) {
        if let Some(f) = g.on_pace.clone() {
            f(Self::saved_pace(g, now));
        }
    }

    /// Pausa e intervalo salvos depois do último 429 (`fim da pausa Unix|intervalo ms|próxima pausa ms|fim
    /// das leituras sem login Unix|último encurtamento Unix|piso ms|último 429 Unix|última resposta sem 429 Unix`). Sem pausa ativa, o 1º campo é a hora do último 429: ao subir, a
    /// escalada só zera 60 min depois dele.
    fn saved_pace(g: &mut Inner, now: Instant) -> String {
        let s = g.limiter.stats(now);
        let last_429 = g
            .limiter
            .last_429
            .map(|t| unix_in(Duration::ZERO) - now.duration_since(t).as_secs() as i64);
        let anon = g
            .anon_reads_until
            .filter(|t| *t > now)
            .map_or(0, |t| unix_in(t - now));
        let adjusted =
            unix_in(Duration::ZERO) - now.duration_since(g.limiter.last_adjust).as_secs() as i64;
        let floor_since = g.limiter.floor_since.map_or(0, |t| {
            unix_in(Duration::ZERO) - now.saturating_duration_since(t).as_secs() as i64
        });
        let last_ok = g.limiter.last_ok.map_or(0, |t| {
            unix_in(Duration::ZERO) - now.saturating_duration_since(t).as_secs() as i64
        });
        format!(
            "{}|{}|{}|{anon}|{adjusted}|{}|{floor_since}|{last_ok}",
            s.paused_until.or(last_429).unwrap_or(0),
            s.gap_ms,
            g.limiter.pause_len.as_millis(),
            g.limiter.floor_at_429.as_millis()
        )
    }

    /// Retoma o que `saved_pace` gravou antes de um reinício: reiniciar não zera a cota da Steam.
    /// Texto que não é desse formato é ignorado.
    pub fn restore_pace(&self, saved: &str) {
        let mut parts = saved.split('|').map(str::parse::<i64>);
        let (Some(Ok(until)), Some(Ok(gap)), pause) = (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let anon = parts.next();
        // shortcut: texto antigo (4 campos) encurta a partir do 1º campo; se foi salvo num 429 logado ou numa troca de
        // conta, encurta duas vezes uma única vez (nunca abaixo do piso de 6 s). Some no próximo 429, que grava o 5º.
        let adjusted = parts.next().and_then(|r| r.ok()).unwrap_or(until);
        let floor = parts.next().and_then(|r| r.ok());
        let floor_since = parts.next().and_then(|r| r.ok());
        let last_ok = parts.next().and_then(|r| r.ok()).filter(|t| *t > 0);
        let now = Instant::now();
        let ms = |v: i64| Duration::from_millis(v.max(0) as u64);
        let g = &mut *self.inner.lock().unwrap();
        let anon_left = anon.and_then(|r| r.ok()).unwrap_or(0) - unix_in(Duration::ZERO);
        if anon_left > 0 {
            g.anon_reads_until =
                Some(now + Duration::from_secs(anon_left as u64).min(ANON_AFTER_429));
        }
        let l = &mut g.limiter;
        if let Some(t) = last_ok {
            let ago = (unix_in(Duration::ZERO) - t).max(0) as u64;
            l.last_ok = now.checked_sub(Duration::from_secs(ago));
        }
        if let (Some(f), Some(t)) = (floor, floor_since.filter(|t| *t > 0)) {
            let ago = (unix_in(Duration::ZERO) - t).max(0) as u64;
            l.floor_at_429 = ms(f).clamp(MIN_GAP, FLOOR_MAX);
            l.floor_since = now.checked_sub(Duration::from_secs(ago));
        }
        // O tempo desde o último encurtamento salvo continua encurtando; sem isso cada reinício voltava ao
        // intervalo de horas atrás e levava mais 2,5 h para sair dele (visto ao vivo em 04/11).
        let since = (unix_in(Duration::ZERO) - adjusted).max(0) as u64;
        let every = SHORTEN_EVERY.as_secs();
        l.gap = ms(gap)
            .mul_f64(SHORTEN_FACTOR.powi((since / every).min(100) as i32))
            .clamp(MIN_GAP, MAX_GAP)
            .max(l.floor(now));
        // o pedaço de 10 min já andado também conta para o próximo encurtamento
        l.last_adjust = now
            .checked_sub(Duration::from_secs(since % every))
            .unwrap_or(now);
        // a hora do último 429 vem do 7º campo; texto antigo usa o 1º (o fim da pausa, se salvo durante uma)
        let last_429 = floor_since.filter(|t| *t > 0).unwrap_or(until);
        let quiet_for = (unix_in(Duration::ZERO) - last_429).max(0) as u64;
        let quiet = quiet_for >= PAUSE_RESET_AFTER.as_secs();
        if let Some(Ok(p)) = pause.filter(|_| !quiet) {
            l.pause_len = ms(p).clamp(PAUSE_START, PAUSE_MAX);
            // A escalada segue valendo depois de subir, contada da hora salva (não do reinício).
            // shortcut: texto antigo salvo durante uma pausa conta do fim da pausa (mais cauteloso que o 429 real).
            l.last_429 = Some(
                now.checked_sub(Duration::from_secs(quiet_for))
                    .unwrap_or(now),
            );
        }
        let left = until - unix_in(Duration::ZERO);
        if left > 0 {
            l.paused_until = Some(now + Duration::from_secs(left as u64));
        }
    }

    /// Chamado (sob o lock do cliente) a cada 429 e ao trocar de conta, de qualquer endpoint, com o texto para `restore_pace` (o 4º
    /// campo é o fim das leituras sem login).
    pub fn on_pace_change(&self, f: impl Fn(String) + Send + Sync + 'static) {
        self.inner.lock().unwrap().on_pace = Some(Arc::new(f));
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

    /// Consulta `GET /market/actions?q=<método>&qp=<json>` e devolve o campo `data`. Vai logado quando há
    /// cookie, como a página do mercado aberta no navegador (pedido do Erick: ler o mercado logado), menos
    /// nas 6 h depois de um 429 logado (`ANON_AFTER_429`).
    pub async fn action(&self, ep: Endpoint, method: &str, qp: &Value) -> Result<Value> {
        self.action_at(ep, "/market/actions", method, qp).await
    }

    /// `action` numa rota própria (ex.: `/market/orderbook`, método `Load`).
    pub async fn action_at(
        &self,
        ep: Endpoint,
        route: &str,
        method: &str,
        qp: &Value,
    ) -> Result<Value> {
        for attempt in 0..2 {
            let req = self
                .http
                .get(self.url(route))
                .query(&[("q", method), ("qp", &qp.to_string())])
                .header("x-valve-request-type", "queryAction")
                .header("accept", "application/json, text/javascript, */*");
            let response = self
                .send_checked(ep, req, true, Some(envelope_problem), attempt == 0)
                .await;
            if matches!(response, Err(Error::ActionUnavailable { .. })) && attempt == 0 {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            let (_, body) = response.map_err(|e| match e {
                Error::Parse(m) => Error::Parse(format!("{method}: {m}")),
                other => other,
            })?;
            let mut v: Value = serde_json::from_slice(&body)
                .map_err(|e| Error::Parse(format!("{method}: {e}")))?;
            return match v.get_mut("data") {
                Some(d) if !d.is_null() => Ok(d.take()),
                _ => Err(Error::Parse(format!("{method}: resposta sem `data`"))),
            };
        }
        unreachable!("a segunda tentativa sempre retorna")
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

    /// Interface antiga, opção pública "sair do beta". Compartilha o ritmo de busca/livro, sem rajada.
    /// Login só no histórico; anúncios continuam públicos, sem encaminhar credenciais para a saída extra.
    pub async fn legacy_json(
        &self,
        path: &str,
        query: &[(&str, String)],
        auth: bool,
    ) -> Result<Value> {
        let mut req = self
            .http
            .get(self.url(path))
            .query(query)
            .header("accept", "application/json");
        if !auth {
            req = req.header(COOKIE, "bMarketOptOut=1");
        }
        let (parts, body) = self.send(Endpoint::Legacy, req, auth).await?;
        if parts.status != 200 {
            return Err(Error::Http {
                endpoint: Endpoint::Legacy,
                status: parts.status,
            });
        }
        serde_json::from_slice(&body)
            .map_err(|_| Error::Parse("render: resposta não é JSON".into()))
    }

    /// Faz uma tentativa pública de anúncio somente pela saída alternativa.
    /// Reserva a próxima vaga em até 6 s; livros seguintes usam a principal ou esperam outra vaga.
    /// `None` indica saída desativada, espera maior, pausa, breaker ou falha de rede/429 nela.
    /// Erros de orçamento, HTTP e JSON permanecem em `Some(Err(_))`; nunca usa a principal.
    pub async fn legacy_json_public_alt(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Option<Result<Value>> {
        let ep = Endpoint::Legacy;
        if !self.alt_public_only || self.alt.is_none() {
            return None;
        }
        let at = {
            let now = Instant::now();
            let mut g = self.inner.lock().unwrap();
            if g.breakers[ep.idx()].check(now).is_some()
                || g.alt_held(now)
                || g.alt_limiter.next_at > now + BASE_GAP
            {
                return None;
            }
            g.alt_limiter.shorten(now);
            let at = now.max(g.alt_limiter.next_at);
            g.alt_limiter.next_at = at + g.alt_limiter.gap;
            at
        };
        tokio::time::sleep_until(at.into()).await;
        let req = self
            .http
            .get(self.url(path))
            .query(query)
            .header("accept", "application/json");
        let result = self.send_alt(ep, &req, None, true).await?;
        let (parts, body) = match result {
            Ok(response) => response,
            Err(e) => return Some(Err(e)),
        };
        if parts.status != 200 {
            return Some(Err(Error::Http {
                endpoint: ep,
                status: parts.status,
            }));
        }
        Some(
            serde_json::from_slice(&body)
                .map_err(|_| Error::Parse("render: resposta não é JSON".into())),
        )
    }

    /// Só quem validou identidade, moeda e conteúdo pode declarar a leitura recuperada.
    pub fn note_legacy_ok(&self) {
        self.inner.lock().unwrap().legacy_ok = Some(Instant::now());
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
        req: RequestBuilder,
        auth: bool,
    ) -> Result<(Parts, Vec<u8>)> {
        self.send_checked(ep, req, auth, None, true).await
    }

    /// Como `send`, mas `check(status, corpo)` pode dizer que a resposta (200) não é o envelope esperado:
    /// conta como falha no breaker, não como sucesso, e vira `Error::Parse` com o diagnóstico.
    async fn send_checked(
        &self,
        ep: Endpoint,
        mut req: RequestBuilder,
        auth: bool,
        check: Option<EnvelopeCheck>,
        reset_on_success: bool,
    ) -> Result<(Parts, Vec<u8>)> {
        if !matches!(ep, Endpoint::Session | Endpoint::Buy | Endpoint::Auth) {
            crate::research::check_budget().await.map_err(|e| match e {
                crate::research::ReserveError::Budget => Error::BudgetExhausted,
                crate::research::ReserveError::Database(e) => {
                    Error::Transport(format!("registro de pesquisa: {e:#}"))
                }
            })?;
        }
        loop {
            let slot = {
                let now = Instant::now();
                let mut g = self.inner.lock().unwrap();
                if let Some(t) = g.breakers[ep.idx()].check(now) {
                    return Err(Error::BreakerOpen {
                        endpoint: ep,
                        until: unix_in(t - now),
                    });
                }
                // No modo público, usa a vaga extra primeiro, sem encaminhar histórico autenticado.
                if self.alt_public_only
                    && !(ep == Endpoint::Legacy && auth)
                    && matches!(self.alt_slot(&mut g, ep, now), Some(Ok(())))
                {
                    Slot::Alt
                } else {
                    let main = if now < g.next_any {
                        Err(g.next_any - now)
                    } else {
                        g.bucket(ep).try_acquire(now)
                    };
                    match main {
                        Ok(()) => {
                            g.next_any = now + g.spacing;
                            Slot::Main
                        }
                        Err(w) => match if ep == Endpoint::Legacy && auth {
                            None
                        } else {
                            self.alt_slot(&mut g, ep, now)
                        } {
                            Some(Ok(())) => Slot::Alt,
                            alt => {
                                // Balde em pausa por 429 e a extra sem vaga: falha já, em vez de segurar quem chamou
                                // por até 30 min (a caça segue pelo outro balde).
                                if let Some(end) = g.bucket(ep).pause_end(now)
                                    && (alt.is_none() || g.alt_held(now))
                                {
                                    return Err(Error::RateLimited {
                                        until: unix_in(end - now),
                                    });
                                }
                                Slot::Wait(match alt {
                                    Some(Err(a)) => w.min(a),
                                    _ => w,
                                })
                            }
                        },
                    }
                }
            };
            match slot {
                Slot::Main => break,
                Slot::Wait(w) => tokio::time::sleep(w).await,
                Slot::Alt => {
                    if let Some(r) = self.send_alt(ep, &req, check, reset_on_success).await {
                        return r;
                    }
                }
            }
        }
        // Sessão que mandou a consulta: cookies da resposta só valem para ela (o login pode mudar no meio).
        let mut sent_by: Option<String> = None;
        if auth {
            let g = self.inner.lock().unwrap();
            // Decidido depois da vaga: um 429 logado de outra consulta durante a espera já vale para esta.
            let anon = matches!(ep, Endpoint::Listings | Endpoint::History | Endpoint::Book)
                && g.anon_reads_until.is_some_and(|t| Instant::now() < t);
            if let Some(c) = g.cookie.as_ref().filter(|_| !anon) {
                sent_by = Some(c.steam_login_secure.clone());
                let mut h = c.header();
                if ep == Endpoint::Legacy {
                    h.push_str("; bMarketOptOut=1");
                }
                for (k, v) in &g.extra {
                    if ep == Endpoint::Legacy && k == "bMarketOptOut" {
                        continue;
                    }
                    h.push_str(&format!("; {k}={v}"));
                }
                req = req.header(COOKIE, h);
            } else if ep == Endpoint::Legacy {
                req = req.header(COOKIE, "bMarketOptOut=1");
            }
        }
        let request = req
            .build()
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let attempt = self.reserve_read(ep, "main").await?;
        if let Err(e) = self.pace_send(ep, false).await {
            crate::research::finish(attempt, "not_sent").await;
            return Err(e);
        }
        let resp = match self.http.execute(request).await {
            Ok(r) => r,
            Err(e) => {
                crate::research::finish(attempt, "network_error").await;
                self.inner.lock().unwrap().breakers[ep.idx()].on_fail(Instant::now());
                return Err(Error::Transport(e.without_url().to_string()));
            }
        };
        let result = self
            .finish(ep, resp, sent_by, check, reset_on_success, false)
            .await;
        crate::research::finish(attempt, research_outcome(&result)).await;
        result
    }

    /// Vaga na saída extra: só leitura de mercado, com a saída ligada. Descansando, devolve quanto falta.
    fn alt_slot(
        &self,
        g: &mut Inner,
        ep: Endpoint,
        now: Instant,
    ) -> Option<std::result::Result<(), Duration>> {
        if !self.alt_reads(ep) {
            return None;
        }
        match g.alt_down_until {
            Some(t) if now < t => Some(Err(t - now)),
            _ => Some(g.alt_limiter.try_reserve(now)),
        }
    }

    /// A saída pública aceita só livro e anúncios antigos; o modo genérico atende leituras nativas.
    fn alt_reads(&self, ep: Endpoint) -> bool {
        self.alt.is_some()
            && if self.alt_public_only {
                matches!(ep, Endpoint::Book | Endpoint::Legacy)
            } else {
                matches!(ep, Endpoint::Listings | Endpoint::History)
            }
    }

    /// Leitura pela saída extra, sempre sem login (o pedido é clonado antes do cookie). `None` = recusa, rede ou
    /// 429 nela: a consulta volta para a fila da principal sem tocar no breaker nem no limitador dela.
    async fn send_alt(
        &self,
        ep: Endpoint,
        req: &RequestBuilder,
        check: Option<EnvelopeCheck>,
        reset_on_success: bool,
    ) -> Option<Result<(Parts, Vec<u8>)>> {
        let alt = self.alt.as_ref()?;
        let mut request = req.try_clone()?.build().ok()?;
        if self.alt_public_only {
            request.headers_mut().remove(COOKIE);
            if ep == Endpoint::Legacy {
                request
                    .headers_mut()
                    .insert(COOKIE, HeaderValue::from_static("bMarketOptOut=1"));
            }
        }
        let attempt = match self.reserve_read(ep, "alt").await {
            Ok(attempt) => attempt,
            Err(e) => return Some(Err(e)),
        };
        if let Err(e) = self.pace_send(ep, true).await {
            crate::research::finish(attempt, "not_sent").await;
            return if matches!(e, Error::RateLimited { .. }) {
                None
            } else {
                Some(Err(e))
            };
        }
        match alt.execute(request).await {
            Ok(r) if r.status() == StatusCode::TOO_MANY_REQUESTS => {
                crate::research::finish(attempt, "rate_limited").await;
                let retry = retry_after(&r);
                let mut g = self.inner.lock().unwrap();
                g.calls[1].limited += 1;
                g.alt_limiter.on_429(Instant::now(), retry);
                drop(g);
                tracing::warn!(endpoint = ?ep, "Steam: 429 na saída extra; ela pausa e a principal segue");
                None
            }
            Ok(r) => {
                let result = self
                    .finish(ep, r, None, check, reset_on_success, true)
                    .await;
                crate::research::finish(attempt, research_outcome(&result)).await;
                Some(result)
            }
            Err(e) => {
                crate::research::finish(attempt, "network_error").await;
                let mut g = self.inner.lock().unwrap();
                if g.alt_down_until.is_none_or(|t| Instant::now() >= t) {
                    tracing::warn!(erro = %e.without_url(), "saída extra indisponível; tenta de novo em 5 min");
                }
                g.alt_down_until = Some(Instant::now() + ALT_DOWN);
                None
            }
        }
    }

    /// Classifica a resposta de qualquer saída. `sent_by` é a sessão do cookie enviado (só ela guarda os cookies
    /// da resposta); `via_alt` escolhe o limitador. O 429 da extra não chega aqui (`send_alt`).
    async fn finish(
        &self,
        ep: Endpoint,
        resp: reqwest::Response,
        sent_by: Option<String>,
        check: Option<EnvelopeCheck>,
        reset_on_success: bool,
        via_alt: bool,
    ) -> Result<(Parts, Vec<u8>)> {
        let now = Instant::now();
        let status = resp.status();
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if sent_by.is_some() {
            let set: Vec<String> = resp
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|v| v.to_str().ok().map(str::to_string))
                .collect();
            self.keep_cookies(&set, sent_by.as_deref());
        }
        let eresult = resp
            .headers()
            .get("x-eresult")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i32>().ok());
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry = retry_after(&resp);
            let mut g = self.inner.lock().unwrap();
            g.calls[if side(ep) { 2 } else { 0 }].limited += 1; // o 429 da extra não chega aqui
            let logged_read = sent_by.is_some()
                && matches!(ep, Endpoint::Listings | Endpoint::History | Endpoint::Book)
                && g.cookie.as_ref().map(|c| c.steam_login_secure.as_str()) == sent_by.as_deref();
            if logged_read {
                // Trocar o login não libera cota: a próxima leitura anônima também espera a pausa.
                g.anon_reads_until = Some(now + ANON_AFTER_429);
            }
            g.bucket(ep).on_429(now, retry);
            let until = g
                .bucket(ep)
                .stats(now)
                .paused_until
                .unwrap_or_else(|| unix_in(PAUSE_START));
            if !side(ep) || logged_read {
                Self::notify_pace(&mut g, now);
            }
            drop(g);
            // Qual consulta levou o 429 (e se ia logada): sem isso não dá para saber o que a Steam segura.
            tracing::warn!(endpoint = ?ep, logado = sent_by.is_some(),
                pausa_segundos = until - unix_in(Duration::ZERO),
                retry_after_segundos = retry.map(|d| d.as_secs()),
                proxima_sem_login = logged_read, "Steam: 429");
            return Err(Error::RateLimited { until });
        }
        let mut resp = resp;
        if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
            return Err(Error::Parse("resposta da Steam excede 16 MiB".into()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?
        {
            if chunk.len() > MAX_BODY - body.len() {
                return Err(Error::Parse("resposta da Steam excede 16 MiB".into()));
            }
            body.extend_from_slice(&chunk);
        }
        let bad_envelope = check.and_then(|f| f(status.as_u16(), &body));
        {
            let mut g = self.inner.lock().unwrap();
            let i = if via_alt {
                1
            } else if side(ep) {
                2
            } else {
                0
            };
            g.calls[i].ok += 1;
            if via_alt {
                g.alt_limiter.on_ok(now);
            } else {
                g.bucket(ep).on_ok(now);
            }
            if status.is_server_error() || status == StatusCode::FORBIDDEN || bad_envelope.is_some()
            {
                g.breakers[ep.idx()].on_fail(now);
            } else if reset_on_success {
                g.breakers[ep.idx()].on_ok();
            }
        }
        if status.is_server_error() || status == StatusCode::FORBIDDEN {
            return Err(Error::Http {
                endpoint: ep,
                status: status.as_u16(),
            });
        }
        if let Some(diag) = bad_envelope {
            if status == StatusCode::OK
                && serde_json::from_slice::<Value>(&body)
                    .ok()
                    .is_some_and(|v| {
                        v.as_object().is_some_and(|o| o.len() == 1)
                            && v.get("data") == Some(&Value::Null)
                    })
            {
                return Err(Error::ActionUnavailable { endpoint: ep });
            }
            return Err(Error::Parse(format!("resposta sem `data` ({diag})")));
        }
        Ok((
            Parts {
                status: status.as_u16(),
                location,
                eresult,
            },
            body,
        ))
    }

    /// Para o `buy.rs` (outra etapa): POST com cookie, pelo mesmo limitador e breaker.
    pub(crate) fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, self.url(path))
    }

    /// Como `request`, mas com URL completa (api.steampowered.com), pelo mesmo limitador e breaker.
    pub(crate) fn request_url(&self, method: Method, url: &str) -> RequestBuilder {
        self.http.request(method, url)
    }
}

/// `None` se o corpo é o envelope de `/market/actions` (JSON com `data` não nulo). Senão, um diagnóstico
/// sem conteúdo do usuário: status, tamanho, nomes das chaves do topo e `success` se for número ou booleano.
fn envelope_problem(status: u16, body: &[u8]) -> Option<String> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return Some(format!("status {status}, {} bytes, não é JSON", body.len()));
    };
    if v.get("data").is_some_and(|d| !d.is_null()) {
        return None;
    }
    let keys: Vec<&str> = v
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let success = v
        .get("success")
        .filter(|s| s.is_number() || s.is_boolean())
        .map(|s| format!(", success {s}"))
        .unwrap_or_default();
    Some(format!(
        "status {status}, {} bytes, chaves [{}]{success}",
        body.len(),
        keys.join(", ")
    ))
}

/// Confere o corpo de uma resposta 200; `Some(diagnóstico)` = fora do formato esperado.
type EnvelopeCheck = fn(u16, &[u8]) -> Option<String>;

pub(crate) struct Parts {
    pub status: u16,
    pub location: Option<String>,
    /// Header `x-eresult` (API da Steam).
    pub eresult: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::StatusCode as AxStatus, routing::get};

    #[tokio::test]
    async fn research_limits_retry_but_never_blocks_account_calls() {
        let app = Router::new()
            .route("/market/actions", get(|| async { r#"{"data":null}"# }))
            .route("/ok", get(|| async { "{}" }));
        let c = fast(&serve(app).await);
        let (db, _dir) = crate::db::temp_db();
        c.set_research_db(db.clone());
        for _ in 0..15 {
            crate::research::reserve(Some(db.clone()), "history", "main")
                .await
                .unwrap();
        }
        let decision = crate::research::Decision {
            source: "exploracao_externa".into(),
            policy: "test".into(),
            target: "x".into(),
            scale: 1.0,
            block_id: None,
        };
        let (result, context) = crate::research::scope_tracked(db.clone(), decision, async {
            let err = c
                .action(Endpoint::Listings, "Q", &serde_json::json!([]))
                .await
                .unwrap_err();
            assert!(matches!(err, Error::BudgetExhausted));
            c.get_json(Endpoint::Session, "/ok", &[]).await.unwrap();
            c.get_json(Endpoint::Buy, "/ok", &[]).await.unwrap();
            c.get_json(Endpoint::Auth, "/ok", &[]).await.unwrap();
            Ok(())
        })
        .await
        .unwrap();
        result.unwrap();
        assert_eq!(crate::research::attempts(db, context.id).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn research_checks_budget_again_after_alt_fallback() {
        let app = Router::new().route(
            "/market/actions",
            get(|h: axum::http::HeaderMap| async move {
                if h.contains_key("x-saida") {
                    (AxStatus::TOO_MANY_REQUESTS, "")
                } else {
                    (AxStatus::OK, r#"{"data":{"ok":true}}"#)
                }
            }),
        );
        let c = SteamClient::with_base(&serve(app).await)
            .unwrap()
            .with_alt(alt_marked());
        c.set_gap_for_test(Duration::from_millis(200));
        let (db, _dir) = crate::db::temp_db();
        c.set_research_db(db.clone());
        for _ in 0..30 {
            crate::research::reserve(Some(db.clone()), "history", "main")
                .await
                .unwrap();
        }
        let decision = crate::research::Decision {
            source: "exploracao_externa".into(),
            policy: "test".into(),
            target: "x".into(),
            scale: 1.0,
            block_id: None,
        };
        let (result, context) = crate::research::scope_tracked(db.clone(), decision, async {
            c.action(Endpoint::Listings, "Q", &serde_json::json!([]))
                .await
                .unwrap();
            let err = c
                .action(Endpoint::Listings, "Q", &serde_json::json!([]))
                .await
                .unwrap_err();
            assert!(matches!(err, Error::BudgetExhausted));
            Ok(())
        })
        .await
        .unwrap();
        result.unwrap();
        assert_eq!(crate::research::attempts(db, context.id).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn research_distinguishes_unbuilt_request_from_network_failure() {
        let c = fast("http://127.0.0.1:1");
        let (db, _dir) = crate::db::temp_db();
        c.set_research_db(db.clone());
        assert!(
            c.send(Endpoint::History, c.http.get("://invalid"), false)
                .await
                .is_err()
        );
        let count = || {
            let db = db.clone();
            async move {
                db.call(|c| {
                    c.query_row("SELECT count(*) FROM research_attempts", [], |r| {
                        r.get::<_, i64>(0)
                    })
                })
                .await
                .unwrap()
            }
        };
        assert_eq!(count().await, 0);
        assert!(c.get_json(Endpoint::History, "/x", &[]).await.is_err());
        let outcomes: (i64, String) = db
            .call(|c| {
                c.query_row("SELECT count(*), outcome FROM research_attempts", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
            })
            .await
            .unwrap();
        assert_eq!(outcomes, (1, "network_error".into()));
    }

    #[tokio::test]
    async fn slow_database_cannot_collapse_actual_send_spacing() {
        use std::sync::Arc;
        let times = Arc::new(Mutex::new(Vec::new()));
        let seen = times.clone();
        let app = Router::new().route(
            "/ok",
            get(move || {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(Instant::now());
                    "{}"
                }
            }),
        );
        let c = Arc::new(fast(&serve(app).await));
        let (db, _dir) = crate::db::temp_db();
        c.set_research_db(db.clone());
        let (started, ready) = tokio::sync::oneshot::channel();
        let db2 = db.clone();
        let hold = tokio::spawn(async move {
            db2.call(move |_| {
                let _ = started.send(());
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            })
            .await
            .unwrap()
        });
        ready.await.unwrap();
        let a = {
            let c = c.clone();
            tokio::spawn(async move { c.get_json(Endpoint::History, "/ok", &[]).await.unwrap() })
        };
        let b = {
            let c = c.clone();
            tokio::spawn(async move { c.get_json(Endpoint::History, "/ok", &[]).await.unwrap() })
        };
        a.await.unwrap();
        b.await.unwrap();
        hold.await.unwrap();
        let times = times.lock().unwrap();
        assert_eq!(times.len(), 2);
        assert!(times[1].duration_since(times[0]) >= Duration::from_millis(80));
    }

    #[tokio::test]
    async fn waiting_history_does_not_hold_up_purchase() {
        use std::sync::Arc;
        let app = Router::new().route("/ok", get(|| async { "{}" }));
        let c = Arc::new(fast(&serve(app).await));
        c.inner.lock().unwrap().limiter.gap = Duration::from_secs(1);
        c.main_send_pace.lock().await.last_bucket[0] = Some(Instant::now());
        let history = {
            let c = c.clone();
            tokio::spawn(async move { c.get_json(Endpoint::History, "/ok", &[]).await.unwrap() })
        };
        tokio::time::sleep(Duration::from_millis(30)).await;
        let started = Instant::now();
        c.get_json(Endpoint::Buy, "/ok", &[]).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(700));
        history.await.unwrap();
    }

    #[tokio::test]
    async fn empty_research_budget_does_not_consume_limiter_slot() {
        let c = fast("http://127.0.0.1:1");
        let (db, _dir) = crate::db::temp_db();
        c.set_research_db(db.clone());
        let before = c.inner.lock().unwrap().next_any;
        let d = crate::research::Decision {
            source: "exploracao_externa".into(),
            policy: "p".into(),
            target: "x".into(),
            scale: 1.0,
            block_id: None,
        };
        crate::research::scope(db, d, async {
            assert!(matches!(
                c.get_json(Endpoint::History, "/x", &[]).await,
                Err(Error::BudgetExhausted)
            ));
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(c.inner.lock().unwrap().next_any, before);
        assert_eq!(c.sent(), 0);
    }

    #[tokio::test]
    async fn pause_after_reservation_prevents_send_and_refunds_budget() {
        use std::sync::Arc;
        let app = Router::new().route("/ok", get(|| async { "{}" }));
        let c = Arc::new(fast(&serve(app).await));
        c.set_gap_for_test(Duration::from_millis(400));
        c.main_send_pace.lock().await.last_any = Some(Instant::now());
        let (db, _dir) = crate::db::temp_db();
        c.set_research_db(db.clone());
        for _ in 0..15 {
            crate::research::reserve(Some(db.clone()), "history", "main")
                .await
                .unwrap();
        }
        let d = crate::research::Decision {
            source: "exploracao_externa".into(),
            policy: "p".into(),
            target: "x".into(),
            scale: 1.0,
            block_id: None,
        };
        let c2 = c.clone();
        let task = tokio::spawn({
            let db = db.clone();
            async move {
                crate::research::scope_tracked(db, d, async move {
                    assert!(matches!(
                        c2.get_json(Endpoint::History, "/ok", &[]).await,
                        Err(Error::RateLimited { .. })
                    ));
                    Ok(())
                })
                .await
                .unwrap()
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.inner.lock().unwrap().limiter.on_429(Instant::now(), None);
        let (result, context) = task.await.unwrap();
        result.unwrap();
        assert_eq!(c.sent(), 0);
        assert_eq!(
            crate::research::attempts(db.clone(), context.id)
                .await
                .unwrap(),
            0
        );
        let summary = db
            .call(|c| crate::research::summary(c, crate::db::now_unix()))
            .await
            .unwrap();
        assert_eq!((summary.reads_1h, summary.available), (15, 1));
    }

    #[tokio::test]
    async fn null_data_retries_once() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for ep in [Endpoint::Listings, Endpoint::History] {
            let calls = Arc::new(AtomicUsize::new(0));
            let seen = calls.clone();
            let app = Router::new().route(
                "/market/actions",
                get(move || {
                    let seen = seen.clone();
                    async move {
                        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                            include_str!("../../testdata/steam_actions_null_contract.json")
                        } else {
                            r#"{"data":{"ok":true}}"#
                        }
                    }
                }),
            );
            let c = fast(&serve(app).await);
            let started = Instant::now();
            let data = c
                .action(ep, "Query", &serde_json::json!([730, "item"]))
                .await
                .unwrap();
            assert_eq!(data["ok"], true);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(started.elapsed() >= Duration::from_secs(5));
            assert!(c.breaker_open_until(ep).is_none());
        }
    }

    #[tokio::test]
    async fn retry_wait_does_not_delay_other_endpoints_and_alternating_failures_open_breaker() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let app = Router::new()
            .route(
                "/market/actions",
                get(move || {
                    let n = seen.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n.is_multiple_of(2) {
                            include_str!("../../testdata/steam_actions_null_contract.json")
                        } else {
                            r#"{"data":{"ok":true}}"#
                        }
                    }
                }),
            )
            .route("/other", get(|| async { r#"{"ok":true}"# }));
        let c = Arc::new(fast(&serve(app).await));
        let first = c.clone();
        let task = tokio::spawn(async move {
            first
                .action(Endpoint::Listings, "Query", &serde_json::json!([]))
                .await
        });
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let other = tokio::time::timeout(
            Duration::from_secs(2),
            c.get_json(Endpoint::History, "/other", &[]),
        )
        .await;
        assert!(other.unwrap().is_ok());
        assert!(task.await.unwrap().is_ok());
        assert!(
            c.action(Endpoint::Listings, "Query", &serde_json::json!([]))
                .await
                .is_ok()
        );
        assert!(matches!(
            c.action(Endpoint::Listings, "Query", &serde_json::json!([]))
                .await,
            Err(Error::BreakerOpen { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn persistent_null_is_not_empty_and_still_opens_breaker() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let app = Router::new().route(
            "/market/actions",
            get(move || {
                seen.fetch_add(1, Ordering::SeqCst);
                async { include_str!("../../testdata/steam_actions_null_contract.json") }
            }),
        );
        let c = fast(&serve(app).await);
        let qp = serde_json::json!([]);
        assert!(matches!(
            c.action(Endpoint::Listings, "Query", &qp).await,
            Err(Error::ActionUnavailable { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(matches!(
            c.action(Endpoint::Listings, "Query", &qp).await,
            Err(Error::BreakerOpen { .. })
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(c.breaker_open_until(Endpoint::History).is_none());
    }

    #[tokio::test]
    async fn unknown_envelopes_do_not_retry() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for body in ["{}", "not json", r#"{"data":null,"success":false}"#] {
            let calls = Arc::new(AtomicUsize::new(0));
            let seen = calls.clone();
            let app = Router::new().route(
                "/market/actions",
                get(move || {
                    seen.fetch_add(1, Ordering::SeqCst);
                    async move { body }
                }),
            );
            let c = fast(&serve(app).await);
            assert!(matches!(
                c.action(Endpoint::Listings, "Query", &serde_json::json!([]))
                    .await,
                Err(Error::Parse(_))
            ));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

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
            Err(Duration::from_secs(5))
        );
        assert!(l.try_acquire(t + Duration::from_secs(6)).is_ok());
    }

    #[test]
    fn limiter_shortens_15pct_per_10min_with_floor() {
        let t = t0();
        let mut l = Limiter::new(t);
        // sem 429 não passa da base de 6 s
        assert_eq!(l.stats(t + Duration::from_secs(30 * 60)).gap_ms, 6000);
        l.on_429(t + Duration::from_secs(30 * 60), None);
        let after = t + Duration::from_secs(40 * 60);
        assert_eq!(l.stats(after).gap_ms, 7650); // 9000 × 0,85
        assert_eq!(l.stats(t + Duration::from_secs(48 * 3600)).gap_ms, 6000);
    }

    #[test]
    fn limiter_learns_floor_from_first_429_and_relaxes_every_6h() {
        let t = t0();
        let mut l = Limiter::new(t);
        l.on_429(t, None); // a 6 s: piso 7,5 s
        // 429s seguintes da série (intervalo já alongado) não sobem o piso
        l.on_429(t + Duration::from_secs(61), None);
        let h = |x: u64| t + Duration::from_secs(x * 3600);
        assert_eq!(l.stats(h(5)).gap_ms, 7500);
        assert_eq!(l.stats(h(7)).gap_ms, 6000); // 6 h quietas: 7,5 ÷ 1,25
        // a 6 s de novo, outro 429 depois de 60 min: piso 7,5 s outra vez, sobrevive ao reinício
        l.on_429(h(7), None);
        assert_eq!(l.stats(h(10)).gap_ms, 7500);
        // um 429 da mesma série 30 min depois não sobe o piso, mas recomeça as 6 h
        let half = h(7) + Duration::from_secs(1800);
        l.on_429(half, None);
        assert_eq!(l.stats(h(13)).gap_ms, 7500);
        assert_eq!(l.stats(h(14)).gap_ms, 6000);
        l.on_429(h(15), None);
        let c = SteamClient::new().unwrap();
        c.inner.lock().unwrap().limiter = l;
        let saved = SteamClient::saved_pace(&mut c.inner.lock().unwrap(), Instant::now());
        let again = SteamClient::new().unwrap();
        again.restore_pace(&saved);
        let floor = again.inner.lock().unwrap().limiter.floor(Instant::now());
        assert_eq!(floor, Duration::from_millis(7500));
        // reinício no meio de uma pausa: o último 429 é a hora do 7º campo, não o fim da pausa
        let now_u = unix_in(Duration::ZERO);
        let mid = SteamClient::new().unwrap();
        mid.restore_pace(&format!(
            "{}|9000|120000|0|{}|7500|{}",
            now_u + 600,
            now_u - 1800,
            now_u - 1800
        ));
        let ago =
            Instant::now().duration_since(mid.inner.lock().unwrap().limiter.last_429.unwrap());
        assert!((1795..=1805).contains(&ago.as_secs()), "{ago:?}");
    }

    #[tokio::test]
    async fn pace_saved_on_429_survives_restart() {
        let app = Router::new().route("/busy", get(|| async { (AxStatus::TOO_MANY_REQUESTS, "") }));
        let c = fast(&serve(app).await);
        let saved = Arc::new(Mutex::new(None));
        let hook = saved.clone();
        c.on_pace_change(move |v| *hook.lock().unwrap() = Some(v));
        let r = c.get_json(Endpoint::History, "/busy", &[]).await;
        assert!(matches!(r, Err(Error::RateLimited { .. })));
        let v = saved.lock().unwrap().clone().expect("429 chama o gancho");
        let before = c.stats();

        let again = SteamClient::new().unwrap();
        again.restore_pace(&v);
        let after = again.stats();
        assert_eq!(after.paused_until, before.paused_until);
        assert_eq!(after.gap_ms, before.gap_ms.max(6000)); // `fast` encurta abaixo do piso
        let pause = |c: &SteamClient| c.inner.lock().unwrap().limiter.pause_len;
        assert_eq!(pause(&again), pause(&c)); // a próxima pausa segue dobrada

        // pausa vencida não volta; lixo é ignorado
        let fresh = SteamClient::new().unwrap();
        fresh.restore_pace(&format!("{}|1500|60000", unix_in(Duration::ZERO) - 5));
        fresh.restore_pace("lixo");
        let s = fresh.stats();
        assert_eq!((s.gap_ms, s.paused_until), (6000, None));
        // 429 de mais de 60 min atrás: a escalada não volta
        let old = SteamClient::new().unwrap();
        old.restore_pace(&format!("{}|6000|600000", unix_in(Duration::ZERO) - 3700));
        assert_eq!(pause(&old), PAUSE_START);
        // o tempo quieto depois do 429 salvo conta: 60 min = 6 encurtamentos, 3 h volta à base
        old.restore_pace(&format!("{}|60000|60000", unix_in(Duration::ZERO) - 3600));
        assert_eq!(old.stats().gap_ms, 22628); // 60000 × 0,85⁶
        old.restore_pace(&format!(
            "{}|60000|60000",
            unix_in(Duration::ZERO) - 3 * 3600
        ));
        assert_eq!(old.stats().gap_ms, 6000);
        // intervalo salvo já encurtado (sem 429 novo): conta do 5º campo, não do 429 antigo
        let now_u = unix_in(Duration::ZERO);
        old.restore_pace(&format!("{}|22628|60000|0|{}", now_u - 3600, now_u - 60));
        assert_eq!(old.stats().gap_ms, 22628);
        // e o texto salvo volta igual: salvar e retomar não encurta de novo
        let again = SteamClient::new().unwrap();
        again.restore_pace(&SteamClient::saved_pace(
            &mut old.inner.lock().unwrap(),
            Instant::now(),
        ));
        assert_eq!(again.stats().gap_ms, 22628);
        // e um 429 depois de 60 min quietos recomeça em 60 s, sem precisar de resposta boa no meio
        let mut l = Limiter::new(t0());
        l.on_429(t0(), None);
        l.on_429(t0() + Duration::from_secs(120), None);
        let later = t0() + Duration::from_secs(120) + PAUSE_RESET_AFTER;
        l.on_429(later, None);
        assert_eq!(l.paused_until.unwrap().duration_since(later).as_secs(), 60);
    }

    #[tokio::test]
    async fn last_answer_and_call_counts_for_the_panel_and_the_log() {
        let app = Router::new()
            .route("/ok", get(|| async { "{}" }))
            .route("/busy", get(|| async { (AxStatus::TOO_MANY_REQUESTS, "") }));
        let c = fast(&serve(app).await);
        let saved = Arc::new(Mutex::new(None));
        let hook = saved.clone();
        c.on_pace_change(move |v| *hook.lock().unwrap() = Some(v));
        assert_eq!(c.stats().last_ok_at, None);
        c.get_json(Endpoint::History, "/ok", &[]).await.unwrap();
        let ok_at = c.stats().last_ok_at.expect("resposta boa marca a hora");
        assert!((ok_at - unix_in(Duration::ZERO)).abs() <= 1);
        let last_ok = |c: &SteamClient| c.inner.lock().unwrap().limiter.last_ok;
        let ok_instant = last_ok(&c);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let r = c.get_json(Endpoint::Listings, "/busy", &[]).await;
        assert!(matches!(r, Err(Error::RateLimited { .. })));
        assert_eq!(last_ok(&c), ok_instant); // 429 não conta como resposta
        assert_eq!(
            c.take_calls(),
            [
                Calls { ok: 1, limited: 1 },
                Calls::default(),
                Calls::default()
            ]
        );
        assert_eq!(c.take_calls(), [Calls::default(); 3]); // ler zera
        // reiniciar no meio do bloqueio ainda sabe desde quando a Steam não responde
        let again = SteamClient::new().unwrap();
        again.restore_pace(&saved.lock().unwrap().clone().unwrap());
        let restored = again.stats().last_ok_at.unwrap();
        assert!((restored - ok_at).abs() <= 1); // segundos inteiros nos dois lados
    }

    #[tokio::test]
    async fn listings_429_leaves_book_and_search_free_and_fails_fast() {
        let book: Value = serde_json::from_str(include_str!(
            "../../testdata/steam_orderbook_negev_2026-11.json"
        ))
        .unwrap();
        let app = Router::new()
            .route(
                "/market/actions",
                get(|| async { (AxStatus::TOO_MANY_REQUESTS, "") }),
            )
            .route(
                "/market/orderbook",
                get(move || {
                    let b = book.clone();
                    async move { axum::Json(b) }
                }),
            )
            .route(
                "/market/search/render/",
                get(|| async { r#"{"success":true}"# }),
            );
        let c = fast(&serve(app).await);
        let qp = serde_json::json!([{}]);
        let r = c
            .action(Endpoint::Listings, "QueryListingsForItem", &qp)
            .await;
        assert!(matches!(r, Err(Error::RateLimited { .. })));
        assert!(c.blocked(Endpoint::Listings));
        assert!(!c.blocked(Endpoint::Book) && !c.blocked(Endpoint::Search));
        // livro e busca são outro balde da Steam: seguem sem esperar a pausa de 60 s dos anúncios
        let quick = Duration::from_secs(5);
        let b = tokio::time::timeout(quick, crate::steam::book::fetch_book(&c, "x"))
            .await
            .expect("o livro não espera a pausa dos anúncios")
            .unwrap();
        assert_eq!(b.min_sell_cents, Some(167));
        tokio::time::timeout(
            quick,
            c.get_json(Endpoint::Search, "/market/search/render/", &[]),
        )
        .await
        .unwrap()
        .unwrap();
        // e a leitura de anúncios em pausa falha na hora, sem ir à Steam nem segurar quem chamou
        let again = tokio::time::timeout(
            quick,
            c.action(Endpoint::Listings, "QueryListingsForItem", &qp),
        )
        .await
        .expect("falha na hora");
        assert!(matches!(again, Err(Error::RateLimited { .. })));
        assert_eq!(
            c.take_calls(),
            [
                Calls { ok: 0, limited: 1 },
                Calls::default(),
                Calls { ok: 2, limited: 0 }
            ]
        );
        let st = c.stats();
        assert!(st.book_ok_at.is_some());
        assert!(
            st.paused_until.is_some(),
            "o painel ainda mostra a pausa dos anúncios"
        );
        assert_eq!(
            st.last_ok_at, None,
            "resposta do livro não diz que os anúncios voltaram"
        );
    }

    #[tokio::test]
    async fn buckets_share_the_minimum_spacing() {
        let app = Router::new().route("/ok", get(|| async { "{}" }));
        let c = fast(&serve(app).await);
        c.inner.lock().unwrap().spacing = Duration::from_millis(400);
        let t = Instant::now();
        c.get_json(Endpoint::History, "/ok", &[]).await.unwrap();
        c.get_json(Endpoint::Search, "/ok", &[]).await.unwrap();
        assert!(
            t.elapsed() >= Duration::from_millis(400),
            "baldes diferentes ainda respeitam o espaçamento mínimo"
        );
    }

    #[test]
    fn limiter_429_pauses_doubles_and_lengthens() {
        let t = t0();
        let mut l = Limiter::new(t);
        l.on_429(t, None);
        let s = l.stats(t);
        assert_eq!(s.gap_ms, 9000);
        assert!(s.paused_until.is_some());
        assert_eq!(
            l.try_acquire(t + Duration::from_secs(30)),
            Err(Duration::from_secs(30))
        );
        // outro 429 dentro da pausa não avança o degrau
        l.on_429(t + Duration::from_secs(10), None);
        assert_eq!(l.stats(t).gap_ms, 9000);
        // fim da pausa; novo 429 dobra: 120 s
        let t2 = t + Duration::from_secs(61);
        assert!(l.try_acquire(t2).is_ok());
        l.on_429(t2, None);
        assert_eq!(
            l.try_acquire(t2 + Duration::from_secs(119)),
            Err(Duration::from_secs(1))
        );
        // degraus: 60, 120, 240, 480, 960, 1800, 1800
        let mut now = t2;
        let mut lens = vec![];
        for _ in 0..5 {
            now += Duration::from_secs(1900);
            l.on_429(now, None);
            lens.push(l.paused_until.unwrap().duration_since(now).as_secs());
        }
        assert_eq!(lens, vec![240, 480, 960, 1800, 1800]);
        // resposta boa logo depois (de outro endpoint) não zera a escalada
        l.on_ok(now + Duration::from_secs(1000));
        l.on_429(now + Duration::from_secs(1900), None);
        assert_eq!(
            l.paused_until
                .unwrap()
                .duration_since(now + Duration::from_secs(1900))
                .as_secs(),
            1800
        );
        now += Duration::from_secs(1900);
        // 60 min sem 429: o degrau volta a 60 s
        l.on_ok(now + PAUSE_RESET_AFTER);
        now += PAUSE_RESET_AFTER + Duration::from_secs(1);
        l.on_429(now, None);
        assert_eq!(l.paused_until.unwrap().duration_since(now).as_secs(), 60);
    }

    #[test]
    fn another_429_extends_retry_after_without_escalating_twice() {
        let t = t0();
        let mut l = Limiter::new(t);
        l.on_429(t, None);
        let gap = l.gap;
        let next_pause = l.pause_len;
        l.on_429(t + Duration::from_secs(10), Some(Duration::from_secs(3600)));
        let end = t + Duration::from_secs(3610);
        assert_eq!(l.paused_until, Some(end));
        assert_eq!((l.gap, l.pause_len), (gap, next_pause));
        l.on_429(t + Duration::from_secs(20), Some(Duration::from_secs(1)));
        l.on_429(t + Duration::from_secs(30), None);
        assert_eq!(
            l.paused_until,
            Some(end),
            "prazo menor nunca encurta a pausa"
        );
        assert!(l.try_acquire(end - Duration::from_secs(1)).is_err());
        assert!(l.try_acquire(end).is_ok());
    }

    #[test]
    fn limiter_counts_queries_per_minute() {
        let t = t0();
        let mut l = Limiter::new(t);
        for i in 0..5 {
            assert!(l.try_acquire(t + Duration::from_secs(6 * i)).is_ok());
        }
        assert_eq!(l.stats(t + Duration::from_secs(24)).queries_per_min, 5);
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

    #[test]
    fn response_cookies_only_for_the_session_that_sent() {
        let c = SteamClient::new().unwrap();
        let login = |s: &str| {
            c.set_cookie(Some(Cookie {
                steam_login_secure: s.into(),
                sessionid: None,
            }))
        };
        let extra = |c: &SteamClient| c.inner.lock().unwrap().extra.clone();
        login("a");
        c.keep_cookies(&["webTradeEligibility=1; Path=/".into()], Some("a"));
        assert_eq!(extra(&c), [("webTradeEligibility".into(), "1".into())]);
        login("b"); // login trocou com a consulta de A em voo
        c.keep_cookies(&["browserid=9".into()], Some("a"));
        c.keep_cookies(&["browserid=8".into()], None);
        assert!(extra(&c).iter().all(|(k, _)| k != "browserid"));
    }

    // gap de 100 ms para os testes de rede não dormirem 3 s por consulta
    pub(super) fn fast(base: &str) -> SteamClient {
        let c = SteamClient::with_base(base).unwrap();
        c.set_gap_for_test(Duration::from_millis(100));
        c
    }

    pub(super) async fn serve(app: Router) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    // Saída extra de teste: sem proxy, marcada por cabeçalho para o servidor saber por onde a consulta veio.
    fn alt_marked() -> reqwest::Client {
        let mut h = HeaderMap::new();
        h.insert("x-saida", HeaderValue::from_static("extra"));
        reqwest::Client::builder()
            .default_headers(h)
            .build()
            .unwrap()
    }

    #[test]
    fn alt_exit_never_takes_account_actions() {
        let c = SteamClient::with_base("http://x")
            .unwrap()
            .with_alt(alt_marked());
        let mut g = c.inner.lock().unwrap();
        // livro e busca têm o balde deles na principal: a extra só alivia anúncios e histórico
        for ep in [
            Endpoint::Buy,
            Endpoint::Session,
            Endpoint::Auth,
            Endpoint::Book,
            Endpoint::Search,
        ] {
            assert!(c.alt_slot(&mut g, ep, Instant::now()).is_none(), "{ep:?}");
        }
        assert!(
            c.alt_slot(&mut g, Endpoint::Listings, Instant::now())
                .is_some()
        );
    }

    #[tokio::test]
    async fn alt_exit_takes_reads_the_main_cannot_without_login() {
        let app = Router::new().route(
            "/market/actions",
            get(|h: axum::http::HeaderMap| async move {
                let extra = h.contains_key("x-saida");
                assert!(
                    !(extra && h.contains_key("cookie")),
                    "a saída extra nunca vai logada"
                );
                (AxStatus::OK, format!(r#"{{"data":{{"extra":{extra}}}}}"#))
            }),
        );
        let base = serve(app).await;
        let c = SteamClient::with_base(&base)
            .unwrap()
            .with_alt(alt_marked());
        c.set_gap_for_test(Duration::from_secs(3600));
        c.set_cookie(Some(Cookie {
            steam_login_secure: "x".into(),
            sessionid: None,
        }));
        let qp = serde_json::json!([1]);
        let read = || c.action(Endpoint::Listings, "Q", &qp);
        assert_eq!(
            read().await.unwrap()["extra"],
            false,
            "a principal tem a preferência"
        );
        assert_eq!(
            read().await.unwrap()["extra"],
            true,
            "principal só em 1 h: a extra lê"
        );
        assert_eq!(c.stats().queries_per_min, 2);
    }

    #[tokio::test]
    async fn alt_exit_429_or_down_falls_back_without_touching_the_main() {
        let app = Router::new().route(
            "/market/actions",
            get(|h: axum::http::HeaderMap| async move {
                if h.contains_key("x-saida") {
                    return (AxStatus::TOO_MANY_REQUESTS, String::new());
                }
                (AxStatus::OK, r#"{"data":{"ok":true}}"#.to_string())
            }),
        );
        let base = serve(app).await;
        let qp = serde_json::json!([1]);
        // 429 na extra: pausa só ela, e a consulta sai pela principal quando ela volta.
        let c = SteamClient::with_base(&base)
            .unwrap()
            .with_alt(alt_marked());
        c.set_gap_for_test(Duration::from_millis(200));
        for _ in 0..2 {
            c.action(Endpoint::Listings, "Q", &qp).await.unwrap();
        }
        {
            let g = c.inner.lock().unwrap();
            assert!(
                g.limiter.paused_until.is_none(),
                "a principal não pausa pelo 429 da extra"
            );
            assert!(g.alt_limiter.paused_until.is_some());
        }
        assert_eq!(c.stats().paused_until, None, "a caça segue pela principal");
        // Extra desligada (nada escutando): descansa, a principal segue e nenhum breaker abre.
        let off = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap();
        let c = SteamClient::with_base(&base).unwrap().with_alt(off);
        c.set_gap_for_test(Duration::from_millis(200));
        for _ in 0..3 {
            c.action(Endpoint::Listings, "Q", &qp).await.unwrap();
        }
        assert!(c.inner.lock().unwrap().alt_down_until.is_some());
        assert!(c.breaker_open_until(Endpoint::Listings).is_none());
    }

    #[tokio::test]
    async fn logged_429_honors_retry_after_without_sending_an_anonymous_retry() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let app = Router::new().route(
            "/market/actions",
            get(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async { (AxStatus::TOO_MANY_REQUESTS, [("retry-after", "3600")], "") }
            }),
        );
        let base = serve(app).await;
        for ep in [Endpoint::History, Endpoint::Book] {
            let c = fast(&base);
            c.set_cookie(Some(Cookie {
                steam_login_secure: "test-login".into(),
                sessionid: None,
            }));
            let before = calls.load(std::sync::atomic::Ordering::Relaxed);
            let first = c
                .action(ep, "Limit", &serde_json::json!([]))
                .await
                .unwrap_err();
            assert!(
                matches!(first, Error::RateLimited { until } if until >= unix_in(Duration::from_secs(3595))),
                "o 429 logado deve respeitar Retry-After: {first}"
            );
            assert!(c.blocked(ep));
            assert!(c.inner.lock().unwrap().anon_reads_until.is_some());
            assert!(c.action(ep, "Limit", &serde_json::json!([])).await.is_err());
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::Relaxed),
                before + 1,
                "trocar para anônimo não libera outra chamada durante a pausa"
            );
            let mut g = c.inner.lock().unwrap();
            let saved = SteamClient::saved_pace(&mut g, Instant::now());
            if !side(ep) {
                assert!(
                    saved.split('|').next().unwrap().parse::<i64>().unwrap()
                        >= unix_in(Duration::from_secs(3595)),
                    "a pausa logada deve sobreviver ao reinício: {saved}"
                );
            }
        }
    }

    #[tokio::test]
    async fn action_sends_header_and_unwraps_data_then_429_pauses() {
        let app = Router::new()
            .route(
                "/market/actions",
                get(|h: axum::http::HeaderMap, q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    assert_eq!(h.get("x-valve-request-type").unwrap(), "queryAction");
                    let logged = h.get("cookie").is_some_and(|v| v.to_str().unwrap().contains("steamLoginSecure=x"));
                    match q.get("q").map(String::as_str) {
                        Some("Limit") => return (AxStatus::TOO_MANY_REQUESTS, String::new()),
                        Some("Ok") => assert!(logged, "leitura vai logada"),
                        _ => assert!(!logged, "depois do 429 logado, a leitura vai sem login"),
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
        let saved = Arc::new(Mutex::new(String::new()));
        let keep = saved.clone();
        c.on_pace_change(move |v| *keep.lock().unwrap() = v);
        // 429 logado: pausa e alonga antes de a próxima seguir sem login.
        let e = c
            .action(Endpoint::History, "Limit", &serde_json::json!([1]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::RateLimited { .. }));
        let s = c.stats();
        assert!(s.paused_until.is_some());
        assert_eq!(s.gap_ms, 150);
        // O prazo sem login sobrevive a um reinício.
        let v = saved.lock().unwrap().clone();
        let anon: i64 = v.split('|').nth(3).unwrap().parse().unwrap();
        assert!(anon > unix_in(Duration::from_secs(5 * 3600)), "{v}");
        let again = fast(&base);
        again.set_cookie(c.cookie());
        again.restore_pace(&v);
        assert!(
            again.blocked(Endpoint::Listings),
            "reiniciar não libera a pausa logada"
        );
        // Simula o fim da pausa sem dormir um minuto; o prazo sem login continua valendo.
        again.inner.lock().unwrap().limiter.paused_until = Some(Instant::now());
        c.inner.lock().unwrap().limiter.paused_until = Some(Instant::now());
        again
            .action(Endpoint::Listings, "Anon", &serde_json::json!([1]))
            .await
            .unwrap();
        c.action(Endpoint::Listings, "Anon", &serde_json::json!([1]))
            .await
            .unwrap();
        // 429 sem login também pausa; o status não informa qual cota foi atingida.
        let e = c
            .action(Endpoint::Listings, "Limit", &serde_json::json!([1]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::RateLimited { .. }));
        let s = c.stats();
        assert!(s.paused_until.is_some());
        assert_eq!(s.gap_ms, 225);
    }

    #[test]
    fn other_account_drops_anon_reads_and_saved_pace_keeps_last_429() {
        let c = SteamClient::with_base("http://127.0.0.1:9").unwrap();
        let login = |s: &str| {
            Some(Cookie {
                steam_login_secure: s.into(),
                sessionid: None,
            })
        };
        c.set_cookie(login("x"));
        let anon = |c: &SteamClient| c.inner.lock().unwrap().anon_reads_until.is_some();
        c.restore_pace(&format!(
            "0|6000|60000|{}",
            unix_in(Duration::from_secs(3600))
        ));
        assert!(anon(&c));
        c.set_cookie(login("x"));
        assert!(anon(&c), "mesma conta mantém o prazo");
        let saved = Arc::new(Mutex::new(String::new()));
        let keep = saved.clone();
        c.on_pace_change(move |v| *keep.lock().unwrap() = v);
        c.set_cookie(login("y"));
        assert!(
            saved.lock().unwrap().split('|').nth(3) == Some("0"),
            "o prazo zerado vai para o banco"
        );
        assert!(!anon(&c), "outra conta lê logada de novo");
        // Pausa vencida: o 1º campo salvo é a hora do último 429, não zero.
        let mut g = c.inner.lock().unwrap();
        let now = Instant::now();
        g.limiter.last_429 = Some(now - Duration::from_secs(600));
        let v = SteamClient::saved_pace(&mut g, now);
        let first: i64 = v.split('|').next().unwrap().parse().unwrap();
        assert!((first - unix_in(Duration::ZERO) + 600).abs() <= 1, "{v}");
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

    #[tokio::test]
    async fn envelope_without_data_counts_as_failure_with_diagnostic() {
        let app = Router::new().route(
            "/market/actions",
            get(|| async { r#"{"success":false,"lista":[1,2]}"# }),
        );
        let c = fast(&serve(app).await);
        let qp = serde_json::json!({});
        for _ in 0..3 {
            let e = c
                .action(Endpoint::Listings, "QueryListingsForItem", &qp)
                .await
                .unwrap_err();
            let m = e.to_string();
            assert!(m.contains("sem `data`"), "{m}");
            assert!(
                m.contains("status 200") && m.contains("chaves [lista, success]"),
                "{m}"
            );
            assert!(m.contains("success false"), "{m}");
            assert!(!m.contains("[1,2]"), "{m}"); // só nomes de chave, nunca o conteúdo
        }
        // três respostas assim seguidas abrem o breaker do endpoint, como um 5xx
        let e = c
            .action(Endpoint::Listings, "QueryListingsForItem", &qp)
            .await
            .unwrap_err();
        assert!(matches!(e, Error::BreakerOpen { .. }), "{e}");
    }
}

#[cfg(test)]
#[path = "client_legacy_tests.rs"]
mod legacy_tests;
