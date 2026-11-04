//! Servidor de verdade numa porta efêmera + reqwest: cobre guarda, rotas e SSE de ponta a ponta.

use super::api::{Event, Opportunity, State};
use super::{AppState, router};
use crate::db::temp_db;
use crate::settings::SettingsStore;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

struct Server {
    base: String,
    app: AppState,
    http: Client,
    _dir: crate::db::tempfile_dir::Dir,
}

async fn start() -> Server {
    let (db, dir) = temp_db();
    let mut app = AppState::new(db.clone(), SettingsStore::new(db));
    app.pc_name = Some("meu-pc".into());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, router(app.clone())).into_future());
    Server {
        base,
        app,
        http: Client::new(),
        _dir: dir,
    }
}

impl Server {
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{path}", self.base))
    }
    fn send(&self, m: reqwest::Method, path: &str, body: Value) -> reqwest::RequestBuilder {
        self.http
            .request(m, format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .body(body.to_string())
    }
}

fn opp_json(id: &str) -> Value {
    let tier = |p: i64| json!({"price_cents": p, "net_cents": p - 10, "profit_cents": 5, "eta_hours": null});
    json!({
        "id": id, "name": "AK-47 | Redline (Field-Tested)", "icon_url": "i", "steam_url": "s", "inspect_url": null,
        "wear": "Field-Tested", "float": 0.2, "seed": 7, "group": null, "kind": "desconto",
        "price_cents": 100, "sell": {"high": tier(130), "mid": tier(120), "quick": tier(110)},
        "margin_pct": 5.0, "safe": true, "confidence": "alta", "evidence": "x", "seen_at": 1
    })
}

#[tokio::test]
async fn host_and_content_type_guard() {
    let s = start().await;
    let r = s
        .get("/api/state")
        .header("host", "evil.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = s
        .get("/api/state")
        .header("host", "meu-pc:8788")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.headers().get("access-control-allow-origin").is_none());

    let r = s
        .http
        .post(format!("{}/api/buy", s.base))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let r = s
        .http
        .post(format!("{}/api/buy", s.base))
        .header("content-type", "text/plain")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn state_saldo_and_settings_follow_contract() {
    let s = start().await;
    let st: State = s
        .get("/api/state")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st.saldo_cents, 0);
    let v: Value = s
        .get("/api/state")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["saldo_source"], "none");
    assert_eq!(
        v["keys"]["steam"],
        json!({"set": false, "ok": null, "detail": null})
    );

    let v: Value = s
        .send("PUT".parse().unwrap(), "/api/saldo", json!({"cents": 3608}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (v["saldo_cents"].as_i64(), v["saldo_source"].as_str()),
        (Some(3608), Some("manual"))
    );
    let v: Value = s
        .send("PUT".parse().unwrap(), "/api/saldo", json!({"cents": null}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["saldo_source"], "none");
    let r = s
        .send("PUT".parse().unwrap(), "/api/saldo", json!({"cents": -1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);

    let v: Value = s
        .send(
            "PUT".parse().unwrap(),
            "/api/settings",
            json!({"telegram_token": "123456:ABCDEFgLO9", "sound": false}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["telegram_token"], "••••gLO9");
    assert_eq!(v["sound"], false);
    assert_eq!(v["notify_min_profit_cents"], 100);
    let v: Value = s
        .get("/api/settings")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["telegram_token"], "••••gLO9");
    let v: Value = s
        .get("/api/state")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["keys"]["telegram"]["set"], true);
    let v: Value = s
        .send(
            "PUT".parse().unwrap(),
            "/api/settings",
            json!({"telegram_token": null}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["telegram_token"], Value::Null);
    let r = s
        .send("PUT".parse().unwrap(), "/api/settings", json!({"nope": 1}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_client_error());
}

#[tokio::test]
async fn unwired_routes_say_so_and_db_routes_read_db() {
    let s = start().await;
    let r = s
        .send("POST".parse().unwrap(), "/api/buy", json!({"id": "1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = r.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("ainda não ligado"));

    let o = opp_json("42").to_string();
    s.app
        .db
        .call(move |c| {
            c.execute("INSERT INTO opportunities(id, json, created_at, gone_at) VALUES ('42', ?1, 1, NULL)", [&o])?;
            c.execute("INSERT INTO opportunities(id, json, created_at, gone_at) VALUES ('43', ?1, 1, 5)", [&o]) // encerrada
        })
        .await
        .unwrap();
    let list: Vec<Opportunity> = s
        .get("/api/opportunities")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        list.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["42"]
    );
    assert_eq!(
        s.get("/api/opportunities/42/history")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        s.get("/api/opportunities/99/history")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE // sem Runtime nada é ligado; o 404 real é testado em runtime::tests
    );
    let p: Vec<Value> = s
        .get("/api/purchases")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(p.is_empty());
    assert_eq!(
        s.get("/api/nada").send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn static_fallback_serves_html() {
    let s = start().await;
    for path in ["/", "/qualquer/rota"] {
        let r = s.get(path).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.text().await.unwrap().to_lowercase().contains("<"));
    }
}

#[tokio::test]
async fn sse_delivers_published_event() {
    let s = start().await;
    let mut r = s.get("/api/events").send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    // Só publica depois de assinar: o `send` retorna quando os cabeçalhos chegam, mas a assinatura
    // acontece no handler; repete até alguém receber.
    let publisher = {
        let tx = s.app.events.clone();
        tokio::spawn(async move {
            loop {
                if tx.send(Event::Gone { id: "42".into() }).unwrap_or(0) > 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
    };
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), r.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(
        text.contains("event: gone") && text.contains(r#"{"id":"42"}"#),
        "{text}"
    );
    publisher.await.unwrap();
}

#[tokio::test]
async fn sse_closes_when_client_lags() {
    let s = start().await;
    let mut r = s.get("/api/events").send().await.unwrap();
    // Espera o handler assinar, depois estoura a capacidade (256) sem ninguém ler.
    while s.app.events.receiver_count() == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    for i in 0..1000 {
        let _ = s.app.events.send(Event::Gone { id: i.to_string() });
    }
    let end = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while r.chunk().await.ok().flatten().is_some() {}
    })
    .await;
    assert!(end.is_ok(), "stream deveria encerrar após Lagged");
}
