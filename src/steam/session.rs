//! Valida o cookie `steamLoginSecure`: abre a página do mercado logado e lê `sessionid`, saldo e conta.
//!
//! Formas lidas (variáveis JS inline da página do mercado):
//! `g_sessionID = "…"`, `g_steamID = "7656…"` (ou `false` sem login), `g_bLoggedIn = true|false` e
//! `g_rgWalletInfo = {"wallet_balance":"3608","wallet_currency":7,…}` (saldo em centavos, moeda da carteira).
//! As três primeiras foram vistas numa captura anônima (testdata); `g_rgWalletInfo` e o nome da conta
//! só aparecem logado e **não foram capturados** (sem cookie disponível): seguem a forma documentada.
#![allow(dead_code)]

use serde_json::Value;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use super::client::Cookie;
use super::client::{CURRENCY_BRL, Endpoint, Error, Result, SteamClient};

#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub sessionid: String,
    /// Saldo da carteira em centavos da moeda da carteira.
    pub wallet_balance_cents: i64,
    pub wallet_currency: i32,
    pub account_name: Option<String>,
}

/// GET `/market/` com o cookie. Sem cookie, ou com a página anônima (cookie expirado), devolve
/// `SessionExpired`; carteira que não é BRL devolve `WrongCurrency`. Em caso de sucesso grava o
/// `sessionid` da página no cookie do cliente (a compra exige o mesmo valor no cookie e no formulário).
pub async fn validate(c: &SteamClient) -> Result<SessionInfo> {
    let Some(mut cookie) = c.cookie() else {
        return Err(Error::SessionExpired);
    };
    // O navegador sempre manda `sessionid` junto; sem ele, manda um nosso (a Steam aceita qualquer valor
    // de 24 hex, desde que cookie e formulário combinem) e depois adota o da página.
    if cookie.sessionid.is_none() {
        let id = random_sessionid();
        if c.adopt_sessionid(&cookie, &id) {
            cookie.sessionid = Some(id);
        }
    }
    let (status, location, html) = c.get_page_auth(Endpoint::Session, "/market/").await?;
    if (300..400).contains(&status) || status == 401 {
        // sem segredo: só o status e o destino (host + caminho) do redirecionamento
        tracing::warn!(status, redirect = %redirect_target(location.as_deref()), shape = %cookie_shape(&cookie.steam_login_secure), "sessão recusada pela Steam");
        return Err(Error::SessionExpired);
    }
    if status != 200 {
        return Err(Error::Http {
            endpoint: Endpoint::Session,
            status,
        });
    }
    let info = parse_market_page(&html).inspect_err(|e| {
        if matches!(e, Error::SessionExpired) {
            tracing::warn!(
                status,
                shape = %cookie_shape(&cookie.steam_login_secure),
                "sessão recusada pela Steam: página do mercado veio anônima"
            );
        }
    })?;
    if cookie.sessionid.as_deref() != Some(info.sessionid.as_str()) {
        c.adopt_sessionid(&cookie, &info.sessionid);
    }
    Ok(info)
}

/// 24 caracteres hex, como o `sessionid` do navegador. Não é segredo de verdade (só casa cookie e formulário).
fn random_sessionid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    format!("{:016x}{:08x}", h.finish(), std::process::id())
}

/// Forma do cookie para o log, sem o valor: separador, tamanho e horas até o `exp` do JWT.
fn cookie_shape(v: &str) -> String {
    let sep = if v.contains("%7C%7C") {
        "%7C%7C"
    } else if v.contains("||") {
        "||"
    } else {
        "nenhum"
    };
    let exp = super::auth::cookie_exp(v).map_or("ilegível".to_string(), |e| {
        format!("{}h", (e - super::auth::now_unix()) / 3600)
    });
    format!("separador={sep} tamanho={} exp_em={exp}", v.len())
}

/// Host e caminho do `Location`, sem query nem fragmento (podem trazer dados da sessão).
fn redirect_target(location: Option<&str>) -> String {
    let Some(l) = location else {
        return "(sem Location)".into();
    };
    l.split(['?', '#']).next().unwrap_or_default().to_string()
}

pub fn parse_market_page(html: &str) -> Result<SessionInfo> {
    let steam_id = js_value(html, "g_steamID").map(|v| v.trim_matches('"'));
    let logged = js_value(html, "g_bLoggedIn") == Some("true")
        || steam_id.is_some_and(|s| s.len() > 5 && s.bytes().all(|b| b.is_ascii_digit()));
    if !logged {
        return Err(Error::SessionExpired);
    }
    let sessionid = js_value(html, "g_sessionID")
        .map(|v| v.trim_matches('"'))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Parse("g_sessionID ausente".into()))?
        .to_string();
    let wallet: Value = js_value(html, "g_rgWalletInfo")
        .and_then(|j| serde_json::from_str(j).ok())
        .ok_or_else(|| Error::Parse("g_rgWalletInfo ausente".into()))?;
    let currency = wallet
        .get("wallet_currency")
        .and_then(as_i64)
        .ok_or_else(|| Error::Parse("wallet_currency ausente".into()))?;
    let balance = wallet
        .get("wallet_balance")
        .and_then(as_i64)
        .filter(|b| *b >= 0)
        .ok_or_else(|| Error::Parse("wallet_balance ausente".into()))?;
    if currency != i64::from(CURRENCY_BRL) {
        return Err(Error::WrongCurrency {
            got: currency as i32,
        });
    }
    Ok(SessionInfo {
        sessionid,
        wallet_balance_cents: balance,
        wallet_currency: currency as i32,
        account_name: account_name(html),
    })
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Valor da atribuição `nome = valor;` num script inline. Objeto `{…}` vai até a chave que fecha
/// (respeitando strings); o resto vai até `;` ou fim de linha.
fn js_value<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = html[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = html[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let Some(rest) = html[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        if !before_ok {
            continue;
        }
        let rest = rest.trim_start();
        if rest.starts_with('{') {
            let (mut depth, mut in_str, mut esc) = (0i32, false, false);
            for (k, ch) in rest.char_indices() {
                match (in_str, ch) {
                    (true, _) if esc => esc = false,
                    (true, '\\') => esc = true,
                    (true, '"') => in_str = false,
                    (true, _) => {}
                    (false, '"') => in_str = true,
                    (false, '{') => depth += 1,
                    (false, '}') => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&rest[..=k]);
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        let end = rest.find([';', '\n', '\r']).unwrap_or(rest.len());
        return Some(rest[..end].trim());
    }
    None
}

/// Nome do cabeçalho do site: `id="account_pulldown"` ou `persona_name_text_content`.
fn account_name(html: &str) -> Option<String> {
    for marker in ["id=\"account_pulldown\"", "persona_name_text_content"] {
        if let Some(i) = html.find(marker)
            && let Some(gt) = html[i..].find('>')
        {
            let tail = &html[i + gt + 1..];
            let text = tail[..tail.find('<').unwrap_or(tail.len())].trim();
            if !text.is_empty() {
                return Some(unescape(text));
            }
        }
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FORMA DOCUMENTADA, NÃO CAPTURA: não havia cookie da Steam disponível. As variáveis seguem o que a
    /// página anônima mostra (g_sessionID, g_steamID, g_bLoggedIn) mais `g_rgWalletInfo` como descrito
    /// na pesquisa; o markup do nome da conta é a forma clássica do cabeçalho. Valores inventados.
    fn logged_page(currency: i32) -> String {
        format!(
            r#"<html><body><a class="global_action_link persona_name_text_content" href="x">  Jo&amp;ão Teste </a>
<script>
g_bLoggedIn = true;
g_steamID = "76561198000000000";
g_sessionID = "0123456789abcdef01234567";
g_nWalletCurrency = {currency};
g_rgWalletInfo = {{"wallet_fee":1,"wallet_balance":"3608","wallet_currency":{currency},"wallet_country":"BR","success":1,"note":"}} chave solta"}};
</script></body></html>"#
        )
    }

    #[test]
    fn documented_logged_page() {
        let i = parse_market_page(&logged_page(7)).unwrap();
        assert_eq!(
            i,
            SessionInfo {
                sessionid: "0123456789abcdef01234567".into(),
                wallet_balance_cents: 3608,
                wallet_currency: 7,
                account_name: Some("Jo&ão Teste".into()),
            }
        );
    }

    #[test]
    fn redirect_target_drops_query() {
        assert_eq!(
            redirect_target(Some(
                "https://steamcommunity.com/login/home/?goto=%2Fmarket%2F&x=1"
            )),
            "https://steamcommunity.com/login/home/"
        );
        assert_eq!(redirect_target(None), "(sem Location)");
    }

    #[test]
    fn wrong_wallet_currency() {
        assert!(matches!(
            parse_market_page(&logged_page(1)),
            Err(Error::WrongCurrency { got: 1 })
        ));
    }

    #[test]
    fn real_anonymous_page_means_expired() {
        // trecho literal de uma captura anônima real (g_steamID = false, g_bLoggedIn = false)
        let html = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/steam_market_page_anonymous_excerpt.html"
        ))
        .unwrap();
        assert!(matches!(
            parse_market_page(&html),
            Err(Error::SessionExpired)
        ));
    }

    #[test]
    fn logged_without_wallet_is_parse_error_not_zero_balance() {
        let html = r#"<script>g_bLoggedIn = true; g_sessionID = "abc";</script>"#;
        assert!(matches!(parse_market_page(html), Err(Error::Parse(_))));
    }

    #[test]
    fn js_value_ignores_lookalike_names() {
        let html = r#"x_g_sessionID = "no"; g_sessionID = "yes"; g_sessionID_old = "no";"#;
        assert_eq!(js_value(html, "g_sessionID"), Some("\"yes\""));
    }

    #[tokio::test]
    async fn no_cookie_is_expired_without_network() {
        let c = SteamClient::with_base("http://127.0.0.1:1").unwrap();
        assert!(matches!(validate(&c).await, Err(Error::SessionExpired)));
        assert!(c.stats().last_query_at.is_none());
    }

    /// JWT de mentira no formato do navegador (cabeçalho.corpo.assinatura em base64url, `aud` web:community),
    /// guardado URL-encoded (`%7C%7C`) como o Chrome mostra. O cabeçalho Cookie tem de sair idêntico:
    /// nem codificar de novo (`%257C`) nem decodificar (`||`).
    #[tokio::test]
    async fn cookie_header_is_sent_verbatim_with_sessionid() {
        use axum::{Router, http::HeaderMap, response::Html, routing::get};
        let value = "76561198000000001%7C%7CeyAidHlwIjogIkpXVCIsICJhbGciOiAiRWREU0EiIH0.eyJpc3MiOiJyOjAwMTIiLCJzdWIiOiI3NjU2MTE5ODAwMDAwMDAwMSIsImF1ZCI6WyJ3ZWI6Y29tbXVuaXR5Il0sImV4cCI6MjAwMDAwMDAwMH0.ZmFsc2EtYXNzaW5hdHVyYS1fLTAxMjM";
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let app = Router::new().route(
            "/market/",
            get(move |h: HeaderMap| {
                let seen = seen2.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(h["cookie"].to_str().unwrap().to_string());
                    Html(logged_page(7))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let c = SteamClient::with_base(&format!("http://{addr}")).unwrap();
        c.set_cookie(Some(Cookie {
            steam_login_secure: value.into(),
            sessionid: None,
        }));
        validate(&c).await.unwrap();
        c.set_gap_for_test(std::time::Duration::ZERO);
        validate(&c).await.unwrap();
        let seen = seen.lock().unwrap();
        // 1º pedido: sessionid gerado por nós (24 hex); 2º: o que a página devolveu
        let (name, rest) = seen[0].split_once("; sessionid=").unwrap();
        assert_eq!(name, format!("steamLoginSecure={value}"));
        assert!(
            rest.len() == 24 && rest.bytes().all(|b| b.is_ascii_hexdigit()),
            "{rest}"
        );
        assert_eq!(
            seen[1],
            format!("steamLoginSecure={value}; sessionid=0123456789abcdef01234567")
        );
        assert!(
            cookie_shape(value).contains("separador=%7C%7C")
                && cookie_shape(value).contains("exp_em=")
        );
        assert!(!cookie_shape(value).contains("eyJ"));
    }

    #[tokio::test]
    async fn login_redirect_is_expired_and_cookie_is_sent() {
        use axum::{Router, http::HeaderMap, http::StatusCode, routing::get};
        let app = Router::new().route(
            "/market/",
            get(|h: HeaderMap| async move {
                let c = h.get("cookie").unwrap().to_str().unwrap();
                assert!(c.starts_with("steamLoginSecure=x; sessionid="), "{c}");
                (StatusCode::FOUND, [("location", "/login")])
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let c = SteamClient::with_base(&format!("http://{addr}")).unwrap();
        c.set_cookie(Some(Cookie {
            steam_login_secure: "x".into(),
            sessionid: None,
        }));
        assert!(matches!(validate(&c).await, Err(Error::SessionExpired)));
    }

    async fn serve_logged(app_cookie_swap: bool) -> (Arc<SteamClient>, Cookie) {
        use axum::{Router, response::Html, routing::get};
        let cell: Arc<std::sync::OnceLock<Arc<SteamClient>>> = Arc::new(std::sync::OnceLock::new());
        let cell2 = cell.clone();
        let app = Router::new().route(
            "/market/",
            get(move || {
                let cell = cell2.clone();
                async move {
                    if app_cookie_swap {
                        cell.get().unwrap().set_cookie(None); // o usuário apaga a sessão no meio da consulta
                    }
                    Html(logged_page(7))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let c = Arc::new(SteamClient::with_base(&format!("http://{addr}")).unwrap());
        cell.set(c.clone()).ok();
        let ck = Cookie {
            steam_login_secure: "x".into(),
            sessionid: None,
        };
        c.set_cookie(Some(ck.clone()));
        (c, ck)
    }

    #[tokio::test]
    async fn validate_adopts_sessionid_when_session_unchanged() {
        let (c, _) = serve_logged(false).await;
        validate(&c).await.unwrap();
        assert_eq!(
            c.cookie().unwrap().sessionid.as_deref(),
            Some("0123456789abcdef01234567")
        );
    }

    #[tokio::test]
    async fn validate_does_not_resurrect_a_cleared_session() {
        let (c, _) = serve_logged(true).await;
        validate(&c).await.unwrap();
        assert!(c.cookie().is_none());
    }

    #[tokio::test]
    #[ignore = "ao vivo: exige um cookie real em STEAM_LOGIN_SECURE; nunca foi rodado nesta etapa"]
    async fn live_validate() {
        let c = SteamClient::new().unwrap();
        let v = std::env::var("STEAM_LOGIN_SECURE").expect("defina STEAM_LOGIN_SECURE");
        c.set_cookie(Some(Cookie {
            steam_login_secure: v,
            sessionid: None,
        }));
        let info = validate(&c).await.unwrap();
        assert!(info.wallet_balance_cents >= 0);
    }
}
