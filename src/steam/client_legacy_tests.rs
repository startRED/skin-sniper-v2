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

#[tokio::test]
async fn public_exit_only_receives_book_and_public_legacy() {
    let alt_hits = Arc::new(AtomicUsize::new(0));
    let main_hits = Arc::new(AtomicUsize::new(0));
    let a = alt_hits.clone();
    let m = main_hits.clone();
    let app = Router::new().fallback(get(move |headers: AxHeaders| {
        let a = a.clone();
        let m = m.clone();
        async move {
            if headers.contains_key("x-test-alt") {
                a.fetch_add(1, Ordering::Relaxed);
                let cookie = headers.get("cookie").and_then(|v| v.to_str().ok());
                assert!(cookie.is_none() || cookie == Some("bMarketOptOut=1"));
            } else {
                m.fetch_add(1, Ordering::Relaxed);
            }
            "{}"
        }
    }));
    let base = serve(app).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
    c.alt_public_only = true;
    c.set_gap_for_test(Duration::from_millis(5));
    c.inner.lock().unwrap().alt_limiter.gap = Duration::from_millis(5);
    c.set_cookie(Some(Cookie {
        steam_login_secure: "secret-test".into(),
        sessionid: None,
    }));
    c.get_json(Endpoint::Listings, "/native", &[])
        .await
        .unwrap();
    c.get_json(Endpoint::History, "/native-history", &[])
        .await
        .unwrap();
    assert_eq!(alt_hits.load(Ordering::Relaxed), 0);
    {
        let mut g = c.inner.lock().unwrap();
        g.limiter.last_ok = None;
        g.limiter.on_429(Instant::now(), None);
    }
    c.get_json(Endpoint::Book, "/book", &[]).await.unwrap();
    assert!(c.stats().last_ok_at.is_none());
    assert!(c.stats().book_ok_at.is_some());
    assert!(c.stats().paused_until.is_some());
    tokio::time::sleep(Duration::from_millis(10)).await;
    c.legacy_json("/legacy", &[], false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    c.legacy_json("/history", &[], true).await.unwrap();
    assert_eq!(alt_hits.load(Ordering::Relaxed), 2);
    assert_eq!(main_hits.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn public_book_does_not_clear_native_pause_and_alt_429_falls_back() {
    let app = Router::new().fallback(get(|headers: AxHeaders| async move {
        if headers.contains_key("x-test-alt") {
            (AxStatus::TOO_MANY_REQUESTS, "{}")
        } else {
            (AxStatus::OK, "{}")
        }
    }));
    let base = serve(app).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
    c.alt_public_only = true;
    {
        let mut g = c.inner.lock().unwrap();
        g.limiter.on_429(Instant::now(), None);
    }
    assert!(c.prefer_public_listings());
    c.get_json(Endpoint::Book, "/book", &[]).await.unwrap();
    assert!(c.stats().paused_until.is_some());
    assert!(c.stats().last_ok_at.is_none());
    assert!(!c.prefer_public_listings());
    assert_eq!(c.take_calls()[1].limited, 1);
}

#[tokio::test]
async fn public_exit_network_failure_falls_back_to_main() {
    let base = serve(Router::new().fallback(get(|| async { "{}" }))).await;
    let c = fast(&base)
        .with_public_alt_proxy("http://127.0.0.1:9")
        .unwrap();
    c.set_gap_for_test(Duration::from_millis(5));
    c.get_json(Endpoint::Book, "/book", &[]).await.unwrap();
    assert!(c.inner.lock().unwrap().alt_down_until.is_some());
    assert_eq!(c.sent(), 2);
}

#[tokio::test]
async fn named_query_prefers_valid_public_page_and_rejects_invalid_one() {
    let bad = Arc::new(AtomicBool::new(false));
    let action_calls = Arc::new(AtomicUsize::new(0));
    let b = bad.clone();
    let actions = action_calls.clone();
    let app = Router::new()
        .route(
            "/market/listings/730/{name}/render/",
            get(move |headers: AxHeaders| {
                let invalid = b.load(Ordering::Relaxed);
                async move {
                    assert!(headers.contains_key("x-test-alt"));
                    if invalid {
                        "{}"
                    } else {
                        include_str!("../../testdata/steam_legacy_render_moonrise_2026-11.json")
                    }
                }
            }),
        )
        .route(
            "/market/actions",
            get(move |headers: AxHeaders| {
                actions.fetch_add(1, Ordering::Relaxed);
                async move {
                    assert!(!headers.contains_key("x-test-alt"));
                    include_str!("../../testdata/steam_listings_moonrise_sorted_price.json")
                }
            }),
        );
    let base = serve(app).await;
    let make_client = || {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-test-alt", HeaderValue::from_static("yes"));
        let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
        c.alt_public_only = true;
        c.set_gap_for_test(Duration::from_millis(5));
        c
    };
    let c = make_client();
    c.inner.lock().unwrap().limiter.next_at = Instant::now() + Duration::from_secs(5);
    let (page, named_only) = tokio::time::timeout(
        Duration::from_secs(2),
        crate::steam::listings::query_listings_named(&c, NAME, CODE, 0, Some(2)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(named_only && page.listings.len() == 3);
    assert_eq!(action_calls.load(Ordering::Relaxed), 0);
    bad.store(true, Ordering::Relaxed);
    let c = make_client();
    c.inner.lock().unwrap().limiter.next_at = Instant::now() + Duration::from_millis(50);
    let (page, named_only) =
        crate::steam::listings::query_listings_named(&c, NAME, CODE, 0, Some(2))
            .await
            .unwrap();
    assert!(!named_only && !page.listings.is_empty());
    assert_eq!(action_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn public_exit_respects_budget_and_global_send_spacing() {
    let mut c = fast("http://127.0.0.1:1");
    c.alt_public_only = true;
    c.set_gap_for_test(Duration::from_millis(50));
    c.inner.lock().unwrap().alt_limiter.gap = Duration::from_millis(5);
    c.pace_send(Endpoint::Book, true).await.unwrap();
    let start = Instant::now();
    c.pace_send(Endpoint::Listings, true).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(40));
    let (db, _dir) = crate::db::temp_db();
    c.set_research_db(db.clone());
    let before = c.sent();
    let decision = crate::research::Decision {
        source: "exploracao_externa".into(),
        policy: "test".into(),
        target: "x".into(),
        scale: 1.0,
        block_id: None,
    };
    crate::research::scope(db, decision, async {
        assert!(matches!(
            c.get_json(Endpoint::Book, "/book", &[]).await,
            Err(Error::BudgetExhausted)
        ));
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(c.sent(), before);
}
