//! Testes locais do saldo e da leitura de histórico.
use super::*;
use crate::csfloat::CsfloatClient;
use crate::db::temp_db;
use crate::hunt::HuntDeps;
use crate::web::api::Event;
use std::time::Duration;

fn runtime(
    base: &str,
) -> (
    Arc<Runtime>,
    broadcast::Receiver<Event>,
    crate::db::tempfile_dir::Dir,
) {
    let (db, dir) = temp_db();
    let (events, rx) = broadcast::channel(64);
    let settings = SettingsStore::new(db.clone());
    let steam = Arc::new(SteamClient::with_base(base).unwrap());
    steam.set_gap_for_test(Duration::from_millis(1));
    let csfloat = Arc::new(CsfloatClient::with_base(base).unwrap());
    let telegram = Arc::new(Telegram::with_base(base, Duration::ZERO).unwrap());
    let hunt = Hunt::new(HuntDeps {
        db: db.clone(),
        steam: steam.clone(),
        csfloat: csfloat.clone(),
        events: events.clone(),
        settings: settings.clone(),
        telegram: Some(telegram.clone()),
    });
    (
        Runtime::new(db, settings, events, hunt, steam, csfloat, telegram),
        rx,
        dir,
    )
}

#[tokio::test]
async fn saldo_is_manual_and_cached_steam_wallet_is_ignored() {
    let (rt, _rx, _dir) = runtime("http://127.0.0.1:1");
    rt.settings
        .set_raw("steam_wallet_cache", Some("old,3608,7,9999999999".into()))
        .await
        .unwrap();
    rt.apply_local().await.unwrap();
    assert_eq!(
        (rt.hunt.state().saldo_cents, rt.hunt.state().saldo_source),
        (0, SaldoSource::None)
    );
    rt.settings.set_saldo(Some(1500)).await.unwrap();
    rt.apply_local().await.unwrap();
    assert_eq!(
        (rt.hunt.state().saldo_cents, rt.hunt.state().saldo_source),
        (1500, SaldoSource::Manual)
    );
}

#[tokio::test]
async fn telegram_is_verified_and_history_reads_saved_points() {
    use axum::{Router, routing::get};
    let app = Router::new().route(
        "/bot{token}/getMe",
        get(|| async { r#"{"ok":true,"result":{"username":"fakebot"}}"# }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let (rt, _rx, _dir) = runtime(&base);

    let i = crate::web::api::SettingsInput {
        telegram_token: Some(Some("123:abc".into())),
        telegram_chat_id: Some(Some("42".into())),
        ..Default::default()
    };
    rt.settings.apply(i).await.unwrap();
    rt.apply_local().await.unwrap();
    rt.verify_telegram().await;
    let k = rt.hunt.state().keys;
    assert_eq!(
        (k.telegram.ok, k.telegram.detail.as_deref()),
        (Some(true), Some("@fakebot"))
    );

    const ID: &str = "6624491228774232473";
    const NAME: &str = "Glock-18 | Moonrise (Field-Tested)";
    let tier = |p: i64| {
        serde_json::json!({"price_cents": p, "net_cents": p * 85 / 100,
               "profit_cents": p * 85 / 100 - 400, "eta_hours": null})
    };
    let opp = serde_json::json!({
        "id": ID, "name": NAME, "icon_url": "https://i/x.png",
        "steam_url": "https://s/x", "inspect_url": null, "wear": "Field-Tested", "float": 0.2,
        "seed": 151, "group": null, "kind": "desconto", "price_cents": 400,
        "sell": {"high": tier(591), "mid": tier(578), "quick": tier(561)},
        "margin_pct": 25.75, "safe": true, "confidence": "alta", "evidence": "x", "seen_at": 1
    })
    .to_string();
    rt.db
        .call(move |c| {
            c.execute(
                "INSERT INTO opportunities(id, json, created_at) VALUES (?1, ?2, 1)",
                params![ID, opp],
            )?;
            c.execute(
                "INSERT INTO price_points(name, ts, median_cents, purchases) VALUES (?1, ?2, 590, 12)",
                params![NAME, now_unix() - 3600],
            )
            .map(|_| ())
        })
        .await
        .unwrap();
    let h = rt.history(ID).await.ok().unwrap();
    assert_eq!(h.points.len(), 1);
    assert_eq!((h.points[0].1, h.points[0].2), (590, 12));
    assert_eq!(h.sell.quick.price_cents, 561);
    assert_eq!(
        rt.history("nope").await.err().unwrap().0,
        StatusCode::NOT_FOUND
    );
}
