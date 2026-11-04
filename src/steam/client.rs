//! HTTP único da Steam: cookie trocável, limitador global adaptativo e breaker por endpoint.
//!
//! Ritmo (docs/projeto.md, "Caça → Ritmo"): começa em 1 consulta a cada 6 s (também o piso); cada 10 min sem
//! 429 encurta 15 %; um 429 pausa 60 s (dobrando até 30 min) e alonga o intervalo em 50 %.
//! O limitador é um só para todos os endpoints; os breakers são um por endpoint (conhecimento 2): falhas
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
const PAUSE_START: Duration = Duration::from_secs(60);
// 30 min: em 04/11 a Steam seguiu dando 429 por horas com uma tentativa a cada 10 min; uma consulta depois de
// minutos quietos passou. Tentar menos deixa a penalidade esfriar.
const PAUSE_MAX: Duration = Duration::from_secs(30 * 60);
// O degrau da pausa só volta a 60 s depois de tanto tempo sem 429: uma resposta boa de outro
// endpoint entre dois 429 não pode zerar a escalada.
const PAUSE_RESET_AFTER: Duration = Duration::from_secs(60 * 60); // maior que PAUSE_MAX, senão a escalada zera sozinha
// Leitura logada levou 429: anúncios e histórico vão sem login por este tempo. Em 04/11 o logado tomou 429
// por horas enquanto a mesma consulta sem login, do mesmo IP, voltava 200 (a Steam segura a conta).
const ANON_AFTER_429: Duration = Duration::from_secs(6 * 60 * 60);

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
    /// api.steampowered.com (login por QR e renovação): mesmo limitador global, breaker próprio.
    Auth,
}

impl Endpoint {
    const ALL: [Endpoint; 6] = [
        Self::Listings,
        Self::History,
        Self::Search,
        Self::Session,
        Self::Buy,
        Self::Auth,
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
        }
    }

    fn shorten(&mut self, now: Instant) {
        while now.duration_since(self.last_adjust) >= SHORTEN_EVERY {
            self.gap = self.gap.mul_f64(SHORTEN_FACTOR).max(MIN_GAP);
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
        // 60 min sem 429 (mesmo sem nenhuma resposta no meio): a escalada recomeça.
        if self
            .last_429
            .is_some_and(|t| now.duration_since(t) >= PAUSE_RESET_AFTER)
        {
            self.pause_len = PAUSE_START;
        }
        let pause = self.pause_len.max(retry_after.unwrap_or_default());
        self.paused_until = Some(now + pause);
        self.pause_len = (self.pause_len * 2).min(PAUSE_MAX);
        self.last_429 = Some(now);
        self.gap = self.gap.mul_f64(1.5).min(MAX_GAP);
        self.last_adjust = now;
    }

    /// Resposta que não foi 429: o degrau da pausa volta ao início se o último 429 já tem 60 min.
    pub(crate) fn on_ok(&mut self, now: Instant) {
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
    breakers: [Breaker; 6],
    cookie: Option<Cookie>,
    /// Cookies que a Steam setou nas respostas autenticadas (`webTradeEligibility` etc.); valem enquanto
    /// o login for o mesmo e vão em toda requisição autenticada, inclusive a compra.
    extra: Vec<(String, String)>,
    on_pace: Option<PaceHook>,
    /// Até quando `action` lê sem login (depois de um 429 logado).
    anon_reads_until: Option<Instant>,
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
                breakers: [Breaker::new(); 6],
                cookie: None,
                extra: Vec::new(),
                on_pace: None,
                anon_reads_until: None,
            }),
        })
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
        self.inner.lock().unwrap().limiter.gap = gap;
    }

    pub fn stats(&self) -> Stats {
        self.inner.lock().unwrap().limiter.stats(Instant::now())
    }

    /// Avisa o gancho de ritmo ainda sob o lock: a ordem dos avisos é a ordem dos 429 (o gancho não pode
    /// chamar o cliente).
    fn notify_pace(g: &mut Inner, now: Instant) {
        if let Some(f) = g.on_pace.clone() {
            f(Self::saved_pace(g, now));
        }
    }

    /// Pausa e intervalo salvos depois do último 429 (`fim da pausa Unix|intervalo ms|próxima pausa ms|fim
    /// das leituras sem login Unix`). Sem pausa ativa, o 1º campo é a hora do último 429: ao subir, a
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
        format!(
            "{}|{}|{}|{anon}",
            s.paused_until.or(last_429).unwrap_or(0),
            s.gap_ms,
            g.limiter.pause_len.as_millis()
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
        let now = Instant::now();
        let ms = |v: i64| Duration::from_millis(v.max(0) as u64);
        let g = &mut *self.inner.lock().unwrap();
        let anon_left = anon.and_then(|r| r.ok()).unwrap_or(0) - unix_in(Duration::ZERO);
        if anon_left > 0 {
            g.anon_reads_until =
                Some(now + Duration::from_secs(anon_left as u64).min(ANON_AFTER_429));
        }
        let l = &mut g.limiter;
        l.gap = ms(gap).clamp(MIN_GAP, MAX_GAP);
        let quiet = unix_in(Duration::ZERO) - until >= PAUSE_RESET_AFTER.as_secs() as i64;
        if let Some(Ok(p)) = pause.filter(|_| !quiet) {
            l.pause_len = ms(p).clamp(PAUSE_START, PAUSE_MAX);
            // A escalada segue valendo depois de subir, contada da hora salva (não do reinício).
            // shortcut: salvo durante uma pausa, conta do fim da pausa (mais cauteloso que o 429 real); gravar a
            // hora do 429 num 5º campo se a escalada longa atrapalhar.
            let ago = (unix_in(Duration::ZERO) - until).max(0) as u64;
            l.last_429 = Some(now.checked_sub(Duration::from_secs(ago)).unwrap_or(now));
        }
        l.last_adjust = now;
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
        for attempt in 0..2 {
            let req = self
                .http
                .get(self.url("/market/actions"))
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
        // Sessão que mandou a consulta: cookies da resposta só valem para ela (o login pode mudar no meio).
        let mut sent_by: Option<String> = None;
        if auth {
            let g = self.inner.lock().unwrap();
            // Decidido depois da vaga: um 429 logado de outra consulta durante a espera já vale para esta.
            let anon = matches!(ep, Endpoint::Listings | Endpoint::History)
                && g.anon_reads_until.is_some_and(|t| Instant::now() < t);
            if let Some(c) = g.cookie.as_ref().filter(|_| !anon) {
                sent_by = Some(c.steam_login_secure.clone());
                let mut h = c.header();
                for (k, v) in &g.extra {
                    h.push_str(&format!("; {k}={v}"));
                }
                req = req.header(COOKIE, h);
            }
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
        if auth {
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
            let retry = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut g = self.inner.lock().unwrap();
            if sent_by.is_some() && matches!(ep, Endpoint::Listings | Endpoint::History) {
                // A Steam segurou a conta, não o IP: sem pausa, a próxima leitura (no ritmo) vai sem login.
                // 429 de uma sessão que já saiu não marca a conta nova.
                let current = g.cookie.as_ref().map(|c| c.steam_login_secure.as_str());
                if current == sent_by.as_deref() {
                    g.anon_reads_until = Some(now + ANON_AFTER_429);
                    Self::notify_pace(&mut g, now);
                }
                drop(g);
                tracing::warn!(endpoint = ?ep, "Steam: 429 logado; leituras seguem sem login por 6 h");
                return Err(Error::RateLimited {
                    until: unix_in(Duration::ZERO),
                });
            }
            g.limiter.on_429(now, retry);
            let until = g
                .limiter
                .stats(now)
                .paused_until
                .unwrap_or_else(|| unix_in(PAUSE_START));
            Self::notify_pace(&mut g, now);
            drop(g);
            // Qual consulta levou o 429 (e se ia logada): sem isso não dá para saber o que a Steam segura.
            tracing::warn!(endpoint = ?ep, logado = sent_by.is_some(), "Steam: 429");
            return Err(Error::RateLimited { until });
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::Transport(e.without_url().to_string()))?;
        let bad_envelope = check.and_then(|f| f(status.as_u16(), &body));
        {
            let mut g = self.inner.lock().unwrap();
            g.limiter.on_ok(now);
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
            body.to_vec(),
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
        // e um 429 depois de 60 min quietos recomeça em 60 s, sem precisar de resposta boa no meio
        let mut l = Limiter::new(t0());
        l.on_429(t0(), None);
        l.on_429(t0() + Duration::from_secs(120), None);
        let later = t0() + Duration::from_secs(120) + PAUSE_RESET_AFTER;
        l.on_429(later, None);
        assert_eq!(l.paused_until.unwrap().duration_since(later).as_secs(), 60);
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
        // 429 logado: sem pausa nem alongamento; a próxima vai sem login.
        let e = c
            .action(Endpoint::History, "Limit", &serde_json::json!([1]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::RateLimited { .. }));
        let s = c.stats();
        assert_eq!((s.paused_until, s.gap_ms), (None, 100));
        // O prazo sem login sobrevive a um reinício.
        let v = saved.lock().unwrap().clone();
        let anon: i64 = v.rsplit('|').next().unwrap().parse().unwrap();
        assert!(anon > unix_in(Duration::from_secs(5 * 3600)), "{v}");
        let again = fast(&base);
        again.set_cookie(c.cookie());
        again.restore_pace(&v);
        again
            .action(Endpoint::Listings, "Anon", &serde_json::json!([1]))
            .await
            .unwrap();
        c.action(Endpoint::Listings, "Anon", &serde_json::json!([1]))
            .await
            .unwrap();
        // 429 sem login: aí é o IP, e o limitador pausa.
        let e = c
            .action(Endpoint::Listings, "Limit", &serde_json::json!([1]))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::RateLimited { .. }));
        let s = c.stats();
        assert!(s.paused_until.is_some());
        assert_eq!(s.gap_ms, 150);
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
            saved.lock().unwrap().ends_with("|0"),
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
