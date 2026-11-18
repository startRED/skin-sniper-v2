//! Anúncios lidos por um Chrome de verdade, sem login: o `fetch` sai do próprio navegador (TLS, HTTP/2,
//! cookies `browserid`/`sessionid` da visita), como quem recarrega a página do item na mão.
//! 11/11: com o `/market/actions` em 429 para o reqwest e para o curl, o mercado beta abria no navegador.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const HOME: &str = "https://steamcommunity.com/market/";
// Intervalo mínimo entre dois envios pelo mesmo Chrome, o `MIN_SPACING` do cliente. O cliente dá a vez antes de o
// pedido entrar aqui; se a trava do Chrome segura a fila (abertura, resposta lenta), ela não pode sair colada.
const SPACING: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug)]
pub struct ResponseElapsed(pub Option<u64>);

/// Número da visita (abertura do Chrome) que atendeu a resposta.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Visit(pub u64);

pub struct Browser {
    exe: PathBuf,
    profile: PathBuf,
    port: u16,
    proxy: Option<String>,
    live: Mutex<Option<(tokio::process::Child, Ws)>>,
    next_id: AtomicU64,
    visit: AtomicU64,
    /// Visita recusada pela Steam, a fechar antes do próximo pedido; 0 = nenhuma.
    stale: AtomicU64,
    /// Antes desta hora o próximo pedido não sai: último envio (ou abertura) mais `SPACING`.
    hold: std::sync::Mutex<Option<Instant>>,
}

/// Chrome instalado no Windows, se houver.
pub fn find_chrome() -> Option<PathBuf> {
    let mut paths = vec![
        PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
    ];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        paths.push(Path::new(&local).join(r"Google\Chrome\Application\chrome.exe"));
    }
    paths.into_iter().find(|p| p.is_file())
}

impl Browser {
    /// `proxy` = `Some("http://127.0.0.1:1056")` para sair pelo celular. Cada instância tem perfil e porta próprios.
    pub fn new(exe: PathBuf, profile: PathBuf, port: u16, proxy: Option<&str>) -> Self {
        Self {
            exe,
            profile,
            port,
            proxy: proxy.map(str::to_string),
            live: Mutex::new(None),
            next_id: AtomicU64::new(1),
            visit: AtomicU64::new(0),
            stale: AtomicU64::new(0),
            hold: std::sync::Mutex::new(None),
        }
    }

    /// GET same-origin pela aba do mercado. Devolve status, corpo, tempo e visita. Erro = navegador caiu; o próximo pedido religa.
    pub async fn fetch(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<(u16, Vec<u8>, Option<u64>, Visit), String> {
        let mut live = self.live.lock().await;
        if live.is_some() && self.refused() {
            *live = None; // kill_on_drop derruba o Chrome
        }
        if live.is_none() {
            *live = Some(self.start().await?);
            self.visit.fetch_add(1, Ordering::Relaxed);
            self.left(Instant::now()); // a abertura carrega a página do mercado: conta como envio
        }
        if let Some(at) = self.turn(Instant::now()) {
            tokio::time::sleep_until(at.into()).await;
        }
        self.left(Instant::now()); // hora real: o sono pode acordar atrasado
        let visit = Visit(self.visit.load(Ordering::Relaxed));
        let (_, ws) = live.as_mut().unwrap();
        let r = self.eval(ws, &fetch_js(url, headers), 40).await;
        let out = r.and_then(|v| {
            let s = v.as_str().ok_or("fetch sem resposta")?;
            let v: Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
            let status = v["s"].as_u64().ok_or("fetch sem status")? as u16;
            Ok((
                status,
                v["b"].as_str().unwrap_or("").as_bytes().to_vec(),
                parse_elapsed(&v),
                visit,
            ))
        });
        if out.is_err() {
            *live = None; // kill_on_drop derruba o Chrome
        }
        out
    }

    /// O próximo pedido fecha esta visita e abre outra (cookie de sessão e conexões novos, mesmo perfil).
    /// Pedir de novo a mesma visita, ou uma que já fechou, não fecha a que está aberta nem apaga o pedido dela.
    /// Devolve se o pedido marcou agora a visita aberta (é o que vale registrar).
    pub fn reset(&self, visit: Visit) -> bool {
        let before = self.stale.fetch_max(visit.0, Ordering::Relaxed);
        before < visit.0 && visit.0 == self.visit.load(Ordering::Relaxed)
    }

    /// Hora de sair de quem chegou colado no envio anterior; `None` = sai já, no ritmo de quem chamou.
    fn turn(&self, now: Instant) -> Option<Instant> {
        self.hold.lock().unwrap().filter(|at| *at > now)
    }

    /// Saiu um pedido agora (ou o Chrome abriu): o seguinte espera o intervalo a partir daqui.
    fn left(&self, now: Instant) {
        *self.hold.lock().unwrap() = Some(now + SPACING);
    }

    /// A visita aberta é a que a Steam recusou.
    fn refused(&self) -> bool {
        self.stale.load(Ordering::Relaxed) == self.visit.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn stale(&self) -> Visit {
        Visit(self.stale.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    async fn probe(&self, expr: &str) -> Result<Value, String> {
        let mut live = self.live.lock().await;
        if live.is_none() {
            *live = Some(self.start().await?);
        }
        let (_, ws) = live.as_mut().unwrap();
        self.eval(ws, expr, 20).await
    }

    async fn start(&self) -> Result<(tokio::process::Child, Ws), String> {
        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.arg("--headless=new")
            .arg(format!("--remote-debugging-port={}", self.port))
            .arg(format!("--user-data-dir={}", self.profile.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-gpu",
                "--mute-audio",
                "--window-size=1920,1080",
            ])
            .arg("about:blank")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if let Some(p) = &self.proxy {
            cmd.arg(format!("--proxy-server={p}"));
        }
        let child = cmd.spawn().map_err(|e| format!("Chrome não abriu: {e}"))?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| e.to_string())?;
        let base = format!("http://127.0.0.1:{}", self.port);
        let mut version = None;
        for _ in 0..40 {
            if let Ok(r) = http.get(format!("{base}/json/version")).send().await
                && let Ok(v) = r.json::<Value>().await
            {
                version = Some(v);
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let version = version.ok_or("Chrome não abriu a porta de depuração")?;
        let pages: Value = http
            .get(format!("{base}/json/list"))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let ws_url = pages
            .as_array()
            .and_then(|a| a.iter().find(|p| p["type"] == "page"))
            .and_then(|p| p["webSocketDebuggerUrl"].as_str())
            .ok_or("Chrome sem aba")?
            .to_string();
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| e.to_string())?;
        // O modo sem janela se anuncia como "HeadlessChrome" e, com o UA trocado, manda as marcas
        // (`sec-ch-ua`) vazias; as duas coisas vêm aqui como o Chrome instalado mandaria.
        self.call(&mut ws, "Network.enable", json!({}), 10).await?;
        self.call(
            &mut ws,
            "Network.setUserAgentOverride",
            user_agent_override(&version),
            10,
        )
        .await?;
        // Sem janela a tela é 800×600, tamanho que monitor de verdade não tem.
        self.call(
            &mut ws,
            "Emulation.setDeviceMetricsOverride",
            json!({"width": 1920, "height": 937, "deviceScaleFactor": 1, "mobile": false,
                   "screenWidth": 1920, "screenHeight": 1080}),
            10,
        )
        .await?;
        self.call(&mut ws, "Page.navigate", json!({"url": HOME}), 30)
            .await?;
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let ready = self
                .eval(&mut ws, "location.href.startsWith('https://steamcommunity.com') && document.readyState === 'complete'", 10)
                .await?;
            if ready == Value::Bool(true) {
                tracing::info!(
                    saida = self.proxy.as_deref().unwrap_or("principal"),
                    "Chrome do mercado aberto"
                );
                return Ok((child, ws));
            }
        }
        Err("mercado não carregou no Chrome".into())
    }

    async fn eval(&self, ws: &mut Ws, expr: &str, secs: u64) -> Result<Value, String> {
        let r = self
            .call(
                ws,
                "Runtime.evaluate",
                json!({"expression": expr, "awaitPromise": true, "returnByValue": true}),
                secs,
            )
            .await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(format!(
                "Chrome: {}",
                e["exception"]["description"]
                    .as_str()
                    .unwrap_or("erro no script")
            ));
        }
        Ok(r["result"]["value"].clone())
    }

    async fn call(
        &self,
        ws: &mut Ws,
        method: &str,
        params: Value,
        secs: u64,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        ws.send(Message::text(msg))
            .await
            .map_err(|e| e.to_string())?;
        let wait = async {
            while let Some(m) = ws.next().await {
                let m = m.map_err(|e| e.to_string())?;
                let Ok(text) = m.to_text() else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text) else {
                    continue;
                };
                if v["id"].as_u64() == Some(id) {
                    if let Some(e) = v.get("error") {
                        return Err(format!("{method}: {e}"));
                    }
                    return Ok(v["result"].clone());
                }
            }
            Err("Chrome fechou a conexão".to_string())
        };
        tokio::time::timeout(Duration::from_secs(secs), wait)
            .await
            .map_err(|_| format!("{method}: Chrome não respondeu em {secs} s"))?
    }
}

/// UA e Client Hints do Chrome de Windows, a partir do `/json/version` (`Browser: HeadlessChrome/157.0.x.y`).
fn user_agent_override(version: &Value) -> Value {
    let ua = version["User-Agent"]
        .as_str()
        .unwrap_or("")
        .replace("HeadlessChrome", "Chrome");
    let full = version["Browser"]
        .as_str()
        .and_then(|b| b.split('/').nth(1))
        .unwrap_or("0.0.0.0")
        .to_string();
    let major = full.split('.').next().unwrap_or("0").to_string();
    let brands = |v: &str| {
        json!([
            {"brand": "Google Chrome", "version": v},
            {"brand": "Chromium", "version": v},
            {"brand": "Not.A/Brand", "version": if v.contains('.') { "99.0.0.0" } else { "99" }},
        ])
    };
    json!({
        "userAgent": ua,
        "acceptLanguage": "pt-BR,pt,en-US,en",
        "platform": "Win32",
        "userAgentMetadata": {
            "brands": brands(&major),
            "fullVersionList": brands(&full),
            "fullVersion": full,
            "platform": "Windows",
            "platformVersion": "19.0.0",
            "architecture": "x86",
            "model": "",
            "mobile": false,
            "bitness": "64",
            "wow64": false,
        },
    })
}

fn parse_elapsed(v: &Value) -> Option<u64> {
    v.get("elapsed_ms").and_then(Value::as_u64)
}

/// Script do `fetch`; URL e cabeçalhos entram como JSON, nunca colados no código.
fn fetch_js(url: &str, headers: &[(String, String)]) -> String {
    let h: serde_json::Map<String, Value> = headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    format!(
        "(async()=>{{const started=performance.now();const r=await fetch({},{{headers:{},credentials:'include'}});\
         const body=await r.text();const elapsed_ms=Math.floor(performance.now()-started);return JSON.stringify({{s:r.status,b:body,elapsed_ms}})}})()",
        Value::String(url.to_string()),
        Value::Object(h)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_queued_on_the_browser_leave_spaced() {
        let b = Browser::new("chrome".into(), "perfil".into(), 9, None);
        let open = Instant::now();
        assert_eq!(b.turn(open), None);
        b.left(open); // abertura
        assert_eq!(b.turn(open), Some(open + SPACING));
        // O primeiro acordou atrasado e a resposta dele demorou 5 s, com dois na fila.
        let first = open + SPACING + Duration::from_millis(2900);
        b.left(first);
        let second = first + Duration::from_secs(5);
        assert_eq!(b.turn(second), None); // já passou o intervalo desde o envio do primeiro
        b.left(second);
        // O terceiro pega a trava 300 ms depois e conta do envio real do segundo.
        assert_eq!(
            b.turn(second + Duration::from_millis(300)),
            Some(second + SPACING)
        );
        b.left(second + SPACING);
        assert_eq!(b.turn(second + 2 * SPACING), None); // no ritmo do cliente ninguém espera
    }

    #[test]
    fn fetch_timing_is_internal_and_optional() {
        let js = fetch_js("http://localhost", &[]);
        assert!(js.find("performance.now()").unwrap() < js.find("await fetch").unwrap());
        assert!(js.find("await r.text()").unwrap() < js.find("performance.now()-started").unwrap());
        assert_eq!(parse_elapsed(&json!({"elapsed_ms": 42})), Some(42));
        for v in [
            json!({}),
            json!({"elapsed_ms": -1}),
            json!({"elapsed_ms": "bad"}),
        ] {
            assert!(parse_elapsed(&v).is_none());
        }
    }

    #[test]
    fn only_the_refused_visit_is_closed() {
        let b = Browser::new("chrome".into(), "perfil".into(), 9, None);
        b.visit.store(4, Ordering::Relaxed);
        assert!(!b.refused());
        assert!(!b.reset(Visit(3))); // recusa atrasada de uma visita que já fechou
        assert!(!b.refused());
        assert!(b.reset(Visit(4)));
        assert!(!b.reset(Visit(4))); // segunda recusa da mesma visita
        assert!(!b.reset(Visit(3)));
        assert!(b.refused());
        b.visit.store(5, Ordering::Relaxed); // o pedido seguinte abriu outra
        assert!(!b.refused());
    }

    #[test]
    fn fetch_js_escapes_url_and_headers() {
        let js = fetch_js("https://x/a?q=\"';", &[("accept".into(), "a\"b".into())]);
        assert!(js.contains(r#"fetch("https://x/a?q=\"';""#));
        assert!(js.contains(r#"{"accept":"a\"b"}"#));
    }

    /// Ao vivo: `cargo test chrome_reads_listings -- --ignored`. Uma consulta à Steam.
    #[tokio::test]
    #[ignore]
    async fn chrome_reads_listings() {
        let exe = find_chrome().expect("Chrome instalado");
        let dir = std::env::temp_dir().join("skin-sniper-chrome-teste");
        let b = Browser::new(exe, dir, 9339, None);
        let qp = r#"[{"appid":730,"strItemName":"G180420B6053004","filters":{},"accessoryFilters":{},"propertyFilters":{},"sort":{"field":0,"direction":0},"start":0}]"#;
        let url = format!(
            "https://steamcommunity.com/market/actions?q=QueryListingsForItem&qp={}",
            urlencode(qp)
        );
        let (s, body, ..) = b
            .fetch(
                &url,
                &[("x-valve-request-type".into(), "queryAction".into())],
            )
            .await
            .unwrap();
        println!("status={s} bytes={}", body.len());
        assert!(s == 200 || s == 429, "status {s}");
    }

    /// Ao vivo, fora da Steam: os cabeçalhos que o Chrome manda não podem dizer "Headless".
    #[tokio::test]
    #[ignore]
    async fn chrome_headers_look_real() {
        let exe = find_chrome().expect("Chrome instalado");
        let dir = std::env::temp_dir().join("skin-sniper-chrome-teste-ua");
        let b = Browser::new(exe, dir, 9338, None);
        let v = b
            .probe("(async()=>JSON.stringify({ua:navigator.userAgent,wd:navigator.webdriver,\
                    brands:navigator.userAgentData&&navigator.userAgentData.brands,\
                    full:navigator.userAgentData&&await navigator.userAgentData.getHighEntropyValues(['fullVersionList','platform']),\
                    plugins:navigator.plugins.length,langs:navigator.languages,\
                    screen:[screen.width,screen.height]}))()")
            .await
            .unwrap();
        let body = v.as_str().unwrap().to_string();
        println!("{body}");
        assert!(!body.contains("Headless"), "{body}");
        assert!(!body.contains("\"wd\":true"), "{body}");
    }

    fn urlencode(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
}
