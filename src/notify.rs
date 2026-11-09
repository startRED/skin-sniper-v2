//! Aviso no Telegram (Bot API `sendMessage` e `getMe`), com token e chat id trocáveis em tempo de
//! execução. Ritmo ≥ 1 s entre mensagens e no máximo 1 aviso por anúncio.
//! O token nunca entra em log, `Debug` nem mensagem de erro (a URL da Bot API o contém).
#![allow(dead_code)]

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::pricing::brl;
use serde_json::{Value, json};

const BASE: &str = "https://api.telegram.org";
const MIN_GAP: Duration = Duration::from_secs(1);
/// Quantos ids já avisados lembrar (os mais antigos saem primeiro).
const REMEMBER: usize = 5000;

/// Dados de um anúncio para o aviso (valores em centavos BRL; `eta_hours` do `Sell.mid`).
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub id: String,
    pub name: String,
    pub group: Option<String>,
    pub price_cents: i64,
    pub mid_profit_cents: i64,
    pub safe_profit_cents: i64,
    pub eta_hours: Option<f64>,
    /// Raro: selo e preço sugerido, já em texto.
    pub seal: Option<String>,
}

#[derive(Debug)]
pub enum NotifyError {
    /// Falta token ou chat id.
    NotConfigured,
    /// O Telegram recusou (token inválido, chat desconhecido...). `ok` no painel vira "recusada".
    Refused(String),
    /// 429 do Telegram: tente de novo depois de `until` (segundos Unix).
    RateLimited {
        until: i64,
    },
    Transport(String),
    Parse(String),
}

impl fmt::Display for NotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => write!(f, "Telegram sem token ou chat id"),
            Self::Refused(d) => write!(f, "Telegram recusou: {d}"),
            Self::RateLimited { until } => write!(f, "Telegram pediu espera até {until}"),
            Self::Transport(e) => write!(f, "falha de rede: {e}"),
            Self::Parse(e) => write!(f, "resposta inesperada: {e}"),
        }
    }
}

impl std::error::Error for NotifyError {}

#[derive(Debug, Clone, PartialEq)]
pub struct BotInfo {
    pub username: Option<String>,
}

struct Inner {
    token: Option<String>,
    chat_id: Option<String>,
    next_at: Instant,
    blocked_until: Option<i64>,
    sent: HashSet<String>,
    order: VecDeque<String>,
}

pub struct Telegram {
    http: reqwest::Client,
    base: String,
    gap: Duration,
    inner: tokio::sync::Mutex<Inner>,
    // espelho do token/chat para `set_*` síncrono sem esperar um envio em andamento
    creds: Mutex<(Option<String>, Option<String>)>,
}

impl fmt::Debug for Telegram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Telegram(<oculto>)")
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Telegram {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_base(BASE, MIN_GAP)
    }

    pub(crate) fn with_base(base: &str, gap: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()?,
            base: base.trim_end_matches('/').to_string(),
            gap,
            inner: tokio::sync::Mutex::new(Inner {
                token: None,
                chat_id: None,
                next_at: Instant::now(),
                blocked_until: None,
                sent: HashSet::new(),
                order: VecDeque::new(),
            }),
            creds: Mutex::new((None, None)),
        })
    }

    /// Troca token e chat id (vazio = apaga). Vale para o próximo envio.
    pub fn set_credentials(&self, token: Option<String>, chat_id: Option<String>) {
        let clean = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        *self.creds.lock().unwrap() = (clean(token), clean(chat_id));
    }

    fn creds(&self) -> (Option<String>, Option<String>) {
        self.creds.lock().unwrap().clone()
    }

    /// `getMe`: valida o token. `Refused` = token recusado (estado "recusada"); erro de rede não diz nada
    /// sobre o token.
    pub async fn get_me(&self) -> Result<BotInfo, NotifyError> {
        let (Some(token), _) = self.creds() else {
            return Err(NotifyError::NotConfigured);
        };
        let v = self.call(&token, "getMe", None).await?;
        Ok(BotInfo {
            username: v
                .pointer("/result/username")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    /// Envia o aviso de um anúncio. `Ok(false)` = esse anúncio já foi avisado (nada enviado).
    /// Serializa as chamadas: segura o lock durante o intervalo e o envio, então nunca há dois iguais.
    pub async fn notify(
        &self,
        alert: &Alert,
        panel_url: Option<&str>,
    ) -> Result<bool, NotifyError> {
        let (Some(token), Some(chat)) = self.creds() else {
            return Err(NotifyError::NotConfigured);
        };
        let mut g = self.inner.lock().await;
        if g.sent.contains(&alert.id) {
            return Ok(false);
        }
        if let Some(until) = g.blocked_until.filter(|u| *u > now_unix()) {
            return Err(NotifyError::RateLimited { until });
        }
        let wait = g.next_at.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        g.next_at = Instant::now() + self.gap;
        let body = json!({
            "chat_id": chat,
            "text": format_message(alert, panel_url),
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        match self.call(&token, "sendMessage", Some(body)).await {
            Ok(_) => {
                g.sent.insert(alert.id.clone());
                g.order.push_back(alert.id.clone());
                while g.order.len() > REMEMBER {
                    if let Some(old) = g.order.pop_front() {
                        g.sent.remove(&old);
                    }
                }
                Ok(true)
            }
            Err(e) => {
                if let NotifyError::RateLimited { until } = &e {
                    g.blocked_until = Some(*until);
                }
                Err(e)
            }
        }
    }

    async fn call(
        &self,
        token: &str,
        method: &str,
        body: Option<Value>,
    ) -> Result<Value, NotifyError> {
        let url = format!("{}/bot{token}/{method}", self.base);
        let req = match body {
            Some(b) => self.http.post(url).json(&b),
            None => self.http.get(url),
        };
        // `without_url`: a URL carrega o token
        let resp = req
            .send()
            .await
            .map_err(|e| NotifyError::Transport(e.without_url().to_string()))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| NotifyError::Transport(e.without_url().to_string()))?;
        parse_reply(status.as_u16(), &bytes)
    }
}

/// `{"ok":true,...}` → corpo; `ok:false` → `Refused(description)` ou `RateLimited` (429 com `retry_after`).
pub(crate) fn parse_reply(status: u16, body: &[u8]) -> Result<Value, NotifyError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| NotifyError::Parse(format!("HTTP {status} sem JSON")))?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v);
    }
    if status == 429 || v.get("error_code").and_then(Value::as_i64) == Some(429) {
        let wait = v
            .pointer("/parameters/retry_after")
            .and_then(Value::as_i64)
            .unwrap_or(30)
            .max(1);
        return Err(NotifyError::RateLimited {
            until: now_unix() + wait,
        });
    }
    let d = v
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("sem descrição");
    Err(NotifyError::Refused(d.to_string()))
}

/// Escapa o texto para `parse_mode=HTML` do Telegram (`&`, `<`, `>`; `"` também, para atributos).
pub fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

fn eta(h: Option<f64>) -> String {
    match h {
        Some(h) if h.is_finite() && h > 0.0 => {
            if h < 48.0 {
                format!("~{} h", h.round().max(1.0))
            } else {
                format!("~{} dias", (h / 24.0).round())
            }
        }
        _ => "prazo incerto".into(),
    }
}

/// Mensagem curta em HTML: skin, grupo, preço, lucro médio/seguro, prazo e link `panel_url/#<id>`.
pub fn format_message(a: &Alert, panel_url: Option<&str>) -> String {
    let mut m = format!("<b>{}</b>", escape_html(&a.name));
    if let Some(g) = a.group.as_deref().filter(|g| !g.is_empty()) {
        m.push_str(&format!(" · {}", escape_html(g)));
    }
    m.push_str(&format!(
        "\nPreço {} · lucro {} (médio) / {} (seguro)\nPrazo {}",
        brl(a.price_cents),
        brl(a.mid_profit_cents),
        brl(a.safe_profit_cents),
        eta(a.eta_hours)
    ));
    if let Some(s) = a.seal.as_deref().filter(|s| !s.is_empty()) {
        m.push_str(&format!("\n{}", escape_html(s)));
    }
    if let Some(u) = panel_url.map(str::trim).filter(|u| !u.is_empty()) {
        let href = format!("{}/#{}", u.trim_end_matches('/'), a.id);
        m.push_str(&format!(
            "\n<a href=\"{}\">abrir no painel</a>",
            escape_html(&href)
        ));
    }
    m
}

/// Toast do Windows via WinRT; os textos chegam por variável de ambiente para não virarem código.
const TOAST: &str = r#"$ErrorActionPreference = 'Stop'
$e = [Security.SecurityElement]
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null
$d = New-Object Windows.Data.Xml.Dom.XmlDocument
$d.LoadXml("<toast activationType='protocol' launch='$($e::Escape($env:SS_URL))'><visual><binding template='ToastGeneric'><text>$($e::Escape($env:SS_TITLE))</text><text>$($e::Escape($env:SS_BODY))</text></binding></visual></toast>")
$n = [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe')
if ($n.Setting -ne 'Enabled') { throw "Avisos do Windows desativados: $($n.Setting)" }
$n.Show([Windows.UI.Notifications.ToastNotification]::new($d))"#;

/// Aviso no canto da tela do Windows, que sai mesmo com o painel fechado; clicar abre `url`.
pub async fn desktop(title: &str, body: &str, url: &str) -> Result<bool, String> {
    #[cfg(all(windows, not(test)))]
    {
        use std::process::Stdio;
        use tokio::process::Command;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command", TOAST])
            .env("SS_TITLE", title)
            .env("SS_BODY", body)
            .env("SS_URL", url)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .map_err(|_| "o Windows não confirmou o aviso em 10 s".to_string())?
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(true)
    }
    #[cfg(test)]
    {
        let _ = (title, body, url);
        Ok(true)
    }
    #[cfg(all(not(windows), not(test)))]
    {
        let _ = (title, body, url);
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::Path,
        routing::{get, post},
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn alert(id: &str) -> Alert {
        Alert {
            id: id.into(),
            name: "Glock-18 | Moonrise (Field-Tested)".into(),
            group: Some("T1 Center Star".into()),
            price_cents: 1439,
            mid_profit_cents: 312,
            safe_profit_cents: -5,
            eta_hours: Some(18.4),
            seal: Some("ainda não comprovado: 4 de 10 vendas fechadas".into()),
        }
    }

    #[test]
    fn message_format() {
        let m = format_message(&alert("527"), Some("http://192.168.0.10:8788/"));
        assert_eq!(
            m,
            "<b>Glock-18 | Moonrise (Field-Tested)</b> · T1 Center Star\nPreço R$ 14,39 · lucro R$ 3,12 (médio) / -R$ 0,05 (seguro)\nPrazo ~18 h\nainda não comprovado: 4 de 10 vendas fechadas\n<a href=\"http://192.168.0.10:8788/#527\">abrir no painel</a>"
        );
    }

    #[test]
    fn escapes_html_everywhere() {
        let mut a = alert("1");
        a.name = "<script>&\"x\"</script>".into();
        a.group = Some("A & B <i>".into());
        let m = format_message(&a, Some("http://x/?a=1&b=\"2\""));
        assert!(m.contains("<b>&lt;script&gt;&amp;&quot;x&quot;&lt;/script&gt;</b>"));
        assert!(m.contains("A &amp; B &lt;i&gt;"));
        assert!(m.contains("href=\"http://x/?a=1&amp;b=&quot;2&quot;/#1\""));
        assert!(!m.contains("<script>"));
    }

    #[test]
    fn optional_parts_and_eta() {
        let mut a = alert("9");
        a.group = None;
        a.eta_hours = None;
        a.seal = None;
        let m = format_message(&a, None);
        assert!(!m.contains("T1") && !m.contains("comprovado"));
        assert!(m.contains("prazo incerto") && !m.contains("href"));
        assert_eq!(eta(Some(72.0)), "~3 dias");
        assert_eq!(eta(Some(0.2)), "~1 h");
        assert_eq!(brl(5), "R$ 0,05");
    }

    #[test]
    fn replies_are_classified() {
        assert!(parse_reply(200, br#"{"ok":true,"result":{"username":"bot"}}"#).is_ok());
        assert!(
            matches!(parse_reply(401, br#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#), Err(NotifyError::Refused(d)) if d == "Unauthorized")
        );
        assert!(matches!(
            parse_reply(
                429,
                br#"{"ok":false,"error_code":429,"parameters":{"retry_after":5}}"#
            ),
            Err(NotifyError::RateLimited { .. })
        ));
        assert!(matches!(
            parse_reply(502, b"<html>"),
            Err(NotifyError::Parse(_))
        ));
    }

    #[test]
    fn debug_hides_token() {
        let t = Telegram::new().unwrap();
        t.set_credentials(Some("123:SEGREDO".into()), Some("1".into()));
        assert!(!format!("{t:?}").contains("SEGREDO"));
    }

    // Respostas montadas a partir da forma documentada da Bot API; nenhuma mensagem real é enviada.
    async fn serve(hits: Arc<AtomicUsize>, texts: Arc<Mutex<Vec<Value>>>) -> String {
        let app = Router::new()
            .route(
                "/{bot}/sendMessage",
                post(move |Path(bot): Path<String>, Json(b): Json<Value>| {
                    let (hits, texts) = (hits.clone(), texts.clone());
                    async move {
                        assert_eq!(bot, "bot123:TOK");
                        hits.fetch_add(1, Ordering::SeqCst);
                        texts.lock().unwrap().push(b);
                        Json(json!({"ok": true, "result": {"message_id": 1}}))
                    }
                }),
            )
            .route(
                "/{bot}/getMe",
                get(|Path(bot): Path<String>| async move {
                    if bot == "bot123:TOK" {
                        (axum::http::StatusCode::OK, Json(json!({"ok": true, "result": {"username": "sniper_bot"}})))
                    } else {
                        (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"})))
                    }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn one_alert_per_listing_with_gap_and_html_mode() {
        let (hits, texts) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![])));
        let base = serve(hits.clone(), texts.clone()).await;
        let t = Telegram::with_base(&base, Duration::from_millis(300)).unwrap();
        assert!(matches!(
            t.notify(&alert("1"), None).await,
            Err(NotifyError::NotConfigured)
        ));
        t.set_credentials(Some("123:TOK".into()), Some("-100".into()));
        let start = Instant::now();
        assert!(t.notify(&alert("1"), Some("http://p")).await.unwrap());
        assert!(!t.notify(&alert("1"), Some("http://p")).await.unwrap()); // repetido
        assert!(t.notify(&alert("2"), Some("http://p")).await.unwrap());
        assert!(
            start.elapsed() >= Duration::from_millis(300),
            "intervalo entre mensagens"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let b = &texts.lock().unwrap()[0];
        assert_eq!(
            (b["chat_id"].as_str(), b["parse_mode"].as_str()),
            (Some("-100"), Some("HTML"))
        );
        assert!(b["text"].as_str().unwrap().contains("http://p/#1"));
    }

    #[tokio::test]
    async fn get_me_validates_token() {
        let base = serve(Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(vec![]))).await;
        let t = Telegram::with_base(&base, Duration::ZERO).unwrap();
        assert!(matches!(t.get_me().await, Err(NotifyError::NotConfigured)));
        t.set_credentials(Some("123:TOK".into()), None);
        assert_eq!(
            t.get_me().await.unwrap().username.as_deref(),
            Some("sniper_bot")
        );
        t.set_credentials(Some("123:ERRADO".into()), None);
        assert!(matches!(t.get_me().await, Err(NotifyError::Refused(_))));
    }

    #[tokio::test]
    #[ignore = "ao vivo: envia UMA mensagem real; exige TELEGRAM_TOKEN, TELEGRAM_CHAT e SKIN_SNIPER_LIVE_TELEGRAM=SIM"]
    async fn live_get_me_and_send() {
        assert_eq!(
            std::env::var("SKIN_SNIPER_LIVE_TELEGRAM").as_deref(),
            Ok("SIM")
        );
        let t = Telegram::new().unwrap();
        t.set_credentials(
            std::env::var("TELEGRAM_TOKEN").ok(),
            std::env::var("TELEGRAM_CHAT").ok(),
        );
        t.get_me().await.unwrap();
        assert!(t.notify(&alert("teste"), None).await.unwrap());
    }
}
