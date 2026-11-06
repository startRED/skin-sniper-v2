//! Login da Steam pelo QR do app: nenhuma senha passa por aqui.
//!
//! Fluxo (docs/pesquisa-web.md, "Login por QR"): `BeginAuthSessionViaQR` devolve o endereço que vira o
//! QR; o app do celular aprova; `PollAuthSessionStatus` entrega `refresh_token` (meses) e `access_token`
//! (~1 dia). O cookie `steamLoginSecure` é `<steamid>%7C%7C<access_token>`. Para renovar, o
//! `GenerateAccessTokenForApp` troca o refresh token por outro access token.
//!
//! Escolhi a plataforma **MobileApp**: só os tokens dela renovam por `GenerateAccessTokenForApp`
//! (WebBrowser dá AccessDenied) e o `aud` deles inclui `web`, então valem no steamcommunity.com.
//!
//! Os endpoints pedem protobuf (`input_protobuf_encoded`, base64, num formulário POST) e respondem
//! protobuf binário com `x-eresult`. São poucos campos, codificados à mão abaixo.
//! O `refresh_token` fica só nas configurações (chave interna, nunca no painel) e nunca é logado.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::runtime::Runtime;
use crate::settings::SettingsStore;
use crate::steam::client::{Endpoint, Error as ClientError, SteamClient};

const API: &str = "https://api.steampowered.com";
const REFRESH_KEY: &str = crate::settings::REFRESH_KEY;
const COOKIE_KEY: &str = "steam_login_secure";
/// Quanto o QR vale. HIPÓTESE: a Steam não documenta; o app recusa QR velho e a consulta falha.
const QR_LIFETIME: Duration = Duration::from_secs(5 * 60);
/// Renova quando faltar menos que isto para o `exp` do access token.
const RENEW_BEFORE: i64 = 15 * 60;
const RENEW_CHECK: Duration = Duration::from_secs(60);
const MIN_POLL_GAP: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------------------------
// Protobuf à mão: varint (tipo 0), 64 bits (1), com tamanho (2), 32 bits (5).

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_varint(out: &mut Vec<u8>, field: u32, v: u64) {
    varint(out, u64::from(field) << 3);
    varint(out, v);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    varint(out, u64::from(field) << 3 | 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn put_fixed64(out: &mut Vec<u8>, field: u32, v: u64) {
    varint(out, u64::from(field) << 3 | 1);
    out.extend_from_slice(&v.to_le_bytes());
}

#[derive(Debug, PartialEq)]
enum Val<'a> {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

/// Lê todos os campos de uma mensagem. Tipo desconhecido ou corte no meio é erro.
fn fields(mut b: &[u8]) -> Result<Vec<(u32, Val<'_>)>> {
    fn read_varint(b: &mut &[u8]) -> Result<u64> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let (&byte, rest) = b.split_first().ok_or_else(|| anyhow!("varint cortado"))?;
            *b = rest;
            if shift >= 64 {
                bail!("varint grande demais");
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn take<'a>(b: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        if b.len() < n {
            bail!("campo cortado");
        }
        let (head, rest) = b.split_at(n);
        *b = rest;
        Ok(head)
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        let tag = read_varint(&mut b)?;
        let field = (tag >> 3) as u32;
        out.push((
            field,
            match tag & 7 {
                0 => Val::Varint(read_varint(&mut b)?),
                1 => Val::Fixed64(u64::from_le_bytes(take(&mut b, 8)?.try_into()?)),
                2 => {
                    let n = read_varint(&mut b)? as usize;
                    Val::Bytes(take(&mut b, n)?)
                }
                5 => Val::Fixed32(u32::from_le_bytes(take(&mut b, 4)?.try_into()?)),
                t => bail!("tipo de campo {t} não suportado"),
            },
        ));
    }
    Ok(out)
}

fn text(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

// ---------------------------------------------------------------------------------------------
// Mensagens (campos conforme SteamDatabase/Protobufs, webui/service_authentication.proto).

/// `BeginAuthSessionViaQR_Request`: só `device_details` (campo 3), como a biblioteca node-steam-session.
/// Dispositivo: "Galaxy S25", platform_type 3 (MobileApp), os_type -500 (AndroidUnknown), tipo 528.
fn begin_request() -> Vec<u8> {
    let mut d = Vec::new();
    put_bytes(&mut d, 1, b"Galaxy S25");
    put_varint(&mut d, 2, 3);
    put_varint(&mut d, 3, -500i64 as u64); // int32 negativo vai estendido a 64 bits
    put_varint(&mut d, 4, 528);
    let mut out = Vec::new();
    put_bytes(&mut out, 3, &d);
    out
}

#[derive(Debug, PartialEq)]
struct Begin {
    client_id: u64,
    challenge_url: String,
    request_id: Vec<u8>,
    interval: f32,
}

fn parse_begin(b: &[u8]) -> Result<Begin> {
    let (mut client_id, mut url, mut request_id, mut interval) = (None, None, None, 5.0);
    for (f, v) in fields(b)? {
        match (f, v) {
            (1, Val::Varint(n)) => client_id = Some(n),
            (2, Val::Bytes(s)) => url = Some(text(s)),
            (3, Val::Bytes(s)) => request_id = Some(s.to_vec()),
            (4, Val::Fixed32(n)) => interval = f32::from_bits(n),
            _ => {}
        }
    }
    match (client_id, url, request_id) {
        (Some(client_id), Some(challenge_url), Some(request_id)) => Ok(Begin {
            client_id,
            challenge_url,
            request_id,
            interval,
        }),
        _ => bail!("resposta do BeginAuthSessionViaQR sem client_id/challenge_url/request_id"),
    }
}

fn poll_request(client_id: u64, request_id: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, 1, client_id);
    put_bytes(&mut out, 2, request_id);
    out
}

#[derive(Debug, Default, PartialEq)]
struct Poll {
    new_client_id: Option<u64>,
    new_challenge_url: Option<String>,
    refresh_token: Option<String>,
    access_token: Option<String>,
    account_name: Option<String>,
}

fn parse_poll(b: &[u8]) -> Result<Poll> {
    let mut p = Poll::default();
    for (f, v) in fields(b)? {
        match (f, v) {
            (1, Val::Varint(n)) => p.new_client_id = Some(n),
            (2, Val::Bytes(s)) => p.new_challenge_url = Some(text(s)),
            (3, Val::Bytes(s)) => p.refresh_token = Some(text(s)),
            (4, Val::Bytes(s)) => p.access_token = Some(text(s)),
            (6, Val::Bytes(s)) => p.account_name = Some(text(s)),
            _ => {}
        }
    }
    Ok(p)
}

/// `GenerateAccessTokenForApp_Request`: refresh_token, steamid (fixed64); renewal_type 0 (não renova o refresh).
fn renew_request(refresh_token: &str, steamid: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, 1, refresh_token.as_bytes());
    put_fixed64(&mut out, 2, steamid);
    out
}

fn parse_renew(b: &[u8]) -> Result<String> {
    fields(b)?
        .into_iter()
        .find_map(|(f, v)| match (f, v) {
            (1, Val::Bytes(s)) => Some(text(s)),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("resposta do GenerateAccessTokenForApp sem access_token"))
}

// ---------------------------------------------------------------------------------------------
// JWT e cookie.

fn b64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &x)| n | u32::from(x) << (16 - 8 * i));
        for i in 0..=c.len() {
            s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
        s.extend(std::iter::repeat_n('=', 3 - c.len()));
    }
    s
}

/// Base64 do JWT (alfabeto de URL, sem preenchimento).
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for ch in s.trim_end_matches('=').bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[derive(Debug, PartialEq)]
struct Claims {
    exp: Option<i64>,
    sub: Option<u64>,
}

fn jwt_claims(jwt: &str) -> Option<Claims> {
    let payload = b64url_decode(jwt.split('.').nth(1)?)?;
    let v: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    Some(Claims {
        exp: v.get("exp").and_then(|e| e.as_i64()),
        sub: v.get("sub").and_then(|s| s.as_str()?.parse().ok()),
    })
}

fn cookie_value(steamid: u64, access_token: &str) -> String {
    format!("{steamid}%7C%7C{access_token}")
}

/// `exp` do access token guardado num cookie `steamLoginSecure` (aceita `||` e `%7C%7C`).
pub(crate) fn cookie_exp(cookie: &str) -> Option<i64> {
    let token = cookie
        .rsplit_once("%7C%7C")
        .or_else(|| cookie.rsplit_once("||"))?
        .1;
    jwt_claims(token)?.exp
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Agenda: renova se o `exp` não se lê ou se falta pouco.
fn needs_renewal(exp: Option<i64>, now: i64) -> bool {
    exp.is_none_or(|e| e - now <= RENEW_BEFORE)
}

fn qr_svg(url: &str) -> Result<String> {
    use qrcode::{QrCode, render::svg};
    Ok(QrCode::new(url.as_bytes())?
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build())
}

// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Waiting,
    Done,
    Expired,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct QrStart {
    pub qr_svg: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Só em `waiting`: a Steam troca o endereço do QR de tempos em tempos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_svg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Status {
    fn of(state: State) -> Self {
        Status {
            state,
            account: None,
            qr_svg: None,
            detail: None,
        }
    }
}

struct Pending {
    /// Número da tentativa: resposta de um QR já substituído não pode gravar nada.
    attempt: u64,
    client_id: u64,
    request_id: Vec<u8>,
    interval: Duration,
    expires_at: i64,
    qr_url: String,
    last_poll: Option<Instant>,
    /// Resultado final; guardado para a tela perguntar de novo sem refazer nada.
    outcome: Option<Status>,
}

/// Resultado de uma renovação.
#[derive(Debug, PartialEq)]
pub enum Renewal {
    /// Sem refresh token (cookie colado à mão), cookie ainda com tempo, ou a sessão guardada mudou
    /// durante a renovação (resultado descartado).
    NotNeeded,
    Renewed,
    /// A Steam recusou o refresh token: só escaneando o QR de novo.
    Expired,
    /// Falha passada (rede, limite); tenta no próximo ciclo.
    Retry(String),
}

pub struct Login {
    settings: SettingsStore,
    /// Cliente da Steam: todo pedido passa pelo limitador global e pelo recuo de 429 dele.
    steam: OnceLock<Arc<SteamClient>>,
    base: String,
    pending: Mutex<Option<Pending>>,
    attempts: AtomicU64,
    /// Serializa a troca de tentativa e a gravação do login, para uma resposta atrasada não vencer.
    write_lock: tokio::sync::Mutex<()>,
}

impl Login {
    pub fn new(settings: SettingsStore) -> Arc<Self> {
        Self::with_base(settings, API, None)
    }

    fn with_base(
        settings: SettingsStore,
        base: &str,
        steam: Option<Arc<SteamClient>>,
    ) -> Arc<Self> {
        let cell = OnceLock::new();
        if let Some(s) = steam {
            let _ = cell.set(s);
        }
        Arc::new(Login {
            settings,
            steam: cell,
            base: base.trim_end_matches('/').to_string(),
            pending: Mutex::new(None),
            attempts: AtomicU64::new(0),
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Liga ao cliente da Steam do programa (o mesmo limitador da caça). Sem isso nada sai.
    pub fn attach(&self, steam: Arc<SteamClient>) {
        let _ = self.steam.set(steam);
    }

    /// POST do método com a mensagem em `input_protobuf_encoded`, pelo limitador global.
    /// Devolve o corpo protobuf. Erro: HTTP fora de 2xx ou `x-eresult` diferente de 1 (OK); a mensagem
    /// não leva dado da resposta.
    async fn call(&self, method: &str, msg: &[u8]) -> Result<Vec<u8>, CallError> {
        let Some(steam) = self.steam.get() else {
            return Err(CallError::Transient("cliente da Steam não ligado".into()));
        };
        let req = steam
            .request_url(
                reqwest::Method::POST,
                &format!("{}/IAuthenticationService/{method}/v1/", self.base),
            )
            .header("user-agent", "okhttp/4.9.2")
            .form(&[("input_protobuf_encoded", b64_encode(msg))]);
        let (parts, body) = match steam.send(Endpoint::Auth, req, false).await {
            Ok(r) => r,
            // 401/403 da API = recusa; o resto (429, breaker, 5xx, rede) passa
            Err(ClientError::Http {
                status: 401 | 403, ..
            }) => {
                return Err(CallError::Refused(format!("{method}: HTTP recusado")));
            }
            Err(e) => return Err(CallError::Transient(format!("{method}: {e}"))),
        };
        match (parts.status, parts.eresult) {
            (200..=299, Some(1) | None) => Ok(body),
            // 3 sem conexão, 16 tempo esgotado, 20 serviço fora, 84 limite de pedidos
            (_, Some(3 | 16 | 20 | 84)) => Err(CallError::Transient(format!(
                "{method}: HTTP {} eresult {:?}",
                parts.status, parts.eresult
            ))),
            _ => Err(CallError::Refused(format!(
                "{method}: HTTP {} eresult {:?}",
                parts.status, parts.eresult
            ))),
        }
    }

    /// Pede um QR novo e descarta o anterior (respostas atrasadas dele deixam de valer).
    pub async fn start_qr(&self) -> Result<QrStart> {
        let begin = parse_begin(&self.call("BeginAuthSessionViaQR", &begin_request()).await?)?;
        let svg = qr_svg(&begin.challenge_url)?;
        let expires_at = now_unix() + QR_LIFETIME.as_secs() as i64;
        let _w = self.write_lock.lock().await;
        *self.pending.lock().unwrap() = Some(Pending {
            attempt: self.attempts.fetch_add(1, Ordering::SeqCst) + 1,
            client_id: begin.client_id,
            request_id: begin.request_id,
            interval: Duration::from_secs_f32(begin.interval.clamp(1.0, 30.0)).max(MIN_POLL_GAP),
            expires_at,
            qr_url: begin.challenge_url,
            last_poll: None,
            outcome: None,
        });
        Ok(QrStart {
            qr_svg: svg,
            expires_at,
        })
    }

    /// Consulta a Steam (no máximo uma vez por `interval`). `true` junto quando o login acabou de
    /// ser salvo: quem chamou deve aplicar as configurações.
    pub async fn poll(&self) -> (Status, bool) {
        let (attempt, client_id, request_id) = {
            let mut g = self.pending.lock().unwrap();
            let Some(p) = g.as_mut() else {
                return (Status::of(State::Expired), false);
            };
            if let Some(o) = &p.outcome {
                return (o.clone(), false);
            }
            if now_unix() > p.expires_at {
                p.outcome = Some(Status::of(State::Expired));
                return (Status::of(State::Expired), false);
            }
            if p.last_poll.is_some_and(|t| t.elapsed() < p.interval) {
                return (waiting_of(p), false);
            }
            p.last_poll = Some(Instant::now());
            (p.attempt, p.client_id, p.request_id.clone())
        };
        let polled = self
            .call(
                "PollAuthSessionStatus",
                &poll_request(client_id, &request_id),
            )
            .await
            .and_then(|b| parse_poll(&b).map_err(|e| CallError::Refused(e.to_string())));
        // Daqui em diante só vale se esta ainda é a tentativa atual (outro QR pode ter aberto no meio).
        let _w = self.write_lock.lock().await;
        if !self.is_current(attempt) {
            return (self.waiting_status(), false);
        }
        let poll = match polled {
            Ok(p) => p,
            // falha passageira: segue esperando; o QR expira sozinho
            Err(CallError::Transient(_)) => return (self.waiting_status(), false),
            Err(CallError::Refused(d)) => {
                tracing::warn!("login por QR recusado: {d}");
                return self.finish(
                    attempt,
                    Status {
                        detail: Some(d),
                        ..Status::of(State::Error)
                    },
                );
            }
        };
        if let (Some(refresh), Some(access)) = (&poll.refresh_token, &poll.access_token) {
            let Some(steamid) = jwt_claims(refresh).and_then(|c| c.sub) else {
                return self.finish(
                    attempt,
                    Status {
                        detail: Some("a Steam devolveu um token que não consegui ler".into()),
                        ..Status::of(State::Error)
                    },
                );
            };
            let saved = async {
                let before = self.settings.get(REFRESH_KEY).await?;
                self.settings
                    .write_if(
                        (REFRESH_KEY, before),
                        vec![
                            (REFRESH_KEY, Some(refresh.clone())),
                            (COOKIE_KEY, Some(cookie_value(steamid, access))),
                        ],
                    )
                    .await
            }
            .await;
            if !matches!(saved, Ok(true)) {
                tracing::error!("login por QR: não consegui salvar o login");
                return self.finish(
                    attempt,
                    Status {
                        detail: Some("não consegui salvar o login".into()),
                        ..Status::of(State::Error)
                    },
                );
            }
            let (status, _) = self.finish(
                attempt,
                Status {
                    account: Some(poll.account_name.unwrap_or_else(|| steamid.to_string())),
                    ..Status::of(State::Done)
                },
            );
            return (status, true);
        }
        if let Some(p) = self.pending.lock().unwrap().as_mut() {
            if let Some(id) = poll.new_client_id {
                p.client_id = id;
            }
            if let Some(u) = poll.new_challenge_url {
                p.qr_url = u;
            }
        }
        (self.waiting_status(), false)
    }

    fn is_current(&self, attempt: u64) -> bool {
        self.pending
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.attempt == attempt && p.outcome.is_none())
    }

    fn waiting_status(&self) -> Status {
        match self.pending.lock().unwrap().as_ref() {
            Some(p) => waiting_of(p),
            None => Status::of(State::Expired),
        }
    }

    fn finish(&self, attempt: u64, s: Status) -> (Status, bool) {
        if let Some(p) = self.pending.lock().unwrap().as_mut()
            && p.attempt == attempt
        {
            p.outcome = Some(s.clone());
        }
        (s, false)
    }

    /// Troca o refresh token por um access token novo quando o cookie está perto de vencer.
    /// O resultado só é gravado se o refresh token guardado ainda for o que iniciou a renovação.
    pub async fn renew_if_needed(&self) -> Renewal {
        let read = |k| async move { self.settings.get(k).await };
        let (Ok(Some(refresh)), cookie) = (read(REFRESH_KEY).await, read(COOKIE_KEY).await) else {
            return Renewal::NotNeeded;
        };
        let exp = cookie.ok().flatten().and_then(|c| cookie_exp(&c));
        if !needs_renewal(exp, now_unix()) {
            return Renewal::NotNeeded;
        }
        let Some(steamid) = jwt_claims(&refresh).and_then(|c| c.sub) else {
            return self.expire(refresh).await;
        };
        let renewed = self
            .call(
                "GenerateAccessTokenForApp",
                &renew_request(&refresh, steamid),
            )
            .await
            .and_then(|b| parse_renew(&b).map_err(|e| CallError::Refused(e.to_string())));
        match renewed {
            Ok(access) => match self
                .settings
                .write_if(
                    (REFRESH_KEY, Some(refresh)),
                    vec![(COOKIE_KEY, Some(cookie_value(steamid, &access)))],
                )
                .await
            {
                Ok(true) => Renewal::Renewed,
                // outra conta entrou (ou o cookie foi trocado à mão) durante a renovação: descarta
                Ok(false) => Renewal::NotNeeded,
                Err(e) => Renewal::Retry(format!("não consegui salvar: {e:#}")),
            },
            Err(CallError::Transient(d)) => Renewal::Retry(d),
            Err(CallError::Refused(d)) => {
                tracing::warn!("renovação do login recusada: {d}");
                self.expire(refresh).await
            }
        }
    }

    /// Esquece o refresh token `old` (só se ainda for ele): sem o token só um QR novo resolve.
    async fn expire(&self, old: String) -> Renewal {
        match self
            .settings
            .write_if((REFRESH_KEY, Some(old)), vec![(REFRESH_KEY, None)])
            .await
        {
            Ok(true) => Renewal::Expired,
            _ => Renewal::NotNeeded,
        }
    }

    /// Confere a cada minuto se o cookie precisa de renovação e aplica o resultado.
    pub fn spawn_renewal(self: &Arc<Self>, rt: Arc<Runtime>) {
        let login = self.clone();
        tokio::spawn(async move {
            loop {
                match login.renew_if_needed().await {
                    Renewal::Renewed => {
                        if let Err(e) = rt.settings_changed().await {
                            tracing::warn!("login renovado, mas não apliquei: {e:#}");
                        }
                    }
                    Renewal::Expired => rt.mark_login_expired(),
                    Renewal::Retry(d) => tracing::warn!("renovação do login adiada: {d}"),
                    Renewal::NotNeeded => {}
                }
                tokio::time::sleep(RENEW_CHECK).await;
            }
        });
    }
}

fn waiting_of(p: &Pending) -> Status {
    Status {
        qr_svg: qr_svg(&p.qr_url).ok(),
        ..Status::of(State::Waiting)
    }
}

enum CallError {
    /// Tentar de novo depois resolve (rede, 5xx, limite).
    Transient(String),
    /// A Steam disse não, ou a resposta não faz sentido.
    Refused(String),
}

impl From<CallError> for anyhow::Error {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Transient(d) | CallError::Refused(d) => anyhow!(d),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::temp_db;

    // Fixtures abaixo: FORMA DOCUMENTADA (campos do .proto da SteamDatabase), montadas pelo codificador
    // deste arquivo, não capturas da Steam. O teste ao vivo (`live_begin_qr`) confere o Begin de verdade.

    fn jwt(payload: &str) -> String {
        let enc = b64_encode(payload.as_bytes())
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_");
        format!("e30.{enc}.assinatura")
    }

    #[test]
    fn begin_request_is_documented_device_details() {
        let b = begin_request();
        let outer = fields(&b).unwrap();
        let Val::Bytes(d) = outer[0].1 else { panic!() };
        assert_eq!(outer[0].0, 3);
        let inner = fields(d).unwrap();
        assert_eq!(inner[0], (1, Val::Bytes(b"Galaxy S25")));
        assert_eq!(inner[1], (2, Val::Varint(3)));
        assert_eq!(inner[2], (3, Val::Varint(-500i64 as u64)));
        assert_eq!(inner[3], (4, Val::Varint(528)));
    }

    #[test]
    fn begin_response_roundtrip() {
        let mut b = Vec::new();
        put_varint(&mut b, 1, 987654321012345);
        put_bytes(&mut b, 2, b"https://s.team/q/1/987654321012345");
        put_bytes(&mut b, 3, &[1, 2, 3, 250]);
        varint(&mut b, 4 << 3 | 5);
        b.extend_from_slice(&5.0f32.to_le_bytes());
        put_bytes(&mut b, 5, &[8, 1]); // allowed_confirmations: ignorado
        assert_eq!(
            parse_begin(&b).unwrap(),
            Begin {
                client_id: 987654321012345,
                challenge_url: "https://s.team/q/1/987654321012345".into(),
                request_id: vec![1, 2, 3, 250],
                interval: 5.0,
            }
        );
        assert!(parse_begin(&b[..b.len() - 3]).is_err(), "corte no meio");
        assert!(parse_begin(&[]).is_err(), "sem campos obrigatórios");
    }

    #[test]
    fn poll_messages() {
        let req = poll_request(300, &[9, 8]);
        assert_eq!(
            fields(&req).unwrap(),
            vec![(1, Val::Varint(300)), (2, Val::Bytes(&[9, 8]))]
        );
        assert_eq!(
            parse_poll(&[]).unwrap(),
            Poll::default(),
            "ainda esperando: corpo vazio"
        );
        let mut b = Vec::new();
        put_bytes(&mut b, 3, b"refresh");
        put_bytes(&mut b, 4, b"access");
        put_bytes(&mut b, 6, b"conta");
        let p = parse_poll(&b).unwrap();
        assert_eq!(p.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(p.access_token.as_deref(), Some("access"));
        assert_eq!(p.account_name.as_deref(), Some("conta"));
    }

    #[test]
    fn renew_messages() {
        let req = renew_request("tok", 76561198000000001);
        assert_eq!(
            fields(&req).unwrap(),
            vec![
                (1, Val::Bytes(b"tok")),
                (2, Val::Fixed64(76561198000000001))
            ]
        );
        let mut b = Vec::new();
        put_bytes(&mut b, 1, b"novo");
        assert_eq!(parse_renew(&b).unwrap(), "novo");
        assert!(parse_renew(&[]).is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64url_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(b64url_decode("Zm8").unwrap(), b"fo");
        assert!(b64url_decode("a*b").is_none());
    }

    #[test]
    fn jwt_exp_and_sub() {
        let t = jwt(
            r#"{"iss":"r:0001","sub":"76561198000000001","exp":1800000000,"aud":["web","mobile"]}"#,
        );
        assert_eq!(
            jwt_claims(&t),
            Some(Claims {
                exp: Some(1800000000),
                sub: Some(76561198000000001)
            })
        );
        assert_eq!(
            cookie_exp(&cookie_value(76561198000000001, &t)),
            Some(1800000000)
        );
        assert_eq!(cookie_exp(&format!("7656||{t}")), Some(1800000000));
        assert_eq!(cookie_exp("sem-jwt"), None);
        assert!(jwt_claims("lixo").is_none());
    }

    #[test]
    fn renewal_schedule() {
        let now = 1_000_000;
        assert!(!needs_renewal(Some(now + RENEW_BEFORE + 1), now));
        assert!(needs_renewal(Some(now + RENEW_BEFORE), now));
        assert!(needs_renewal(Some(now - 5), now), "já vencido");
        assert!(needs_renewal(None, now), "não consegui ler o exp");
    }

    #[test]
    fn qr_is_svg() {
        let s = qr_svg("https://s.team/q/1/42").unwrap();
        assert!(s.contains("<svg"));
    }

    /// Servidor Steam de mentira: devolve as mensagens montadas acima.
    async fn fake_steam(poll_done: bool) -> String {
        use axum::{Router, http::HeaderMap, routing::post};
        let t = jwt(&format!(
            r#"{{"sub":"76561198000000001","exp":{}}}"#,
            now_unix() + 86_400
        ));
        let (refresh, access) = (jwt(r#"{"sub":"76561198000000001","exp":9999999999}"#), t);
        let app = Router::new()
            .route(
                "/IAuthenticationService/BeginAuthSessionViaQR/v1/",
                post(|| async {
                    let mut b = Vec::new();
                    put_varint(&mut b, 1, 7);
                    put_bytes(&mut b, 2, b"https://s.team/q/1/7");
                    put_bytes(&mut b, 3, &[1, 2]);
                    ([("x-eresult", "1")], b)
                }),
            )
            .route(
                "/IAuthenticationService/PollAuthSessionStatus/v1/",
                post(move |h: HeaderMap, body: String| {
                    let (refresh, access) = (refresh.clone(), access.clone());
                    async move {
                        assert!(h["content-type"].to_str().unwrap().contains("urlencoded"));
                        assert!(body.starts_with("input_protobuf_encoded="));
                        let mut b = Vec::new();
                        if poll_done {
                            put_bytes(&mut b, 3, refresh.as_bytes());
                            put_bytes(&mut b, 4, access.as_bytes());
                            put_bytes(&mut b, 6, b"minha_conta");
                        }
                        ([("x-eresult", "1")], b)
                    }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn qr_flow_waiting_then_done_saves_cookie_and_refresh() {
        for done in [false, true] {
            let (db, _dir) = temp_db();
            let settings = SettingsStore::new(db);
            let login = Login::with_base(
                settings.clone(),
                &fake_steam(done).await,
                Some(test_client()),
            );
            let q = login.start_qr().await.unwrap();
            assert!(q.qr_svg.contains("<svg") && q.expires_at > now_unix());
            let (st, applied) = login.poll().await;
            if done {
                assert!(applied && st.state == State::Done);
                assert_eq!(st.account.as_deref(), Some("minha_conta"));
                let cookie = settings.get(COOKIE_KEY).await.unwrap().unwrap();
                assert!(cookie.starts_with("76561198000000001%7C%7C"));
                assert!(settings.get(REFRESH_KEY).await.unwrap().is_some());
                // segunda consulta devolve o mesmo resultado sem aplicar de novo
                let (again, applied) = login.poll().await;
                assert!(again.state == State::Done && !applied);
            } else {
                assert!(!applied && st.state == State::Waiting && st.qr_svg.is_some());
            }
        }
    }

    #[tokio::test]
    async fn poll_without_qr_or_after_expiry_is_expired() {
        let (db, _dir) = temp_db();
        let login = Login::with_base(
            SettingsStore::new(db),
            "http://127.0.0.1:1",
            Some(test_client()),
        );
        assert_eq!(login.poll().await.0.state, State::Expired);
    }

    #[tokio::test]
    async fn renewal_only_when_close_to_expiry() {
        let (db, _dir) = temp_db();
        let settings = SettingsStore::new(db);
        let login = Login::with_base(settings.clone(), "http://127.0.0.1:1", Some(test_client()));
        // sem refresh token (cookie colado à mão): nada a fazer
        assert_eq!(login.renew_if_needed().await, Renewal::NotNeeded);
        let refresh = jwt(r#"{"sub":"76561198000000001","exp":9999999999}"#);
        settings.set_raw(REFRESH_KEY, Some(refresh)).await.unwrap();
        let fresh = jwt(&format!(r#"{{"exp":{}}}"#, now_unix() + 86_400));
        settings
            .set_raw(COOKIE_KEY, Some(cookie_value(1, &fresh)))
            .await
            .unwrap();
        assert_eq!(login.renew_if_needed().await, Renewal::NotNeeded);
        // perto de vencer: tenta renovar; a rede falha (porta 1), então só adia
        let old = jwt(&format!(r#"{{"exp":{}}}"#, now_unix() + 60));
        settings
            .set_raw(COOKIE_KEY, Some(cookie_value(1, &old)))
            .await
            .unwrap();
        assert!(matches!(login.renew_if_needed().await, Renewal::Retry(_)));
        assert!(settings.get(REFRESH_KEY).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn refused_renewal_forgets_refresh_token() {
        use axum::{Router, http::StatusCode, routing::post};
        let app = Router::new().route(
            "/IAuthenticationService/GenerateAccessTokenForApp/v1/",
            post(|| async { (StatusCode::FORBIDDEN, [("x-eresult", "15")], "") }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let (db, _dir) = temp_db();
        let settings = SettingsStore::new(db);
        let login = Login::with_base(
            settings.clone(),
            &format!("http://{addr}"),
            Some(test_client()),
        );
        let refresh = jwt(r#"{"sub":"76561198000000001","exp":9999999999}"#);
        settings.set_raw(REFRESH_KEY, Some(refresh)).await.unwrap();
        assert_eq!(login.renew_if_needed().await, Renewal::Expired);
        assert!(settings.get(REFRESH_KEY).await.unwrap().is_none());
    }

    fn test_client() -> Arc<SteamClient> {
        let c = Arc::new(SteamClient::with_base("http://127.0.0.1:1").unwrap());
        c.set_gap_for_test(Duration::ZERO);
        c
    }

    /// Servidor de mentira com atraso controlado: o Poll do client_id 1 demora e entrega tokens; o
    /// GenerateAccessTokenForApp também demora. `status` permite simular 429.
    async fn slow_steam(begin_status: u16) -> String {
        use axum::{Router, http::StatusCode, routing::post};
        let counter = Arc::new(AtomicU64::new(0));
        let tokens = (
            jwt(r#"{"sub":"76561198000000001","exp":9999999999}"#),
            jwt(&format!(
                r#"{{"sub":"76561198000000001","exp":{}}}"#,
                now_unix() + 86_400
            )),
        );
        let app = Router::new()
            .route(
                "/IAuthenticationService/BeginAuthSessionViaQR/v1/",
                post(move || {
                    let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    async move {
                        let mut b = Vec::new();
                        put_varint(&mut b, 1, n);
                        put_bytes(&mut b, 2, format!("https://s.team/q/1/{n}").as_bytes());
                        put_bytes(&mut b, 3, &[n as u8]);
                        (
                            StatusCode::from_u16(begin_status).unwrap(),
                            [("x-eresult", "1")],
                            b,
                        )
                    }
                }),
            )
            .route(
                "/IAuthenticationService/PollAuthSessionStatus/v1/",
                post({
                    let (refresh, access) = tokens.clone();
                    move |body: String| {
                        let (refresh, access) = (refresh.clone(), access.clone());
                        async move {
                            let mut b = Vec::new();
                            // o QR "1" (A) responde tarde, já aprovado
                            if body.contains("input_protobuf_encoded=CAE") {
                                tokio::time::sleep(Duration::from_millis(400)).await;
                                put_bytes(&mut b, 3, refresh.as_bytes());
                                put_bytes(&mut b, 4, access.as_bytes());
                                put_bytes(&mut b, 6, b"conta_A");
                            }
                            ([("x-eresult", "1")], b)
                        }
                    }
                }),
            )
            .route(
                "/IAuthenticationService/GenerateAccessTokenForApp/v1/",
                post({
                    let access = tokens.1.clone();
                    move || {
                        let access = access.clone();
                        async move {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            let mut b = Vec::new();
                            put_bytes(&mut b, 1, access.as_bytes());
                            ([("x-eresult", "1")], b)
                        }
                    }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn late_answer_of_replaced_qr_is_discarded() {
        let (db, _dir) = temp_db();
        let settings = SettingsStore::new(db);
        let login = Login::with_base(
            settings.clone(),
            &slow_steam(200).await,
            Some(test_client()),
        );
        login.start_qr().await.unwrap(); // A
        let a = tokio::spawn({
            let l = login.clone();
            async move { l.poll().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        login.start_qr().await.unwrap(); // B substitui A enquanto a resposta de A não chegou
        let (st, applied) = a.await.unwrap();
        assert!(!applied && st.state == State::Waiting, "{st:?}");
        assert!(settings.get(COOKIE_KEY).await.unwrap().is_none());
        assert!(settings.get(REFRESH_KEY).await.unwrap().is_none());
        // B segue esperando: não foi marcado como concluído por causa de A
        let (st, applied) = login.poll().await;
        assert!(!applied && st.state == State::Waiting);
    }

    #[tokio::test]
    async fn renewal_in_flight_does_not_overwrite_a_newer_login() {
        let (db, _dir) = temp_db();
        let settings = SettingsStore::new(db);
        let login = Login::with_base(
            settings.clone(),
            &slow_steam(200).await,
            Some(test_client()),
        );
        let refresh_a = jwt(r#"{"sub":"76561198000000001","exp":9999999999}"#);
        let old = jwt(&format!(r#"{{"exp":{}}}"#, now_unix() + 60));
        settings
            .set_raw(REFRESH_KEY, Some(refresh_a))
            .await
            .unwrap();
        settings
            .set_raw(COOKIE_KEY, Some(cookie_value(1, &old)))
            .await
            .unwrap();
        let r = tokio::spawn({
            let l = login.clone();
            async move { l.renew_if_needed().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // enquanto a renovação de A voa, o QR de B grava a conta B
        let refresh_b = jwt(r#"{"sub":"76561198000000002","exp":9999999999}"#);
        settings
            .set_raw(REFRESH_KEY, Some(refresh_b.clone()))
            .await
            .unwrap();
        settings
            .set_raw(COOKIE_KEY, Some("B-cookie".into()))
            .await
            .unwrap();
        assert_eq!(r.await.unwrap(), Renewal::NotNeeded);
        assert_eq!(
            settings.get(COOKIE_KEY).await.unwrap().as_deref(),
            Some("B-cookie")
        );
        assert_eq!(settings.get(REFRESH_KEY).await.unwrap(), Some(refresh_b));
    }

    #[tokio::test]
    async fn login_requests_respect_control_backoff_without_pausing_market_reads() {
        let (db, _dir) = temp_db();
        let steam = test_client();
        let login = Login::with_base(
            SettingsStore::new(db),
            &slow_steam(429).await,
            Some(steam.clone()),
        );
        assert!(login.start_qr().await.is_err());
        assert!(steam.stats().last_query_at.is_some(), "contou no limitador");
        assert!(steam.blocked(Endpoint::Auth), "429 pausa autenticação");
        assert!(
            !steam.blocked(Endpoint::Listings),
            "login não pausa os anúncios"
        );
        // A segunda tentativa de login nem sai: o limitador de controle falha na hora.
        let sent = steam.sent();
        let second = tokio::time::timeout(Duration::from_millis(500), login.start_qr()).await;
        assert!(second.expect("não segura quem chamou").is_err());
        assert_eq!(steam.sent(), sent, "a segunda nem saiu");
    }

    /// AO VIVO: um único `BeginAuthSessionViaQR` para ver se a Steam aceita o pedido. Não completa login.
    #[tokio::test]
    #[ignore = "ao vivo: chama api.steampowered.com uma vez"]
    async fn live_begin_qr() {
        let (db, _dir) = temp_db();
        let login = Login::new(SettingsStore::new(db));
        login.attach(Arc::new(SteamClient::new().unwrap()));
        let raw = login
            .call("BeginAuthSessionViaQR", &begin_request())
            .await
            .map_err(anyhow::Error::from)
            .unwrap();
        let b = parse_begin(&raw).unwrap();
        assert!(b.challenge_url.starts_with("https://s.team/q/"));
        println!("interval={} bytes={}", b.interval, raw.len());
    }
}
