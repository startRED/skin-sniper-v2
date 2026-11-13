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
async fn manual_sales_and_calibration_use_saved_predictions() {
    let s = start().await;
    let mut p = opp_json("42");
    p["status"] = json!("bought");
    p["bought_at"] = json!(1);
    p["tradable_at"] = json!(2);
    p["message"] = Value::Null;
    // O placar compara com os 96 mostrados; o ajuste relativo continua separado.
    p["sell"]["mid"]["price_cents"] = json!(96);
    p["sell"]["correction"] = json!(0.8);
    let p = p.to_string();
    s.app
        .db
        .call(move |c| {
            c.execute("INSERT INTO purchases VALUES ('42', ?1, 1)", [&p])?;
            let hour = crate::db::now_unix().div_euclid(3600);
            c.execute(
                "INSERT INTO hourly_activity(hour_utc, listings, opportunities) VALUES (?1, 3, 1)",
                [hour],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    for (id, amount, status) in [
        ("missing", 130, StatusCode::NOT_FOUND),
        ("42", 0, StatusCode::BAD_REQUEST),
        (
            "42",
            crate::pricing::sell::MAX_PRICE_CENTS + 1,
            StatusCode::BAD_REQUEST,
        ),
        ("42", i64::MAX, StatusCode::BAD_REQUEST),
        ("42", 1, StatusCode::OK),
        ("42", crate::pricing::sell::MAX_PRICE_CENTS, StatusCode::OK),
        ("42", 132, StatusCode::OK),
        ("42", 144, StatusCode::OK),
    ] {
        let r = s
            .send(
                "POST".parse().unwrap(),
                &format!("/api/purchases/{id}/sold"),
                json!({"sold_cents": amount}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), status);
    }
    let purchases: Value = s
        .get("/api/purchases")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(purchases[0]["sold_cents"], 144);
    let result: Value = s
        .get("/api/calibration")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["families"][0]["family"], "AK-47 | Redline");
    assert_eq!(result["families"][0]["kind"], "desconto");
    assert_eq!(result["families"][0]["n"], 1);
    assert_eq!(result["families"][0]["mean_error_pct"], 50.0);
    assert_eq!(result["families"][0]["bias_pct"], 50.0);
    assert_eq!(
        result["hours"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["listings"].as_i64().unwrap())
            .sum::<i64>(),
        3
    );
    let count: i64 = s
        .app
        .db
        .call(|c| c.query_row("SELECT count(*) FROM outcomes", [], |r| r.get(0)))
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn sale_requires_confirmed_unlocked_purchase() {
    let s = start().await;
    let mut p = opp_json("42");
    p["status"] = json!("bought");
    p["bought_at"] = json!(1);
    p["tradable_at"] = json!(crate::db::now_unix() + 1000);
    p["message"] = Value::Null;
    let p = p.to_string();
    s.app
        .db
        .call(move |c| c.execute("INSERT INTO purchases VALUES ('42', ?1, 1)", [&p]))
        .await
        .unwrap();
    let r = s
        .send(
            "POST".parse().unwrap(),
            "/api/purchases/42/sold",
            json!({"sold_cents": 100}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn buying_on_steam_is_recorded_from_the_opportunity_and_can_be_undone() {
    let s = start().await;
    let o = opp_json("7").to_string();
    s.app
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO opportunities(id, json, created_at, gone_at) VALUES ('7', ?1, 1, 1000)",
                [&o],
            )
        })
        .await
        .unwrap();
    let post = |path: &str| s.send("POST".parse().unwrap(), path, json!({})).send();
    assert_eq!(
        post("/api/opportunities/nada/bought")
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let p: Value = post("/api/opportunities/7/bought")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // comprada quando o anúncio sumiu (1000); destrava 7 dias depois, na hora cheia seguinte
    assert_eq!(
        (&p["status"], &p["bought_at"], &p["tradable_at"], &p["seed"]),
        (&json!("bought"), &json!(1000), &json!(608_400), &json!(7))
    );
    // a oportunidade comprada sobrevive à limpeza: o gráfico e o valor de hoje saem dela
    let kept: i64 = s
        .app
        .db
        .call(|c| {
            crate::db::retain(c, crate::db::now_unix())?;
            c.query_row(
                "SELECT count(*) FROM opportunities WHERE id = '7'",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(kept, 1);
    let list = || async {
        let r = s.get("/api/purchases").send().await.unwrap();
        r.json::<Vec<Value>>().await.unwrap().len()
    };
    assert_eq!(list().await, 1);
    let undo = || {
        s.send("DELETE".parse().unwrap(), "/api/purchases/7", json!({}))
            .send()
    };
    assert_eq!(undo().await.unwrap().status(), StatusCode::OK);
    assert_eq!(undo().await.unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(list().await, 0);
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
    s.app.settings.set_saldo(Some(1234)).await.unwrap();
    s.app
        .db
        .call(|c| {
            c.execute(
                "INSERT INTO purchases(id,json,created_at) VALUES ('existing','{}',1)",
                [],
            )
            .map(|_| ())
        })
        .await
        .unwrap();
    let r = s
        .send("POST".parse().unwrap(), "/api/buy", json!({"id": "1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::GONE);
    let v: Value = r.json().await.unwrap();
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("diretamente na Steam")
    );

    assert_eq!(s.app.settings.saldo().await.unwrap(), Some(1234));
    let count: i64 = s
        .app
        .db
        .call(|c| c.query_row("SELECT count(*) FROM purchases", [], |r| r.get(0)))
        .await
        .unwrap();
    assert_eq!(count, 1);

    let (o, lost, old) = (
        opp_json("42").to_string(),
        opp_json("44").to_string(),
        opp_json("43").to_string(),
    );
    let gone = crate::db::now_unix() - 60;
    s.app
        .db
        .call(move |c| {
            c.execute("INSERT INTO opportunities(id, json, created_at, gone_at) VALUES ('42', ?1, 1, NULL)", [&o])?;
            c.execute("INSERT INTO opportunities(id, json, created_at, gone_at) VALUES ('44', ?1, 1, ?2)", (&lost, gone))?;
            c.execute("INSERT INTO opportunities(id, json, created_at, gone_at) VALUES ('43', ?1, 1, 5)", [&old]) // perdida há muito
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
        list.iter()
            .map(|o| (o.id.as_str(), o.gone_at))
            .collect::<Vec<_>>(),
        [("42", None), ("44", Some(gone))]
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
async fn every_opportunity_stays_visible_for_24_hours_after_it_leaves() {
    let s = start().await;
    let now = crate::db::now_unix();
    s.app
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            for i in 0..65 {
                let mut opp = opp_json(&i.to_string());
                opp["gone_reason"] = json!("rejected");
                tx.execute(
                    "INSERT INTO opportunities(id,json,created_at,gone_at) VALUES (?1,?2,?3,?4)",
                    rusqlite::params![i.to_string(), opp.to_string(), now - 90000, now - 60 - i],
                )?;
            }
            for (id, gone) in [
                ("boundary", Some(now - 86400)),
                ("older", Some(now - 86401)),
                ("active", None),
            ] {
                tx.execute(
                    "INSERT INTO opportunities(id,json,created_at,gone_at) VALUES (?1,?2,?3,?4)",
                    rusqlite::params![id, opp_json(id).to_string(), now - 90000, gone],
                )?;
            }
            tx.commit()
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
        list.len(),
        66,
        "nenhuma das 65 perdidas do dia pode ser escondida por um limite de 50"
    );
    assert!(list.iter().all(|o| o.id != "boundary" && o.id != "older"));
    assert!(
        list.iter()
            .filter(|o| o.gone_at.is_some())
            .all(|o| o.gone_reason == Some(super::api::GoneReason::Rejected))
    );
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
                if tx
                    .send(Event::Gone {
                        id: "42".into(),
                        at: 123,
                        reason: super::api::GoneReason::Unavailable,
                    })
                    .unwrap_or(0)
                    > 0
                {
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
        text.contains("event: gone")
            && text.contains(r#""id":"42""#)
            && text.contains(r#""reason":"unavailable""#),
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
        let _ = s.app.events.send(Event::Gone {
            id: i.to_string(),
            at: 123,
            reason: super::api::GoneReason::Unavailable,
        });
    }
    let end = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while r.chunk().await.ok().flatten().is_some() {}
    })
    .await;
    assert!(end.is_ok(), "stream deveria encerrar após Lagged");
}

#[tokio::test]
async fn account_routes_are_disabled() {
    let s = start().await;
    assert_eq!(
        s.get("/api/steam/login/status")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
    assert_eq!(
        s.send("POST".parse().unwrap(), "/api/steam/login/qr", json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
    assert_eq!(
        s.send(
            "PUT".parse().unwrap(),
            "/api/settings",
            json!({"steam_login_secure":"new"})
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::BAD_REQUEST
    );
}
