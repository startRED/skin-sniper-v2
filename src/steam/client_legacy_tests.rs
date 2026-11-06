//! Respostas reais servidas localmente: recuperação sem gastar cota da Steam nem fazer compras.

use super::tests::{fast, serve};
use super::*;
use axum::http::{HeaderMap as AxHeaders, StatusCode as AxStatus};
use axum::{Router, routing::get};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const NAME: &str = "Glock-18 | Moonrise (Field-Tested)";
const CODE: &str = "G180420B6053004";

#[tokio::test]
async fn legacy_recovers_listings_and_history_without_leaking_login_or_resetting_actions() {
    let actions = Arc::new(AtomicUsize::new(0));
    let seen = actions.clone();
    let limited = Arc::new(AtomicBool::new(false));
    let fail = limited.clone();
    let app = Router::new()
        .route(
            "/market/actions",
            get(move || {
                seen.fetch_add(1, Ordering::Relaxed);
                async { AxStatus::TOO_MANY_REQUESTS }
            }),
        )
        .route(
            "/market/listings/730/{name}/render/",
            get(move |headers: AxHeaders| {
                let limited = fail.load(Ordering::Relaxed);
                async move {
                    let cookie = headers.get("cookie").unwrap().to_str().unwrap();
                    assert_eq!(
                        cookie, "bMarketOptOut=1",
                        "anúncio público não leva o login"
                    );
                    (
                        if limited {
                            AxStatus::TOO_MANY_REQUESTS
                        } else {
                            AxStatus::OK
                        },
                        include_str!("../../testdata/steam_legacy_render_moonrise_2026-11.json"),
                    )
                }
            }),
        )
        .route(
            "/market/pricehistory/",
            get(|headers: AxHeaders| async move {
                let cookie = headers.get("cookie").unwrap().to_str().unwrap();
                assert!(cookie.contains("steamLoginSecure=teste-local"));
                assert_eq!(
                    cookie
                        .split("; ")
                        .filter(|c| c.starts_with("bMarketOptOut="))
                        .collect::<Vec<_>>(),
                    ["bMarketOptOut=1"]
                );
                include_str!("../../testdata/steam_legacy_history_moonrise_2026-11.json")
            }),
        );
    let c = fast(&serve(app).await);
    assert!(
        c.action(
            Endpoint::Listings,
            "QueryListingsForItem",
            &serde_json::json!([])
        )
        .await
        .is_err()
    );
    assert!(c.stats().paused_until.is_some());
    c.set_cookie(Some(Cookie {
        steam_login_secure: "teste-local".into(),
        sessionid: None,
    }));
    c.keep_cookies(&["bMarketOptOut=0; Path=/".into()], Some("teste-local"));
    let (page, _) = crate::steam::listings::query_listings_named(&c, NAME, CODE, 0, Some(2))
        .await
        .unwrap();
    assert_eq!(page.listings.len(), 3);
    assert!(c.stats().last_ok_at.is_some());
    assert!(
        c.stats().paused_until.is_none(),
        "a caça lê pela interface antiga"
    );
    assert!(
        c.blocked(Endpoint::Listings),
        "a pausa real das actions continua"
    );
    let history = crate::steam::history::fetch_history(&c, NAME)
        .await
        .unwrap();
    assert_eq!(history.len(), 6);
    assert_eq!(
        actions.load(Ordering::Relaxed),
        1,
        "não insiste em actions durante a pausa"
    );

    limited.store(true, Ordering::Relaxed);
    assert!(matches!(
        crate::steam::listings::query_listings_named(&c, NAME, CODE, 0, Some(2)).await,
        Err(Error::RateLimited { .. })
    ));
    assert!(
        c.stats().paused_until.is_some(),
        "429 na alternativa também é respeitado"
    );
    let before = c.sent();
    assert!(
        crate::steam::listings::query_listings_named(&c, NAME, CODE, 0, Some(2))
            .await
            .is_err()
    );
    assert_eq!(
        c.sent(),
        before,
        "sem chamadas quando as duas leituras estão em pausa"
    );
}

#[tokio::test]
async fn read_limits_do_not_hold_session_or_user_actions() {
    let app = Router::new()
        .route("/busy", get(|| async { AxStatus::TOO_MANY_REQUESTS }))
        .route("/ok", get(|| async { "{}" }));
    let c = fast(&serve(app).await);
    assert!(c.get_json(Endpoint::Listings, "/busy", &[]).await.is_err());
    assert!(c.get_json(Endpoint::Legacy, "/busy", &[]).await.is_err());
    for ep in [Endpoint::Session, Endpoint::Buy, Endpoint::Auth] {
        tokio::time::timeout(Duration::from_secs(2), c.get_json(ep, "/ok", &[]))
            .await
            .unwrap()
            .unwrap();
    }
    assert!(c.blocked(Endpoint::Listings));
    assert!(c.blocked(Endpoint::Legacy));
    assert!(
        c.stats().last_ok_at.is_none(),
        "sessão saudável não significa anúncios recuperados"
    );
}

#[tokio::test]
async fn oversized_response_is_rejected_before_json_parsing() {
    let app = Router::new().route("/big", get(|| async { "x".repeat(MAX_BODY + 1) }));
    let c = fast(&serve(app).await);
    assert!(
        matches!(c.get_json(Endpoint::Legacy, "/big", &[]).await, Err(Error::Parse(message)) if message.contains("16 MiB"))
    );
}
