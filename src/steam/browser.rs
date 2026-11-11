//! Anúncios lidos por um Chrome de verdade, sem login: o `fetch` sai do próprio navegador (TLS, HTTP/2,
//! cookies `browserid`/`sessionid` da visita), como quem recarrega a página do item na mão.
//! 11/11: com o `/market/actions` em 429 para o reqwest e para o curl, o mercado beta abria no navegador.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const HOME: &str = "https://steamcommunity.com/market/";

pub struct Browser {
    exe: PathBuf,
    profile: PathBuf,
    port: u16,
    proxy: Option<String>,
    live: Mutex<Option<(tokio::process::Child, Ws)>>,
    next_id: AtomicU64,
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
        }
    }

    /// GET same-origin pela aba do mercado. Devolve status e corpo. Erro = navegador caiu; o próximo pedido religa.
    pub async fn fetch(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<(u16, Vec<u8>), String> {
        let mut live = self.live.lock().await;
        if live.is_none() {
            *live = Some(self.start().await?);
        }
        let (_, ws) = live.as_mut().unwrap();
        let r = self.eval(ws, &fetch_js(url, headers), 40).await;
        let out = r.and_then(|v| {
            let s = v.as_str().ok_or("fetch sem resposta")?;
            let v: Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
            let status = v["s"].as_u64().ok_or("fetch sem status")? as u16;
            Ok((status, v["b"].as_str().unwrap_or("").as_bytes().to_vec()))
        });
        if out.is_err() {
            *live = None; // kill_on_drop derruba o Chrome
        }
        out
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
        // O modo sem janela se anuncia como "HeadlessChrome"; o resto do navegador é o Chrome instalado.
        let ua = version["User-Agent"]
            .as_str()
            .unwrap_or("")
            .replace("HeadlessChrome", "Chrome");
        self.call(&mut ws, "Network.enable", json!({}), 10).await?;
        self.call(
            &mut ws,
            "Network.setUserAgentOverride",
            json!({"userAgent": ua}),
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

/// Script do `fetch`; URL e cabeçalhos entram como JSON, nunca colados no código.
fn fetch_js(url: &str, headers: &[(String, String)]) -> String {
    let h: serde_json::Map<String, Value> = headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    format!(
        "(async()=>{{const r=await fetch({},{{headers:{},credentials:'include'}});\
         return JSON.stringify({{s:r.status,b:await r.text()}})}})()",
        Value::String(url.to_string()),
        Value::Object(h)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (s, body) = b
            .fetch(
                &url,
                &[("x-valve-request-type".into(), "queryAction".into())],
            )
            .await
            .unwrap();
        println!("status={s} bytes={}", body.len());
        assert!(s == 200 || s == 429, "status {s}");
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
