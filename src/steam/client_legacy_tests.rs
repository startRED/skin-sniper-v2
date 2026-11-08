//! Respostas reais servidas localmente: recuperação sem gastar cota da Steam nem fazer compras.

use super::tests::{fast, serve};
use super::*;
use axum::http::{HeaderMap as AxHeaders, StatusCode as AxStatus};
use axum::{Router, routing::get};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const NAME: &str = "Glock-18 | Moonrise (Field-Tested)";
const CODE: &str = "G180420B6053004";

#[tokio::test]
async fn paused_or_broken_native_prefers_public_ads_but_respects_public_failures() {
    for pause in [false, true] {
        let c = fast("http://127.0.0.1:9");
        {
            let mut g = c.inner.lock().unwrap();
            if pause {
                g.limiter.on_429(Instant::now(), None);
            } else {
                g.breakers[Endpoint::Listings.idx()].open_until =
                    Some(Instant::now() + Duration::from_secs(60));
            }
        }
        assert!(c.prefer_public_listings(), "não depende de saída extra");
        {
            c.inner.lock().unwrap().side.on_429(Instant::now(), None);
        }
        assert!(!c.prefer_public_listings(), "respeita a pausa pública");
        {
            let mut g = c.inner.lock().unwrap();
            g.side.paused_until = None;
            g.breakers[Endpoint::Legacy.idx()].open_until =
                Some(Instant::now() + Duration::from_secs(60));
        }
        assert!(!c.prefer_public_listings(), "respeita falhas públicas");
    }
}

#[tokio::test]
async fn slow_native_listings_use_ready_main_legacy_when_public_exit_is_down() {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let seen = hits.clone();
    let base = serve(Router::new().fallback(get(
        move |uri: axum::http::Uri, headers: AxHeaders| {
            seen.lock()
                .unwrap()
                .push((uri.path().to_owned(), Instant::now()));
            async move {
                assert_eq!(headers.get("cookie").unwrap(), "bMarketOptOut=1");
                include_str!("../../testdata/steam_legacy_render_moonrise_2026-11.json")
            }
        },
    )))
    .await;
    let c = fast(&base).with_public_alt_for_test(
        http_builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap(),
    );
    c.set_cookie(Some(Cookie {
        steam_login_secure: "must-not-leak".into(),
        sessionid: None,
    }));
    let started = Instant::now();
    {
        let mut g = c.inner.lock().unwrap();
        g.limiter.next_at = started + Duration::from_secs(5);
        g.next_any = started + Duration::from_millis(50);
        g.alt_down_until = Some(started + Duration::from_secs(60));
    }
    let (page, named_only) = tokio::time::timeout(
        Duration::from_millis(500),
        crate::steam::listings::query_listings_named(&c, NAME, CODE, 0, Some(2)),
    )
    .await
    .expect("a leitura comum não deve esperar a fila nativa")
    .unwrap();
    assert!(named_only);
    assert_eq!(page.listings.len(), 3);
    {
        let hits = hits.lock().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].0.ends_with("/render/"));
        assert!(hits[0].1.duration_since(started) >= Duration::from_millis(50));
    }
    assert_eq!(c.take_calls()[1].ok, 0, "a extra continua indisponível");
    assert!(!c.blocked(Endpoint::Listings), "não houve 429 nativo");
    {
        let mut g = c.inner.lock().unwrap();
        g.side.on_429(Instant::now(), None);
    }
    assert!(!c.prefer_public_listings(), "respeita a pausa do render");
    {
        let mut g = c.inner.lock().unwrap();
        g.side.paused_until = None;
        g.breakers[Endpoint::Legacy.idx()].open_until =
            Some(Instant::now() + Duration::from_secs(60));
    }
    assert!(!c.prefer_public_listings(), "respeita falhas do render");
}

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
    assert!(
        c.prefer_public_listings(),
        "o render da principal segue disponível"
    );
    c.inner.lock().unwrap().side.on_429(Instant::now(), None);
    assert!(!c.prefer_public_listings(), "as duas saídas estão em pausa");
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

#[tokio::test]
async fn public_alt_only_legacy_sends_fixture_without_private_cookies() {
    let main_hits = Arc::new(AtomicUsize::new(0));
    let seen_main = main_hits.clone();
    let app = Router::new().route(
        "/public",
        get(move |headers: AxHeaders| {
            let seen_main = seen_main.clone();
            async move {
                if headers.contains_key("x-test-alt") {
                    assert_eq!(
                        headers.get("cookie").and_then(|v| v.to_str().ok()),
                        Some("bMarketOptOut=1")
                    );
                    include_str!("../../testdata/steam_legacy_render_moonrise_2026-11.json")
                } else {
                    seen_main.fetch_add(1, Ordering::Relaxed);
                    "{}"
                }
            }
        }),
    );
    let base = serve(app).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
    c.alt_public_only = true;
    c.set_cookie(Some(Cookie {
        steam_login_secure: "must-not-leak".into(),
        sessionid: Some("private-session".into()),
    }));

    assert!(c.public_alt_available());
    let value = c
        .legacy_json_public_alt("/public", &[])
        .await
        .expect("alt slot available")
        .unwrap();
    assert_eq!(value["success"], true);
    assert_eq!(main_hits.load(Ordering::Relaxed), 0);
    assert_eq!(c.sent(), 1);
    assert_eq!(c.take_calls()[1].ok, 1);
    assert!(c.public_alt_available(), "the next slot can be reserved");
}

#[tokio::test]
async fn public_legacy_reserves_the_next_slot_ahead_of_books_and_rechecks_pause() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hits = seen.clone();
    let app = Router::new().fallback(get(move |uri: axum::http::Uri, headers: AxHeaders| {
        let hits = hits.clone();
        async move {
            hits.lock().unwrap().push((
                uri.path().to_string(),
                headers.contains_key("x-test-alt"),
                Instant::now(),
            ));
            "{}"
        }
    }));
    let base = serve(app).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
    c.alt_public_only = true;
    c.set_gap_for_test(Duration::from_millis(5));
    let gap = Duration::from_millis(100);
    c.inner.lock().unwrap().alt_limiter.gap = gap;
    c.get_json(Endpoint::Book, "/first-book", &[])
        .await
        .unwrap();

    let (page, ()) = tokio::join!(c.legacy_json_public_alt("/page", &[]), async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        for _ in 0..3 {
            c.get_json(Endpoint::Book, "/next-book", &[]).await.unwrap();
        }
    });
    page.expect("o livro não pode roubar a reserva").unwrap();
    {
        let hits = seen.lock().unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!(hits[0].0, "/first-book");
        assert!(hits[0].1);
        let page = hits.iter().find(|h| h.0 == "/page").unwrap();
        assert!(page.1, "o anúncio usa somente a extra");
        assert!(page.2.duration_since(hits[0].2) >= gap - Duration::from_millis(10));
        assert!(hits.iter().filter(|h| h.0 == "/next-book").all(|h| !h.1));
    }

    let before = c.sent();
    let counted = c.stats().queries_per_min;
    let (page, ()) = tokio::join!(c.legacy_json_public_alt("/paused-page", &[]), async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        c.inner
            .lock()
            .unwrap()
            .alt_limiter
            .on_429(Instant::now(), None);
    });
    assert!(
        page.is_none(),
        "429 durante a espera cancela o envio reservado"
    );
    assert_eq!(c.sent(), before);
    assert_eq!(
        c.stats().queries_per_min,
        counted,
        "reserva cancelada não é consulta"
    );
    assert_eq!(seen.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn authenticated_history_pause_does_not_hold_the_public_hunt_loop() {
    let base = serve(Router::new().fallback(get(|headers: AxHeaders| async move {
        assert!(headers.contains_key("x-test-alt"));
        include_str!("../../testdata/steam_legacy_render_moonrise_2026-11.json")
    })))
    .await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let c = fast(&base)
        .with_public_alt_for_test(http_builder().default_headers(headers).build().unwrap());
    c.set_cookie(Some(Cookie {
        steam_login_secure: "local-test".into(),
        sessionid: None,
    }));
    {
        let mut s = c.inner.lock().unwrap();
        s.limiter.on_429(Instant::now(), None);
        s.side.on_429(Instant::now(), None);
    }
    assert!(c.main_blocked(Endpoint::Legacy));
    assert!(
        !c.blocked(Endpoint::Legacy),
        "anúncios públicos ainda podem usar a extra"
    );
    let (db, _dir) = crate::db::temp_db();
    db.call(|c| {
        c.execute("INSERT INTO items(name,updated_at) VALUES (?1,0)", [NAME])?;
        Ok(())
    })
    .await
    .unwrap();
    let (events, _) = tokio::sync::broadcast::channel(64);
    let h = crate::hunt::Hunt::new(crate::hunt::HuntDeps {
        db: db.clone(),
        settings: crate::settings::SettingsStore::new(db.clone()),
        steam: Arc::new(c),
        csfloat: Arc::new(crate::csfloat::CsfloatClient::with_base(&base).unwrap()),
        events,
        telegram: None,
    });
    h.set_saldo(3608, crate::web::api::SaldoSource::Manual);
    {
        let mut s = h.lock();
        s.queue.set_tasks(vec![crate::hunt::scheduler::Task {
            id: format!("d:{NAME}"),
            name: NAME.into(),
            code: CODE.into(),
            wear: Some(2),
            seeds: vec![],
            float: None,
            score: 1.0,
        }]);
        for _ in 0..4 {
            s.queue.next(crate::db::now_unix());
        }
    }
    let tick = tokio::time::timeout(Duration::from_secs(2), crate::hunt::scheduler::tick(&h))
        .await
        .unwrap();
    assert_eq!(
        tick,
        crate::hunt::scheduler::Tick::Watched(format!("d:{NAME}"))
    );
    let discovery: i64 = db
        .call(|c| {
            c.query_row(
                "SELECT count(*) FROM research_decisions WHERE source='descoberta'",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(
        discovery, 0,
        "uma saída pública livre não libera o histórico autenticado"
    );
}

#[tokio::test]
async fn public_alt_only_legacy_stops_on_429_and_pause() {
    let alt_hits = Arc::new(AtomicUsize::new(0));
    let main_hits = Arc::new(AtomicUsize::new(0));
    let a = alt_hits.clone();
    let m = main_hits.clone();
    let app = Router::new().route(
        "/public",
        get(move |headers: AxHeaders| {
            let (a, m) = (a.clone(), m.clone());
            async move {
                if headers.contains_key("x-test-alt") {
                    a.fetch_add(1, Ordering::Relaxed);
                    AxStatus::TOO_MANY_REQUESTS
                } else {
                    m.fetch_add(1, Ordering::Relaxed);
                    AxStatus::OK
                }
            }
        }),
    );
    let base = serve(app).await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
    c.alt_public_only = true;

    assert!(c.public_alt_available());
    assert!(c.legacy_json_public_alt("/public", &[]).await.is_none());
    let sent = c.sent();
    assert_eq!(sent, 1);
    assert!(!c.public_alt_available());
    assert!(c.legacy_json_public_alt("/public", &[]).await.is_none());
    assert_eq!(c.sent(), sent);
    assert_eq!(alt_hits.load(Ordering::Relaxed), 1);
    assert_eq!(main_hits.load(Ordering::Relaxed), 0);
    assert_eq!(c.take_calls()[1].limited, 1);
    assert!(
        c.inner
            .lock()
            .unwrap()
            .alt_limiter
            .pause_end(Instant::now())
            .is_some()
    );
}

#[tokio::test]
async fn public_alt_only_legacy_offline_and_budget_never_use_main() {
    let main_hits = Arc::new(AtomicUsize::new(0));
    let seen_main = main_hits.clone();
    let app = Router::new().route(
        "/public",
        get(move || {
            let seen_main = seen_main.clone();
            async move {
                seen_main.fetch_add(1, Ordering::Relaxed);
                "{}"
            }
        }),
    );
    let base = serve(app).await;
    let c = fast(&base)
        .with_public_alt_proxy("http://127.0.0.1:9")
        .unwrap();
    assert!(c.public_alt_available());
    assert!(c.legacy_json_public_alt("/public", &[]).await.is_none());
    assert_eq!(c.sent(), 1);
    assert_eq!(main_hits.load(Ordering::Relaxed), 0);
    assert!(c.inner.lock().unwrap().alt_down_until.is_some());

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-test-alt", HeaderValue::from_static("yes"));
    let mut c = fast(&base).with_alt(http_builder().default_headers(headers).build().unwrap());
    c.alt_public_only = true;
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
            c.legacy_json_public_alt("/public", &[]).await,
            Some(Err(Error::BudgetExhausted))
        ));
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(c.sent(), before);
    assert_eq!(main_hits.load(Ordering::Relaxed), 0);
}
