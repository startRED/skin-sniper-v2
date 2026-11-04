//! web/guard — anti DNS rebinding (Host) e anti CSRF (Content-Type JSON em mutações).
//! Sem CORS de propósito: outra origem não passa do preflight. Sem login (decisão do Erick).

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::IpAddr;

/// Nome do PC: Windows tem `COMPUTERNAME`; Linux (CI) `HOSTNAME` ou `/etc/hostname`.
pub fn pc_name() -> Option<String> {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .ok()?;
    let name = name.trim().to_ascii_lowercase();
    (!name.is_empty()).then_some(name)
}

/// `host` é o cabeçalho Host cru (com porta, se houver); `pc` já em minúsculas.
pub fn host_ok(host: &str, pc: Option<&str>) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let name = match host.strip_prefix('[') {
        // [::1]:8788
        Some(rest) => match rest.split_once(']') {
            Some((ip, "")) => ip,
            Some((ip, port)) if port.starts_with(':') => ip,
            _ => return false,
        },
        None if host.parse::<IpAddr>().is_ok() => host.as_str(), // IPv6 sem colchetes
        None => host.rsplit_once(':').map_or(host.as_str(), |(h, _)| h),
    };
    name.parse::<IpAddr>().is_ok()
        || name == "localhost"
        || name.strip_suffix(".ts.net").is_some_and(|n| !n.is_empty())
        || pc == Some(name)
}

fn json_content(h: &HeaderMap) -> bool {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
}

pub async fn check(State(pc): State<Option<String>>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    if !host.is_some_and(|h| host_ok(h, pc.as_deref())) {
        return (StatusCode::FORBIDDEN, "Host não permitido").into_response();
    }
    let reads = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !reads && !json_content(req.headers()) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "use Content-Type: application/json",
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_rules() {
        let pc = Some("meu-pc");
        for ok in [
            "127.0.0.1:8788",
            "192.168.0.10",
            "[::1]:8788",
            "::1",
            "localhost:8788",
            "LOCALHOST",
            "pc.tail1234.ts.net",
            "pc.tail1234.ts.net:8788",
            "MEU-PC:8788",
        ] {
            assert!(host_ok(ok, pc), "{ok}");
        }
        for bad in [
            "evil.com",
            "evil.com:8788",
            "127.0.0.1.evil.com",
            "ts.net.evil.com",
            ".ts.net",
            "evilts.net",
            "meu-pc.evil.com",
            "[::1]x",
            "",
        ] {
            assert!(!host_ok(bad, pc), "{bad}");
        }
    }

    #[test]
    fn content_type_rules() {
        let mk = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::CONTENT_TYPE, v.parse().unwrap());
            h
        };
        assert!(json_content(&mk("application/json")));
        assert!(json_content(&mk("Application/JSON; charset=utf-8")));
        assert!(!json_content(&mk("text/plain")));
        assert!(!json_content(&mk("application/x-www-form-urlencoded")));
        assert!(!json_content(&HeaderMap::new()));
    }
}
