//! Teste de ponta a ponta: um servidor HTTP local (axum) serve as capturas reais no lugar da Steam e do
//! CSFloat, e a caça roda do banco vazio até o evento `opportunity`. É teste; não entra no produto.
//!
//! As capturas reais são usadas como estão, com estas alterações feitas no teste (a captura de 03/11 não
//! tem anúncio com lucro): o primeiro anúncio da página de listagens passa a custar R$ 4,00, o segundo
//! recebe seed 59 (tier 1 do Moonrise) e custa R$ 7,00, e a primeira entrada da busca vira o Moonrise FT
//! a US$ 1,00. No CSFloat, três vendas sem adesivo ganham seeds do tier 1 e preço ×1,5, e quatro vendas com float
//! de elite do FT (0,155–0,173; duas já eram reais e saíram a ~1,0×) passam a ×2.

use super::detect::points_for;
use super::scheduler::{Task, Tick, tick};
use super::*;
use crate::db::temp_db;
use crate::pricing::sell;
use crate::steam::listings::Listing;
use crate::web::api::{Confidence, Kind};
use axum::extract::{Path, Query, State as AxState};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const NOW: i64 = 1_793_754_060; // um minuto depois do último ponto da captura de histórico
const MOON: &str = "Glock-18 | Moonrise (Field-Tested)";
const ST_MOON: &str = "StatTrak™ Glock-18 | Moonrise (Field-Tested)";
const ID_CHEAP: &str = "6624491228774232473";
const ID_T1: &str = "8709556594461768769";

#[tokio::test]
async fn history_success_does_not_restore_ads_or_claim_a_paused_book_is_running() {
    use crate::steam::client::{Cookie, Endpoint};
    let app = Router::new()
        .route(
            "/market/actions",
            get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
        )
        .route(
            "/market/pricehistory/",
            get(|| async {
                include_str!("../../testdata/steam_legacy_history_moonrise_2026-11.json")
            }),
        )
        .route(
            "/busy",
            get(|| async { axum::http::StatusCode::TOO_MANY_REQUESTS }),
        )
        .route(
            "/broken",
            get(|| async { axum::http::StatusCode::BAD_GATEWAY }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against(&base, db);
    h.steam.set_cookie(Some(Cookie {
        steam_login_secure: "test".into(),
        sessionid: None,
    }));
    assert!(
        !crate::steam::history::fetch_history(&h.steam, MOON)
            .await
            .unwrap()
            .is_empty()
    );
    let after_history = h.state().hunt;
    assert!(
        after_history.paused_until.is_some(),
        "histórico válido não recupera anúncios"
    );
    assert!(after_history.last_ok_at.is_none());
    for _ in 0..3 {
        assert!(
            h.steam
                .get_json(Endpoint::Book, "/broken", &[])
                .await
                .is_err()
        );
    }
    assert!(
        h.state().hunt.book_ok_at.is_none(),
        "via suspensa por falhas não aparece ativa"
    );
    assert!(
        h.steam
            .get_json(Endpoint::Legacy, "/busy", &[])
            .await
            .is_err()
    );
    let stopped = h.state().hunt;
    assert!(stopped.paused_until.is_some());
    assert!(
        stopped.book_ok_at.is_none(),
        "uma via pausada não aparece como caça ativa"
    );
    assert_eq!(
        stopped.message.as_deref(),
        Some("a Steam pediu calma (429); caça em pausa")
    );
}

#[test]
fn recorded_stattrak_moonrise_alerts_without_quick_profit() {
    // Registro original do achado perdido, sem alterar preço, seed, risco ou estimativas.
    let mut opp: Opportunity = serde_json::from_str(include_str!(
        "../../testdata/opportunity_stattrak_moonrise_t1_2026-11-05.json"
    ))
    .unwrap();
    assert_eq!((opp.price_cents, opp.seed), (844, Some(706)));
    assert_eq!(opp.sell.mid.profit_cents, 1075);
    assert_eq!(opp.ev_profit_cents, Some(639));
    assert!(!opp.safe && detect::should_notify(&opp, 100));
    assert!(!detect::should_notify(&opp, 1076));
    opp.gone_at = Some(NOW);
    assert!(!detect::should_notify(&opp, 100));
}

#[tokio::test]
async fn recheck_targets_the_active_seed_even_after_another_range_was_read() {
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against("http://127.0.0.1:1", db);
    let mut opp: Opportunity = serde_json::from_str(include_str!(
        "../../testdata/opportunity_stattrak_moonrise_t1_2026-11-05.json"
    ))
    .unwrap();
    opp.checked_at = Some(NOW - 61);
    let task = Task {
        id: "r:moon".into(),
        name: MOON.into(),
        code: "G180420B6053004".into(),
        wear: None,
        seeds: crate::pricing::groups::seed_ranges(MOON),
        float: None,
        score: 1.0,
    };
    let retry = {
        let mut s = h.lock();
        s.queue.set_tasks(vec![task.clone()]);
        s.queue.next(NOW);
        s.queue.reward(&task.id, NOW, scheduler::Found::default());
        s.active.insert(
            opp.id.clone(),
            Active {
                opp,
                task: task.id.clone(),
                attempted_at: 0,
            },
        );
        let overdue = scheduler::overdue_task(&s, NOW, false).unwrap();
        assert_eq!(overdue.seeds, vec![(706, 706)]);
        assert_eq!(s.queue.focus(overdue).seeds, vec![(706, 706)]);
        assert_eq!(
            s.queue.focus(task.clone()).seeds,
            vec![(58, 59)],
            "acompanhar não pula uma faixa da caça"
        );
        assert!(scheduler::overdue_task(&s, NOW, true).is_none());
        // Uma tentativa que não reencontrou o anúncio não pode bloquear outra seed atrasada.
        let original = s.active.values().next().unwrap().opp.clone();
        let mut other = original.clone();
        other.id = "other-seed".into();
        other.seed = Some(59);
        other.checked_at = Some(NOW - 60);
        s.active.insert(
            other.id.clone(),
            Active {
                opp: other,
                task: "r:moon".into(),
                attempted_at: 0,
            },
        );
        s.active.get_mut(&original.id).unwrap().attempted_at = NOW;
        assert_eq!(
            scheduler::overdue_task(&s, NOW, false).unwrap().seeds,
            vec![(58, 59)]
        );
        s.active.remove("other-seed");
        assert!(scheduler::overdue_task(&s, NOW, false).is_none());
        s.active.values_mut().next().unwrap().opp.checked_at = Some(NOW);
        assert!(scheduler::overdue_task(&s, NOW, false).is_none());
        let a = s.active.values_mut().next().unwrap();
        a.opp.checked_at = Some(NOW - 61);
        a.attempted_at = 0;
        scheduler::overdue_task(&s, NOW, false).unwrap()
    };
    assert!(scheduler::run_task(&h, &retry).await.is_err());
    let s = h.lock();
    assert_eq!(s.active.values().next().unwrap().attempted_at, NOW);
    assert!(
        scheduler::overdue_task(&s, NOW, false).is_none(),
        "falha também espera antes de tentar outra vez"
    );
}

#[tokio::test]
async fn notification_can_qualify_later_and_survives_restart_without_duplicates() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    h.settings
        .apply(serde_json::from_value(json!({"notify_min_profit_cents":100000})).unwrap())
        .await
        .unwrap();
    run_until_two(&h, &mut rx).await;
    let task = h
        .lock()
        .queue
        .tasks()
        .into_iter()
        .find(|t| t.id == format!("d:{MOON}"))
        .unwrap();
    let page = crate::steam::listings::parse_page(&listings_body()["data"], 0).unwrap();
    let pending: i64 = db.call(|c| c.query_row(
        "SELECT count(*) FROM opportunities WHERE notified_at IS NOT NULL OR notify_pending_at IS NOT NULL", [], |r| r.get(0)
    )).await.unwrap();
    assert_eq!(pending, 0);
    h.settings
        .apply(serde_json::from_value(json!({"notify_min_profit_cents":100})).unwrap())
        .await
        .unwrap();
    h.lock()
        .active
        .values_mut()
        .for_each(|a| a.opp.checked_at = Some(NOW - 61));
    let out = detect::process(&h, &task, &page).await.unwrap();
    assert_eq!(out.found.iter().filter(|(_, alert)| *alert).count(), 2);
    assert_eq!(
        out.rewarded.finds, 0,
        "avisar não repete crédito de aprendizado"
    );
    assert!(
        h.lock()
            .active
            .values()
            .all(|a| a.opp.checked_at == Some(NOW))
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let delivered: i64 = db.call(|c| c.query_row(
                "SELECT count(*) FROM opportunities WHERE notified_at IS NOT NULL AND notify_pending_at IS NULL", [], |r| r.get(0)
            )).await.unwrap();
            if delivered == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let (h2, _rx) = hunt_against(&base, db.clone());
    h2.set_saldo(3608, SaldoSource::Manual);
    h2.load().await.unwrap();
    let again = detect::process(&h2, &task, &page).await.unwrap();
    assert!(again.found.iter().all(|(_, alert)| !alert));
    assert_eq!(again.rewarded.finds, 0);
    db.call(|c| {
        c.execute(
            "UPDATE opportunities SET notified_at=NULL, notify_pending_at=?1 WHERE id=?2",
            rusqlite::params![NOW - 61, ID_T1],
        )
    })
    .await
    .unwrap();
    let recovered = detect::process(&h2, &task, &page).await.unwrap();
    assert_eq!(
        recovered
            .found
            .iter()
            .filter(|(_, alert)| *alert)
            .map(|(o, _)| o.id.as_str())
            .collect::<Vec<_>>(),
        vec![ID_T1]
    );
}

#[tokio::test]
async fn notification_starts_before_a_later_listing_finishes_loading_history() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    h.settings
        .apply(serde_json::from_value(json!({"notify_min_profit_cents":100000})).unwrap())
        .await
        .unwrap();
    run_until_two(&h, &mut rx).await;
    h.settings
        .apply(serde_json::from_value(json!({"notify_min_profit_cents":100})).unwrap())
        .await
        .unwrap();
    let task = Task {
        id: "r:early".into(),
        name: MOON.into(),
        code: "G180420B6053004".into(),
        wear: None,
        seeds: vec![(0, 1000)],
        float: None,
        score: 1.0,
    };
    let mut page = crate::steam::listings::parse_page(&listings_body()["data"], 0).unwrap();
    page.listings.truncate(2);
    page.listings[1].name = ST_MOON.into();
    let release = Arc::new(Notify::new());
    *bodies.history_wait.lock().unwrap() = Some(release.clone());
    let pending = {
        let h = h.clone();
        tokio::spawn(async move { detect::process(&h, &task, &page).await })
    };
    tokio::time::timeout(Duration::from_secs(2), bodies.history_started.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let delivered: bool = db
                .call(|c| {
                    c.query_row(
                        "SELECT notified_at IS NOT NULL FROM opportunities WHERE id=?1",
                        [ID_CHEAP],
                        |r| r.get(0),
                    )
                })
                .await
                .unwrap();
            if delivered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("o primeiro aviso não espera o histórico do segundo anúncio");
    assert!(!pending.is_finished());
    release.notify_one();
    pending.await.unwrap().unwrap();
}

fn clock() -> i64 {
    NOW
}

fn boot_tasks(task: Task) -> Vec<Task> {
    (0..scheduler::BOOT_TASKS)
        .map(|i| {
            let mut t = task.clone();
            if i > 0 {
                t.id = format!("duplicate:{i}:{}", t.id);
            }
            t
        })
        .collect()
}

fn fixture(name: &str) -> Value {
    let p = format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn listings_body() -> Value {
    let mut v = fixture("steam_listings_moonrise_sorted_price.json");
    let l = v["data"]["listings"].as_array_mut().unwrap();
    // preços em dobro (como o histórico): a R$ 6 o Moonrise não chega à régua de R$ 1,00 de lucro
    for x in l.iter_mut() {
        for k in ["unPrice", "unFee"] {
            x[k] = json!(x[k].as_i64().unwrap() * 2);
        }
    }
    l[0]["unPrice"] = json!(696);
    l[0]["unFee"] = json!(104);
    l[1]["unPrice"] = json!(870);
    l[1]["unFee"] = json!(130);
    l[1]["asset"]["asset_properties"][0]["int_value"] = json!("59");
    v
}

fn history_body() -> Value {
    let mut v = fixture("steam_pricehistory_moonrise_ft.json");
    for p in v["data"]["prices"].as_array_mut().unwrap() {
        p["price_median"] = json!(p["price_median"].as_f64().unwrap() * 2.0);
    }
    v
}

fn search_body() -> Value {
    let mut v = fixture("steam_search_render_pistol_p1900.json");
    v["results"][0]["hash_name"] = json!(MOON);
    v["results"][0]["name"] = json!(MOON);
    v["results"][0]["sell_price"] = json!(200);
    v
}

fn csfloat_body() -> Value {
    let mut v = fixture("csfloat_sales_moonrise_ft.json");
    for (i, seed) in [(0, 58), (2, 224), (3, 601)] {
        let price = v[i]["price"].as_i64().unwrap();
        v[i]["price"] = json!(price * 3 / 2);
        v[i]["item"]["paint_seed"] = json!(seed);
    }
    for (i, float) in [(4, 0.1628), (6, 0.155), (7, 0.16), (8, 0.1728)] {
        let price = v[i]["price"].as_i64().unwrap();
        v[i]["price"] = json!(price * 2);
        v[i]["item"]["float_value"] = json!(float);
    }
    v
}

struct Bodies {
    listings: Mutex<Value>,
    history: Value,
    history_queries: AtomicUsize,
    history_400: AtomicBool,
    history_wait: Mutex<Option<Arc<Notify>>>,
    history_started: Notify,
    search: Mutex<Value>,
    search_429: AtomicBool,
    search_queries: AtomicUsize,
    listing_queries: AtomicUsize,
    csfloat: Value,
    csfloat_names: Mutex<Vec<String>>,
    /// Livro de ofertas (captura real do Negev; o menor anúncio é trocado pelos testes).
    book: Mutex<Value>,
    book_queries: AtomicUsize,
    /// A leitura de anúncios responde 429 (bloqueio de horas da Steam).
    listings_429: AtomicBool,
    listings_500: AtomicBool,
    legacy_queries: AtomicUsize,
}

async fn serve() -> (String, Arc<Bodies>) {
    let b = Arc::new(Bodies {
        listings: Mutex::new(listings_body()),
        history: history_body(),
        history_queries: AtomicUsize::new(0),
        history_400: AtomicBool::new(false),
        history_wait: Mutex::new(None),
        history_started: Notify::new(),
        search: Mutex::new(search_body()),
        search_429: AtomicBool::new(false),
        search_queries: AtomicUsize::new(0),
        listing_queries: AtomicUsize::new(0),
        csfloat: csfloat_body(),
        csfloat_names: Mutex::new(vec![]),
        book: Mutex::new(fixture("steam_orderbook_negev_2026-11.json")),
        book_queries: AtomicUsize::new(0),
        listings_429: AtomicBool::new(false),
        listings_500: AtomicBool::new(false),
        legacy_queries: AtomicUsize::new(0),
    });
    async fn actions(
        AxState(b): AxState<Arc<Bodies>>,
        Query(q): Query<HashMap<String, String>>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        match q.get("q").map(String::as_str) {
            Some("QueryListingsForItem") => {
                b.listing_queries.fetch_add(1, Ordering::Relaxed);
                if b.listings_429.load(Ordering::Relaxed) {
                    return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
                }
                if b.listings_500.load(Ordering::Relaxed) {
                    return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                let mut body = b.listings.lock().unwrap().clone();
                // como a Valve: faixa de seed (propriedade 1) filtra os anúncios
                let qp: Value = serde_json::from_str(q.get("qp").unwrap()).unwrap();
                if let Some(f) = qp[0]["propertyFilters"].get("1") {
                    let bound = |k: &str| f[k].as_str().unwrap().parse::<i64>().unwrap();
                    let (lo, hi) = (bound("int_min"), bound("int_max"));
                    body["data"]["listings"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|l| {
                            let seed = l["asset"]["asset_properties"][0]["int_value"].as_str();
                            seed.and_then(|v| v.parse::<i64>().ok())
                                .is_some_and(|v| (lo..=hi).contains(&v))
                        });
                }
                // e a faixa de float (propriedade 2), em número
                if let Some(f) = qp[0]["propertyFilters"].get("2") {
                    let (lo, hi) = (
                        f["float_min"].as_f64().unwrap(),
                        f["float_max"].as_f64().unwrap(),
                    );
                    body["data"]["listings"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|l| {
                            l["asset"]["asset_properties"][1]["float_value"]
                                .as_f64()
                                .is_some_and(|v| (lo..=hi).contains(&v))
                        });
                }
                Json(body).into_response()
            }
            Some("QueryPriceHistory") => {
                b.history_queries.fetch_add(1, Ordering::Relaxed);
                let wait = b.history_wait.lock().unwrap().clone();
                if let Some(wait) = wait {
                    b.history_started.notify_one();
                    wait.notified().await;
                }
                if b.history_400.load(Ordering::Relaxed) {
                    axum::http::StatusCode::BAD_REQUEST.into_response()
                } else {
                    Json(b.history.clone()).into_response()
                }
            }
            other => panic!("consulta inesperada: {other:?}"),
        }
    }
    let app = Router::new()
        .route(
            "/fail",
            get(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/market/pricehistory/",
            get(|AxState(b): AxState<Arc<Bodies>>| async move {
                (
                    if b.history_400.load(Ordering::Relaxed) {
                        axum::http::StatusCode::BAD_REQUEST
                    } else {
                        axum::http::StatusCode::OK
                    },
                    Json(fixture("steam_legacy_history_moonrise_2026-11.json")),
                )
            }),
        )
        .route("/market/actions", get(actions))
        .route(
            "/market/orderbook",
            get(|AxState(b): AxState<Arc<Bodies>>| async move {
                b.book_queries.fetch_add(1, Ordering::Relaxed);
                Json(b.book.lock().unwrap().clone())
            }),
        )
        .route(
            "/market/search/render/",
            get(|AxState(b): AxState<Arc<Bodies>>| async move {
                use axum::response::IntoResponse;
                b.search_queries.fetch_add(1, Ordering::Relaxed);
                if b.search_429.load(Ordering::Relaxed) {
                    axum::http::StatusCode::TOO_MANY_REQUESTS.into_response()
                } else {
                    Json(b.search.lock().unwrap().clone()).into_response()
                }
            }),
        )
        .route(
            "/market/listings/730/{name}/render/",
            get(
                |AxState(b): AxState<Arc<Bodies>>, Path(name): Path<String>| async move {
                    use axum::response::IntoResponse;
                    b.legacy_queries.fetch_add(1, Ordering::Relaxed);
                    if name == MOON || name == ST_MOON {
                        let mut data = fixture("steam_legacy_render_moonrise_2026-11.json");
                        if name == ST_MOON {
                            let assets =
                                std::mem::take(data["assets"]["730"]["2"].as_object_mut().unwrap());
                            for (id, mut asset) in assets {
                                let id = format!("st-{id}");
                                asset["id"] = json!(id);
                                asset["market_hash_name"] = json!(ST_MOON);
                                data["assets"]["730"]["2"][&id] = asset;
                            }
                            for row in data["listinginfo"].as_object_mut().unwrap().values_mut() {
                                row["asset"]["id"] =
                                    json!(format!("st-{}", row["asset"]["id"].as_str().unwrap()));
                            }
                        }
                        let rows = std::mem::take(data["listinginfo"].as_object_mut().unwrap());
                        let mut unique = serde_json::Map::new();
                        for (id, mut row) in rows {
                            let id = format!(
                                "{}{id}",
                                if name == MOON {
                                    "legacy-"
                                } else {
                                    "legacy-st-"
                                }
                            );
                            row["listingid"] = json!(id);
                            unique.insert(id, row);
                        }
                        data["listinginfo"] = Value::Object(unique);
                        Json(data).into_response()
                    } else {
                        axum::http::StatusCode::NOT_FOUND.into_response()
                    }
                },
            ),
        )
        .route(
            "/history/{name}/sales",
            get(
                |AxState(b): AxState<Arc<Bodies>>, Path(n): Path<String>| async move {
                    b.csfloat_names.lock().unwrap().push(n);
                    Json(b.csfloat.clone())
                },
            ),
        )
        .with_state(b.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), b)
}

fn hunt_against(base: &str, db: Db) -> (Arc<Hunt>, broadcast::Receiver<Event>) {
    let (events, rx) = broadcast::channel(512);
    let steam = Arc::new(SteamClient::with_base(base).unwrap());
    steam.set_gap_for_test(Duration::from_millis(1));
    let h = Hunt::with_clock(
        HuntDeps {
            settings: SettingsStore::new(db.clone()),
            db,
            steam,
            csfloat: Arc::new(CsfloatClient::with_base(base).unwrap()),
            events,
            telegram: None,
        },
        clock,
    );
    (h, rx)
}

#[tokio::test]
async fn sales_cache_survives_restart_with_its_original_freshness_and_stickers() {
    let (base, _) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against(&base, db.clone());
    let sales = crate::csfloat::parse_sales(&csfloat_body()).unwrap();
    assert!(sales.iter().any(|s| s.has_stickers));
    patterns::cache_sales(&h, MOON, &sales).await;
    db.call(|c| {
        c.execute(
            "INSERT INTO csfloat_cache VALUES ('stale',?1,'[]')",
            [NOW - catalog::HISTORY_TTL],
        )?;
        c.execute(
            "INSERT INTO csfloat_cache VALUES ('bad',?1,'not json')",
            [NOW],
        )?;
        c.execute(
            "INSERT INTO csfloat_cache VALUES ('future',?1,'[]')",
            [NOW + 1],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let (restarted, _) = hunt_against(&base, db);
    patterns::load_cache(&restarted).await.unwrap();
    let s = restarted.lock();
    assert_eq!(s.csfloat_cache.len(), 1);
    assert_eq!(s.csfloat_cache[MOON], (NOW, sales));
}

#[tokio::test]
async fn sales_warmup_is_faster_only_while_normal_watched_cache_is_missing() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against(&base, db);
    h.lock().queue.set_tasks(
        [MOON, ST_MOON]
            .into_iter()
            .map(|name| Task {
                id: format!("d:{name}"),
                name: name.into(),
                code: "G180420B6053004".into(),
                wear: Some(2),
                seeds: vec![],
                float: None,
                score: 1.0,
            })
            .collect(),
    );
    h.lock().learn_at = NOW - 15;
    patterns::maybe_step(&h).await;
    assert_eq!(bodies.csfloat_names.lock().unwrap().len(), 1);
    h.lock().learn_at = NOW - 14;
    patterns::maybe_step(&h).await;
    assert_eq!(bodies.csfloat_names.lock().unwrap().len(), 1);
    h.lock().learn_at = NOW - 15;
    patterns::maybe_step(&h).await;
    assert_eq!(bodies.csfloat_names.lock().unwrap().len(), 2);
    h.lock().learn_at = NOW - 15;
    patterns::maybe_step(&h).await;
    assert_eq!(bodies.csfloat_names.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn candidate_history_uses_legacy_once_during_native_pause() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    db.call(|c| {
        for name in [MOON, ST_MOON] {
            c.execute("INSERT INTO items(name,g_code,paint_index,updated_at) VALUES (?1,'G180420B6053004',694,?2)", rusqlite::params![name,NOW])?;
            c.execute("INSERT INTO catalog_references(name,usd_cents,observed_at,rank) VALUES (?1,200,?2,1)", rusqlite::params![name,NOW])?;
        }
        Ok(())
    }).await.unwrap();
    {
        let mut s = h.lock();
        s.ratio = Some((5.0, NOW));
        s.history_retry_after.insert(ST_MOON.into(), NOW + 600);
    }
    bodies.listings_429.store(true, Ordering::Relaxed);
    assert!(
        h.steam
            .action(
                crate::steam::client::Endpoint::Listings,
                "QueryListingsForItem",
                &json!([])
            )
            .await
            .is_err()
    );
    h.steam.set_cookie(Some(crate::steam::client::Cookie {
        steam_login_secure: "local-test".into(),
        sessionid: None,
    }));
    assert_eq!(tick(&h).await, Tick::Discovery);
    let (fresh, waiting, attempts): (i64, Option<i64>, i64) = db.call(|c| {
        Ok((
            c.query_row("SELECT history_at FROM items WHERE name=?1", [MOON], |r| r.get(0))?,
            c.query_row("SELECT history_at FROM items WHERE name=?1", [ST_MOON], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM research_attempts WHERE endpoint='legacy' AND outcome != 'not_sent'", [], |r| r.get(0))?,
        ))
    }).await.unwrap();
    assert_eq!((fresh, waiting, attempts), (NOW, None, 1));
    assert_eq!(bodies.history_queries.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn watched_history_renews_through_legacy_during_native_pause() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    db.call(|c| {
        c.execute(
            "INSERT INTO items(name,updated_at,history_at) VALUES (?1,?2,?3)",
            rusqlite::params![MOON, NOW, NOW - catalog::HISTORY_TTL],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    {
        let mut s = h.lock();
        s.queue.set_tasks(boot_tasks(Task {
            id: format!("d:{MOON}"),
            name: MOON.into(),
            code: "G180420B6053004".into(),
            wear: Some(2),
            seeds: vec![],
            float: None,
            score: 1.0,
        }));
        for _ in 0..4 {
            s.queue.next(NOW);
        }
    }
    bodies.listings_429.store(true, Ordering::Relaxed);
    assert!(
        h.steam
            .action(
                crate::steam::client::Endpoint::Listings,
                "QueryListingsForItem",
                &json!([])
            )
            .await
            .is_err()
    );
    h.steam.set_cookie(Some(crate::steam::client::Cookie {
        steam_login_secure: "local-test".into(),
        sessionid: None,
    }));
    assert_eq!(tick(&h).await, Tick::Discovery);
    let at: i64 = db
        .call(|c| {
            c.query_row("SELECT history_at FROM items WHERE name=?1", [MOON], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    assert_eq!(at, NOW);
    assert_eq!(
        bodies.history_queries.load(Ordering::Relaxed),
        0,
        "native continua em pausa"
    );
    assert!(
        !catalog::refresh_watched_history(&h).await.unwrap(),
        "não repete histórico fresco"
    );
    let mut task = h.lock().queue.tasks().pop().unwrap();
    task.id = format!("d:{ST_MOON}");
    task.name = ST_MOON.into();
    db.call(|c| {
        c.execute(
            "INSERT INTO items(name,updated_at) VALUES (?1,?2)",
            rusqlite::params![ST_MOON, NOW],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    {
        let mut s = h.lock();
        s.queue.set_tasks(boot_tasks(task));
        for _ in 0..4 {
            s.queue.next(NOW);
        }
    }
    bodies.history_400.store(true, Ordering::Relaxed);
    assert_eq!(tick(&h).await, Tick::Discovery);
    assert!(
        !h.lock().queue.may_discover(),
        "histórico enviado com erro também gasta o crédito"
    );
}

#[tokio::test]
async fn refused_history_waits_per_name_then_recovers_without_faking_freshness() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db.clone());
    bodies.history_400.store(true, Ordering::Relaxed);
    for _ in 0..3 {
        assert!(points_for(&h, MOON, NOW).await.is_err());
    }
    assert_eq!(bodies.history_queries.load(Ordering::Relaxed), 1);
    assert!(points_for(&h, ST_MOON, NOW).await.is_err());
    assert_eq!(bodies.history_queries.load(Ordering::Relaxed), 2);
    let rows: i64 = db
        .call(|c| {
            c.query_row(
                "SELECT count(*) FROM items WHERE history_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(rows, 0, "falha não pode virar histórico recente");

    bodies.history_400.store(false, Ordering::Relaxed);
    h.lock().history_retry_after.insert(MOON.into(), NOW);
    assert!(!points_for(&h, MOON, NOW).await.unwrap().is_empty());
    assert_eq!(bodies.history_queries.load(Ordering::Relaxed), 3);
    assert!(!h.lock().history_retry_after.contains_key(MOON));
    assert!(h.lock().history_retry_after.contains_key(ST_MOON));
    assert!(!points_for(&h, MOON, NOW).await.unwrap().is_empty());
    assert_eq!(bodies.history_queries.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn sales_collector_warms_watched_cache_before_catalog_without_extra_slots() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db.clone());
    db.call(|c| {
        for (name, spd, floor) in [(MOON, 1, None), (ST_MOON, 1000, Some(800))] {
            c.execute(
                "INSERT INTO items(name,updated_at,floor_cents,sales_per_day) VALUES (?1,?2,?3,?4)",
                rusqlite::params![name, NOW, floor, spd],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    {
        let mut s = h.lock();
        s.learn_at = NOW - 120;
        s.queue.set_tasks(vec![Task {
            id: format!("d:{MOON}"),
            name: MOON.into(),
            code: "G180420B6053004".into(),
            wear: Some(2),
            seeds: vec![],
            float: None,
            score: 1.0,
        }]);
    }
    patterns::maybe_step(&h).await;
    assert_eq!(*bodies.csfloat_names.lock().unwrap(), vec![MOON]);
    assert!(h.lock().csfloat_cache.contains_key(MOON));
    patterns::maybe_step(&h).await;
    assert_eq!(
        bodies.csfloat_names.lock().unwrap().len(),
        1,
        "mantém intervalo de 2 min"
    );

    h.lock().learn_at = NOW - 120;
    patterns::maybe_step(&h).await;
    assert_eq!(*bodies.csfloat_names.lock().unwrap(), vec![MOON, ST_MOON]);
    {
        let mut s = h.lock();
        s.learn_at = NOW - 120;
        s.csfloat_cache.get_mut(MOON).unwrap().0 = NOW - catalog::HISTORY_TTL;
        s.learn_seen.insert(MOON.into(), NOW - 599);
    }
    patterns::maybe_step(&h).await;
    assert_eq!(
        bodies.csfloat_names.lock().unwrap().len(),
        2,
        "falha recente espera 10 min"
    );
    {
        let mut s = h.lock();
        s.learn_at = NOW - 120;
        s.learn_seen.insert(MOON.into(), NOW - 600);
    }
    patterns::maybe_step(&h).await;
    assert_eq!(
        *bodies.csfloat_names.lock().unwrap(),
        vec![MOON, ST_MOON, MOON]
    );
    assert_eq!(
        h.lock().csfloat_cache[MOON].0,
        NOW,
        "renova em 6 h, sem esperar 24 h"
    );
    {
        let mut s = h.lock();
        s.learn_at = NOW - 120;
        s.learn_seen.clear();
        s.csfloat_cache.clear();
        s.queue.external = Some(format!("d:{MOON}"));
    }
    patterns::maybe_step(&h).await;
    assert_eq!(
        bodies.csfloat_names.lock().unwrap().last().unwrap(),
        ST_MOON,
        "candidata externa não ganha aquecimento fora da cota"
    );
}

#[tokio::test]
async fn old_history_failure_cannot_delay_a_new_login() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db);
    let release = Arc::new(Notify::new());
    *bodies.history_wait.lock().unwrap() = Some(release.clone());
    bodies.history_400.store(true, Ordering::Relaxed);
    let pending = {
        let h = h.clone();
        tokio::spawn(async move { points_for(&h, MOON, NOW).await })
    };
    bodies.history_started.notified().await;
    h.steam.set_cookie(Some(crate::steam::client::Cookie {
        steam_login_secure: "new-test-login".into(),
        sessionid: None,
    }));
    h.lock().history_retry_after.clear();
    release.notify_one();
    assert!(pending.await.unwrap().is_err());
    assert!(h.lock().history_retry_after.is_empty());
    *bodies.history_wait.lock().unwrap() = None;
    bodies.history_400.store(false, Ordering::Relaxed);
    assert!(!points_for(&h, MOON, NOW).await.unwrap().is_empty());
    assert_eq!(bodies.history_queries.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn fixture_listing_becomes_opportunity_event_then_gone() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);

    let mut found: Vec<Opportunity> = vec![];
    for _ in 0..400 {
        if tick(&h).await == Tick::Idle {
            break;
        }
        while let Ok(ev) = rx.try_recv() {
            if let Event::Opportunity(o) = ev {
                found.push(o);
            }
        }
        if found.len() >= 2 {
            break;
        }
    }
    found.sort_by_key(|o| o.price_cents);
    assert_eq!(found.len(), 2, "{found:#?}");
    let (seen, opportunities): (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT sum(listings), sum(opportunities) FROM hourly_activity",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert!(seen >= 2);
    assert_eq!(opportunities, 2);

    // razão medida: R$ 4,00 (menor anúncio) ÷ US$ 1,00
    assert_eq!(h.lock().ratio.map(|r| r.0), Some(4.0));

    let (cheap, t1) = (&found[0], &found[1]);
    assert_eq!((cheap.id.as_str(), cheap.price_cents), (ID_CHEAP, 800));
    assert_eq!(cheap.kind, Kind::Desconto);
    assert_eq!(cheap.group, None);
    assert_eq!(cheap.wear.as_deref(), Some("Field-Tested"));
    assert_eq!(cheap.seed, Some(151));
    assert_eq!(t1.id, ID_T1);
    assert_eq!(t1.kind, Kind::Raro);
    assert_eq!(t1.group.as_deref(), Some("T1 Center Star"));
    assert!(
        t1.evidence.contains("grupo raro: 3 vendas"),
        "{}",
        t1.evidence
    );
    // números conferidos à mão: mediana ~R$ 11,8 com queda de 2 %; 11,57 → líquido 10,07 (taxa 15 %)
    let tiers = |o: &Opportunity| {
        [&o.sell.quick, &o.sell.mid, &o.sell.high]
            .map(|t| (t.price_cents, t.net_cents, t.profit_cents))
    };
    assert_eq!(
        tiers(cheap),
        [(1124, 979, 179), (1157, 1007, 207), (1184, 1030, 230)]
    );
    assert_eq!((cheap.margin_pct, cheap.safe), (25.875, true));
    assert_eq!(cheap.confidence, Confidence::Alta);
    assert_eq!(
        cheap.evidence,
        "339 vendas/dia nos últimos 7 dias · preço em queda, descontado 2%"
    );
    // a venda rápida não conta com o prêmio do CSFloat: sem ele o T1 a R$ 10,00 perde, então não é seguro
    assert_eq!(
        tiers(t1),
        [(1124, 979, -21), (1776, 1545, 545), (1826, 1589, 589)]
    );
    assert!((t1.margin_pct - 54.5).abs() < 1e-3 && !t1.safe);
    assert_eq!(t1.confidence, Confidence::Media);
    for o in &found {
        assert_eq!(o.name, MOON);
        assert!(o.steam_url.contains(&format!("detail={}", o.id)));
        assert!(o.sell.mid.profit_cents >= 100 && o.margin_pct >= 10.0);
        assert!(o.sell.quick.price_cents <= o.sell.mid.price_cents);
        assert!(o.sell.mid.price_cents <= o.sell.high.price_cents);
    }

    // o anúncio inteiro fica guardado para a compra refazer o pedido
    let (lj, task): (String, String) = db
        .call(|c| {
            c.query_row(
                &format!("SELECT listing_json, task FROM opportunities WHERE id = '{ID_CHEAP}'"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    let l: Listing = serde_json::from_str(&lj).unwrap();
    assert_eq!(
        (l.listing_id.as_str(), l.price_cents, l.seller_cents),
        (ID_CHEAP, 800, 696)
    );
    assert!(task.starts_with("d:") || task.starts_with("r:"));

    // combo pelo lucro esperado com risco: os dois cabem no saldo e os dois têm lucro esperado positivo
    let st = h.state();
    assert_eq!(st.combo.cost_cents, 1800);
    let mut ids = st.combo.ids.clone();
    ids.sort();
    assert_eq!(ids, vec![ID_CHEAP.to_string(), ID_T1.to_string()]);
    assert!(st.hunt.watching >= 10, "watching {}", st.hunt.watching);

    // o que está sendo vigiado: piso visto, teto de compra (de onde ainda sobra R$ 1,00 e 10 %) e ritmo
    let watch = watch::list(&h).await.unwrap();
    assert_eq!(watch.len(), st.hunt.watching as usize);
    // o Moonrise (aqui raro por seed) aparece com o piso visto e o teto do prêmio do grupo: 3 vendas do
    // CSFloat → mid R$ 17,76 → líquido 15,45 → compraria até 14,04 (10 % de margem)
    let raro = watch
        .iter()
        .find(|r| r.kind == Kind::Raro && r.floor_cents == Some(1000))
        .unwrap_or_else(|| panic!("piso R$ 10,00 visto na faixa 58-59: {watch:#?}"));
    assert_eq!((raro.kind, raro.max_buy_cents), (Kind::Raro, Some(1404)));
    assert_eq!(raro.last_checked_at, Some(NOW));
    // mais perto de compensar primeiro; raro fica fora da conta (piso de qualquer padrão × teto do grupo)
    // e vai para o fim, mesmo com piso abaixo do teto como este Moonrise
    let gaps: Vec<i64> = watch
        .iter()
        .filter(|r| r.kind != Kind::Raro)
        .filter_map(|r| r.floor_cents.zip(r.max_buy_cents).map(|(f, m)| f - m))
        .collect();
    assert!(gaps.windows(2).all(|w| w[0] <= w[1]), "{gaps:?}");
    let first_raro = watch.iter().position(|r| r.kind == Kind::Raro).unwrap();
    assert!(
        watch[first_raro..]
            .iter()
            .all(|r| r.kind == Kind::Raro || r.floor_cents.zip(r.max_buy_cents).is_none()),
        "{watch:#?}"
    );

    // anúncio já visto não vira evento de novo, por mais voltas que a caça dê
    for _ in 0..60 {
        tick(&h).await;
    }
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, Event::Opportunity(_)),
            "oportunidade repetida"
        );
    }

    // o anúncio barato some da Steam: a próxima consulta da tarefa vira `gone`
    bodies.listings.lock().unwrap()["data"]["listings"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let mut gone = None;
    for _ in 0..400 {
        tick(&h).await;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Gone { id, .. } = ev {
                gone = Some(id);
            }
        }
        if gone.is_some() {
            break;
        }
    }
    assert_eq!(gone.as_deref(), Some(ID_CHEAP));
    assert_eq!(h.state().combo.ids, vec![ID_T1.to_string()]);
    let gone_at: Option<i64> = db
        .call(|c| {
            c.query_row(
                &format!("SELECT gone_at FROM opportunities WHERE id = '{ID_CHEAP}'"),
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(gone_at, Some(NOW));
    let reason: String = db
        .call(|c| {
            c.query_row(
                "SELECT json_extract(json, '$.gone_reason') FROM opportunities WHERE id=?1",
                [ID_CHEAP],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(reason, "unavailable");
}

#[tokio::test]
async fn elite_float_listing_is_priced_by_the_premium_of_its_band() {
    const ID_FLOAT: &str = "6932893052431123649";
    let (base, bodies) = serve().await;
    {
        let mut b = bodies.listings.lock().unwrap();
        let l = &mut b["data"]["listings"][12];
        l["asset"]["asset_properties"][1]["float_value"] = json!(0.152);
        l["unPrice"] = json!(1044);
        l["unFee"] = json!(156);
    }
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let mut hit = None;
    for _ in 0..400 {
        tick(&h).await;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Opportunity(o) = ev
                && o.id == ID_FLOAT
            {
                hit = Some(o);
            }
        }
        if hit.is_some() {
            break;
        }
    }
    // R$ 12,00 no FT comum de ~R$ 11,6 não dá lucro; com float 0,152 a faixa vendeu a ~2× (3 vendas: a de seed 95 é tier do Moonrise, pattern vence float)
    let o = hit.expect("float de elite vira oportunidade");
    assert_eq!(
        (o.kind, o.group.as_deref()),
        (Kind::Raro, Some("Float ≤ 0.173"))
    );
    assert!(
        o.evidence.contains("grupo raro: 3 vendas"),
        "{}",
        o.evidence
    );
    assert!(o.sell.mid.profit_cents >= 100 && !o.safe, "{:?}", o.sell);
    // o raro chega com selo: 3 vendas fechadas do grupo (CSFloat guardado) não comprovam
    let seal = o.seal.as_ref().expect("raro tem selo");
    assert_eq!((seal.proven, seal.sales), (false, 3), "{seal:?}");
    assert_eq!(seal.text, "ainda não comprovado: 3 de 10 vendas fechadas");
    assert!(seal.open_cents.is_some() && seal.floor_cents.is_some());
    // a agenda tem braços de float, com o filtro de float na consulta
    let tasks = h.lock().queue.tasks();
    assert!(
        tasks
            .iter()
            .any(|t| t.id == format!("f:{MOON}") && t.float.is_some())
    );
}

/// Roda a caça até aparecerem as duas oportunidades da fixture.
async fn run_until_two(h: &Arc<Hunt>, rx: &mut broadcast::Receiver<Event>) -> Vec<Opportunity> {
    let mut found = vec![];
    for _ in 0..400 {
        if tick(h).await == Tick::Idle {
            break;
        }
        while let Ok(ev) = rx.try_recv() {
            if let Event::Opportunity(o) = ev {
                found.push(o);
            }
        }
        if found.len() >= 2 {
            break;
        }
    }
    found
}

#[tokio::test]
async fn cheaper_competitor_reevaluates_known_opportunity() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    assert_eq!(run_until_two(&h, &mut rx).await.len(), 2);
    let approved: i64 = db
        .call(|c| {
            c.query_row(
                "SELECT count(DISTINCT listing_id) FROM evaluations
                 WHERE reason = 'aprovada' AND mid_profit_cents > 0 AND detail IS NOT NULL",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(approved, 2);
    // os dois avisos viram compras virtuais do professor, uma vez cada
    let ghosts: (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT count(*), coalesce(sum(approved), 0) FROM ghosts WHERE status = 'aguardando'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!(ghosts.1, 2, "{ghosts:?}");
    // outro comum baixa para R$ 4,00: o barato (mesmo preço, mesmo histórico) só vende a R$ 3,99 e perde
    {
        let mut b = bodies.listings.lock().unwrap();
        let other = &mut b["data"]["listings"][2];
        other["unPrice"] = json!(696);
        other["unFee"] = json!(104);
    }
    let mut gone = None;
    for _ in 0..400 {
        tick(&h).await;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Gone { id, .. } = ev {
                gone = Some(id);
            }
        }
        if gone.is_some() {
            break;
        }
    }
    assert_eq!(gone.as_deref(), Some(ID_CHEAP));
}

#[tokio::test]
async fn rare_competitor_in_another_seed_range_caps_mid_and_drops_it() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    // passa pelo fluxo real uma vez (catálogo, histórico, CSFloat); o T1 sai com o médio a R$ 8,87
    assert_eq!(run_until_two(&h, &mut rx).await.len(), 2);
    let page = crate::steam::listings::parse_page(&listings_body()["data"], 0).unwrap();
    let t1 = page
        .listings
        .iter()
        .find(|l| l.listing_id == ID_T1)
        .unwrap()
        .clone();
    let ranges = crate::pricing::groups::seed_ranges(MOON);
    let (lo, hi) = *ranges.iter().find(|r| !(r.0..=r.1).contains(&59)).unwrap();
    let rare = |seeds| Task {
        id: "r:G1:ft".into(),
        name: MOON.into(),
        code: "G1".into(),
        wear: None,
        seeds,
        float: None,
        score: 1.0,
    };
    let only = |l: Listing| {
        let mut p = page.clone();
        p.listings = vec![l];
        p
    };
    // outro T1 Center Star na faixa de seed vizinha: a página do T1 a R$ 7,00 não o mostra
    let range59 = *ranges.iter().find(|r| (r.0..=r.1).contains(&59)).unwrap();
    let mut rival = t1.clone();
    rival.listing_id = "1".into();
    rival.seed = Some(lo);
    let mut gone = None;
    // R$ 9,50 fica acima do médio (R$ 8,87): nada muda. R$ 8,00 limita o médio a R$ 7,99, líquido abaixo dos
    // R$ 7,00 pagos, e o T1 deixa de ser oportunidade.
    for price in [1900, 1250] {
        rival.price_cents = price;
        detect::process(&h, &rare(vec![(lo, hi)]), &only(rival.clone()))
            .await
            .unwrap();
        detect::process(&h, &rare(vec![range59]), &only(t1.clone()))
            .await
            .unwrap();
        assert_eq!(h.lock().evaluated[ID_T1].2, Some(price));
        while let Ok(ev) = rx.try_recv() {
            if let Event::Gone { id, .. } = ev {
                gone = Some(id);
            }
        }
        if price == 1900 {
            assert_eq!(gone, None);
            assert_eq!(h.lock().active[ID_T1].opp.sell.mid.price_cents, 1776);
        }
    }
    assert_eq!(gone.as_deref(), Some(ID_T1));
}

#[tokio::test]
async fn grouped_search_credit_requires_a_page_transport_even_if_that_page_fails() {
    for blocked in [true, false] {
        let (base, bodies) = serve().await;
        let (db, _dir) = temp_db();
        let (h, mut rx) = hunt_against(&base, db.clone());
        h.set_saldo(3608, SaldoSource::Manual);
        run_until_two(&h, &mut rx).await;
        let mut task = h
            .lock()
            .queue
            .tasks()
            .into_iter()
            .find(|t| t.id == format!("d:{MOON}"))
            .unwrap();
        task.code = "G180420B6053004".into();
        {
            let mut s = h.lock();
            s.queue.set_tasks(boot_tasks(task.clone()));
            s.active.clear();
            for _ in 0..4 {
                s.queue.spend_discovery();
            }
            // Uma seleção já deu 1/4; três páginas completam o crédito antes de abrir o breaker.
            s.queue.next(NOW);
        }
        if blocked {
            for endpoint in [
                crate::steam::client::Endpoint::Listings,
                crate::steam::client::Endpoint::Legacy,
            ] {
                for _ in 0..3 {
                    assert!(h.steam.get_json(endpoint, "/fail", &[]).await.is_err());
                }
            }
        } else {
            bodies.listings_500.store(true, Ordering::Relaxed);
        }
        for _ in 0..3 {
            let decision = crate::research::Decision {
                source: "normal".into(),
                policy: "test".into(),
                target: task.id.clone(),
                scale: 0.03,
                block_id: None,
            };
            let _ =
                crate::research::scope(db.clone(), decision, scheduler::run_task(&h, &task)).await;
        }
        assert_eq!(
            h.lock().queue.may_discover(),
            !blocked,
            "busca sem página não ganha crédito extra; página enviada com erro ganha"
        );
        let sent:i64=db.call(|c|c.query_row("SELECT count(*) FROM research_attempts WHERE source='normal' AND endpoint IN ('listings','legacy') AND outcome!='not_sent' AND decision_id IN (SELECT id FROM research_decisions WHERE policy='test')",[],|r|r.get(0))).await.unwrap();
        assert_eq!(sent > 0, !blocked);
    }
}

#[tokio::test]
async fn unavailable_grouped_search_keeps_scanning_listings_without_a_phone() {
    use crate::steam::client::Endpoint;
    for failure in ["first_429", "paused", "breaker"] {
        let (base, bodies) = serve().await;
        let (db, _dir) = temp_db();
        let (h, mut rx) = hunt_against(&base, db);
        let task = lone_moonrise(&h, &mut rx).await;
        let mut duplicate = task.clone();
        duplicate.id = format!("duplicate:{}", task.id);
        h.lock().queue.set_tasks(vec![task.clone(), duplicate]);
        set_lowest_ask(&bodies, 200);
        bodies.search_429.store(true, Ordering::Relaxed);
        if failure == "paused" {
            assert!(
                h.steam
                    .get_json(Endpoint::Search, "/market/search/render/", &[])
                    .await
                    .is_err()
            );
        } else if failure == "breaker" {
            for _ in 0..3 {
                assert!(
                    h.steam
                        .get_json(Endpoint::Search, "/fail", &[])
                        .await
                        .is_err()
                );
            }
        }
        let searches = bodies.search_queries.load(Ordering::Relaxed);
        let listings = bodies.listing_queries.load(Ordering::Relaxed);
        for step in 1..=2 {
            tokio::time::timeout(Duration::from_secs(5), scheduler::run_task(&h, &task))
                .await
                .expect("anúncios não esperam a busca nem o celular")
                .unwrap();
            assert_eq!(
                bodies.listing_queries.load(Ordering::Relaxed),
                listings + step,
                "{failure}"
            );
            assert_eq!(
                bodies.search_queries.load(Ordering::Relaxed),
                searches + usize::from(failure == "first_429"),
                "não repete a busca barrada: {failure}"
            );
        }
    }
}

#[tokio::test]
async fn search_skips_expensive_and_deduplicates_affordable_listings() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    run_until_two(&h, &mut rx).await;
    let task = h
        .lock()
        .queue
        .tasks()
        .into_iter()
        .find(|t| t.id == format!("d:{MOON}"))
        .unwrap();
    let mut duplicate = task.clone();
    duplicate.id = format!("d:duplicada:{MOON}");
    {
        let mut s = h.lock();
        s.queue.set_tasks(vec![task.clone(), duplicate.clone()]);
        s.active.clear();
        s.floors.clear();
        s.cycle.saved_listings = 0;
    }
    bodies.search.lock().unwrap()["results"][0]["sell_price"] = json!(20000);
    let before = bodies.listing_queries.load(Ordering::Relaxed);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), before);
    assert_eq!(h.lock().cycle.saved_listings, 2);

    bodies.search.lock().unwrap()["results"][0]["sell_price"] = json!(200);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), before + 1);
    let s = h.lock();
    assert!(s.floors.contains_key(&task.id));
    assert!(s.floors.contains_key(&duplicate.id));
}

/// Moonrise FT sozinho na vigia (sem outra versão para a busca cobrir): o passo vai pelo livro.
async fn lone_moonrise(h: &Arc<Hunt>, rx: &mut broadcast::Receiver<Event>) -> Task {
    h.set_saldo(3608, SaldoSource::Manual);
    run_until_two(h, rx).await;
    let task = h
        .lock()
        .queue
        .tasks()
        .into_iter()
        .find(|t| t.id == format!("d:{MOON}"))
        .unwrap();
    let mut s = h.lock();
    s.queue.set_tasks(vec![task.clone()]);
    s.active.clear();
    s.floors.clear();
    s.cycle.saved_listings = 0;
    task
}

fn set_lowest_ask(b: &Bodies, cents: i64) {
    b.book.lock().unwrap()["data"]["data"]["amtMinSellOrder"] = json!(cents);
}

#[tokio::test]
async fn newly_unaffordable_page_reselects_without_waiting_fifteen_minutes() {
    let (base, _) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let task = lone_moonrise(&h, &mut rx).await;
    h.lock().selected = Some((h.saldo(), NOW));
    let mut body = listings_body();
    body["data"]["start"] = json!(0);
    for listing in body["data"]["listings"].as_array_mut().unwrap() {
        listing["unPrice"] = json!(5000);
        listing["unFee"] = json!(750);
    }
    let page = crate::steam::listings::parse_page(&body["data"], 0).unwrap();
    detect::process(&h, &task, &page).await.unwrap();
    catalog::maybe_reselect(&h).await.unwrap();
    assert!(
        h.lock().queue.tasks().iter().all(|t| t.id != task.id),
        "não gasta os próximos 15 min no alvo que deixou de caber no saldo"
    );
    let selected = h.lock().selected;
    detect::process(&h, &task, &page).await.unwrap();
    assert_eq!(
        h.lock().selected,
        selected,
        "preço igual não refaz a seleção"
    );
}

#[tokio::test]
async fn a_recent_common_page_does_not_spend_another_listing_query() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let task = lone_moonrise(&h, &mut rx).await;
    {
        let mut s = h.lock();
        let tasks = boot_tasks(task.clone());
        for t in &tasks {
            s.floors.insert(t.floor_key(), (Some(700), NOW));
        }
        s.queue.set_tasks(tasks);
        for _ in 0..4 {
            s.queue.spend_discovery();
        }
        s.book_at = NOW;
    }
    let before = (
        bodies.listing_queries.load(Ordering::Relaxed),
        bodies.legacy_queries.load(Ordering::Relaxed),
    );
    assert_eq!(tick(&h).await, Tick::Idle);
    assert_eq!(
        (
            bodies.listing_queries.load(Ordering::Relaxed),
            bodies.legacy_queries.load(Ordering::Relaxed)
        ),
        before
    );
    {
        let mut s = h.lock();
        for (_, at) in s.floors.values_mut() {
            *at = NOW - super::COMMON_REVISIT_SECS;
        }
    }
    assert!(
        matches!(tick(&h).await, Tick::Watched(_)),
        "página vencida volta à fila"
    );
}

#[tokio::test]
async fn grouped_public_pages_reuse_recent_siblings_but_recheck_active_ones() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let active = run_until_two(&h, &mut rx)
        .await
        .into_iter()
        .find(|o| o.id == ID_CHEAP)
        .unwrap();
    let mut task = h
        .lock()
        .queue
        .tasks()
        .into_iter()
        .find(|t| t.id == format!("d:{MOON}"))
        .unwrap();
    task.code = "G180420B6053004".into();
    let sibling = Task {
        id: format!("d:{ST_MOON}"),
        name: ST_MOON.into(),
        ..task.clone()
    };
    {
        let mut s = h.lock();
        s.queue.set_tasks(vec![task.clone(), sibling.clone()]);
        s.active.clear();
        s.floors.clear();
        s.floors.insert(sibling.floor_key(), (Some(700), NOW));
    }
    h.steam
        .restore_pace(&format!("{}|6000|60000", crate::db::now_unix() + 600));
    let before = bodies.legacy_queries.load(Ordering::Relaxed);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(
        bodies.legacy_queries.load(Ordering::Relaxed),
        before + 1,
        "não relê a versão irmã recente"
    );
    h.lock().active.insert(
        "active-st".into(),
        Active {
            task: sibling.id.clone(),
            attempted_at: 0,
            opp: Opportunity {
                id: "active-st".into(),
                name: ST_MOON.into(),
                ..active
            },
        },
    );
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(
        bodies.legacy_queries.load(Ordering::Relaxed),
        before + 3,
        "oportunidade ativa continua sendo reconferida"
    );
}

#[tokio::test]
async fn old_pace_snapshot_recovers_public_429_from_the_attempt_ledger() {
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against("http://127.0.0.1:9", db.clone());
    let now = crate::db::now_unix();
    catalog::set_setting(&db, "steam_pace", format!("{}|12000|120000|0", now + 3600))
        .await
        .unwrap();
    db.call(move |c| {
        c.execute("INSERT INTO research_attempts(at,source,endpoint,route,outcome) VALUES (?1,'normal','legacy','main','rate_limited')", [now - 60])?;
        Ok(())
    }).await.unwrap();
    scheduler::load_pace(&h).await.unwrap();
    assert!(h.steam.blocked(crate::steam::client::Endpoint::Listings));
    assert!(
        matches!(h.steam.get_json(crate::steam::client::Endpoint::Legacy, "/blocked", &[]).await,
        Err(crate::steam::client::Error::RateLimited { until }) if until >= now + 1735)
    );
    assert!(!h.steam.blocked(crate::steam::client::Endpoint::Session));
    assert_eq!(
        h.steam.sent(),
        0,
        "a migração não testa a Steam durante a pausa"
    );
}

#[tokio::test]
async fn ready_public_listings_skip_the_book_and_search_prefilters() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let mut task = lone_moonrise(&h, &mut rx).await;
    task.code = "G180420B6053004".into();
    h.lock().queue.set_tasks(vec![task.clone()]);
    h.steam.restore_pace("0|6000|60000|0");
    crate::steam::listings::query_listings_wear(&h.steam, &task.code, 0, task.wear, &[])
        .await
        .unwrap();
    let before = (
        bodies.book_queries.load(Ordering::Relaxed),
        bodies.search_queries.load(Ordering::Relaxed),
        bodies.legacy_queries.load(Ordering::Relaxed),
    );
    tokio::time::timeout(Duration::from_secs(1), scheduler::run_task(&h, &task))
        .await
        .expect("a triagem não deve gastar a vaga do render")
        .unwrap();
    assert_eq!(bodies.book_queries.load(Ordering::Relaxed), before.0);
    assert_eq!(bodies.search_queries.load(Ordering::Relaxed), before.1);
    assert_eq!(bodies.legacy_queries.load(Ordering::Relaxed), before.2 + 1);
}

#[tokio::test]
async fn book_skips_listings_when_the_lowest_ask_is_above_the_ceiling() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let task = lone_moonrise(&h, &mut rx).await;
    let listings = || bodies.listing_queries.load(Ordering::Relaxed);
    let books = || bodies.book_queries.load(Ordering::Relaxed);
    let legacy = || bodies.legacy_queries.load(Ordering::Relaxed);
    let (l0, b0) = (listings(), books());

    set_lowest_ask(&bodies, 20000); // R$ 200: nenhum anúncio cabe no teto
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(
        (listings(), books(), legacy()),
        (l0, b0 + 1, 0),
        "só o livro"
    );
    assert_eq!(h.lock().cycle.saved_listings, 1);
    assert_eq!(
        h.lock().books[MOON].1.as_ref().unwrap().min_sell_cents,
        Some(20000),
        "o livro lido serve também aos pedidos sugeridos"
    );

    // R$ 2: cabe, então lê os anúncios, mesmo que o pedido sugerido tenha lido esse livro antes (o próprio
    // menor anúncio não concorre com ele mesmo no teto)
    set_lowest_ask(&bodies, 200);
    h.lock()
        .books
        .get_mut(MOON)
        .unwrap()
        .1
        .as_mut()
        .unwrap()
        .min_sell_cents = Some(200);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!((listings(), books(), legacy()), (l0 + 1, b0 + 2, 0));
}

#[tokio::test]
async fn blocked_native_listings_prioritize_public_ads_over_book_prefilters() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let mut task = lone_moonrise(&h, &mut rx).await;
    task.code = "G180420B6053004".into(); // identidade que o fixture render realmente valida
    h.lock().queue.set_tasks(vec![task.clone()]);
    let listings = || bodies.listing_queries.load(Ordering::Relaxed);
    let books = || bodies.book_queries.load(Ordering::Relaxed);
    let legacy = || bodies.legacy_queries.load(Ordering::Relaxed);

    // a leitura de anúncios leva 429: pausa só esse balde
    bodies.listings_429.store(true, Ordering::Relaxed);
    set_lowest_ask(&bodies, 200);
    let (actions0, legacy_before) = (listings(), legacy());
    scheduler::run_task(&h, &task).await.unwrap();
    assert!(h.steam.blocked(crate::steam::client::Endpoint::Listings));
    assert_eq!((listings(), legacy()), (actions0 + 1, legacy_before + 1));
    let recovered = h.state().hunt;
    assert!(
        recovered.paused_until.is_none(),
        "o render validado mantém a caça ativa"
    );
    assert!(h.steam.blocked(crate::steam::client::Endpoint::Listings));
    assert!(
        recovered.last_ok_at.is_some(),
        "render validado conta como resposta boa"
    );
    assert!(
        recovered
            .message
            .unwrap()
            .contains("Steam: seed/float em pausa")
    );
    let (l0, b0) = (listings(), books());
    let legacy0 = legacy();

    // O render ainda funciona: usa a vaga para anúncios, mesmo se o livro subir acima do teto.
    let quick = Duration::from_secs(5);
    tokio::time::timeout(quick, scheduler::run_task(&h, &task))
        .await
        .expect("não espera a pausa dos anúncios")
        .unwrap();
    assert_eq!((listings(), books(), legacy()), (l0, b0, legacy0 + 1));
    assert_eq!(listings(), l0, "não repete /actions durante a pausa");
    set_lowest_ask(&bodies, 20000);
    tokio::time::timeout(quick, scheduler::run_task(&h, &task))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((listings(), books(), legacy()), (l0, b0, legacy0 + 2));

    // e a agenda deixa os raros (que só os anúncios servem) de fora enquanto dura o bloqueio
    let mut rare = task.clone();
    rare.id = "r:teste".into();
    rare.seeds = vec![(59, 59)];
    let mut s = h.lock();
    s.queue.set_tasks(vec![rare, task.clone()]);
    for _ in 0..5 {
        let t = s.queue.next_matching(NOW, |t| !t.rare()).unwrap();
        assert_eq!(t.id, task.id);
    }
}

#[tokio::test]
async fn cheap_legacy_fallback_learns_during_listings_pause() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    let mut task = lone_moonrise(&h, &mut rx).await;
    task.code = "G180420B6053004".into(); // identidade que o fixture render realmente valida
    h.lock().queue.set_tasks(vec![task.clone()]);
    bodies.listings_429.store(true, Ordering::Relaxed);
    set_lowest_ask(&bodies, 200);
    let actions0 = bodies.listing_queries.load(Ordering::Relaxed);
    let legacy0 = bodies.legacy_queries.load(Ordering::Relaxed);

    let before: (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM listings_seen), (SELECT count(*) FROM evaluations)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    scheduler::run_task(&h, &task).await.unwrap();
    let after: (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM listings_seen), (SELECT count(*) FROM evaluations)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();

    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), actions0 + 1);
    assert_eq!(bodies.legacy_queries.load(Ordering::Relaxed), legacy0 + 1);
    assert!(
        after.0 > before.0,
        "detect::process registrou anúncios vistos"
    );
    assert!(after.1 > before.1, "detect::process registrou avaliações");
    let state = h.state().hunt;
    assert!(
        state.paused_until.is_none(),
        "o render validado mantém a caça ativa"
    );
    assert!(h.steam.blocked(crate::steam::client::Endpoint::Listings));
    assert!(
        state.last_ok_at.is_some(),
        "render validado recupera last_ok"
    );

    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), actions0 + 1);
    assert_eq!(bodies.legacy_queries.load(Ordering::Relaxed), legacy0 + 2);
}

#[tokio::test]
async fn legacy_queries_each_name_without_marking_other_version_gone() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let found = run_until_two(&h, &mut rx).await;
    let mut task = h
        .lock()
        .queue
        .tasks()
        .into_iter()
        .find(|t| t.id == format!("d:{MOON}"))
        .unwrap();
    task.code = "G180420B6053004".into();
    let mut other = task.clone();
    other.name = ST_MOON.into();
    other.id = format!("d:{ST_MOON}");
    let mut opp = found[0].clone();
    opp.id = "legacy-st-5074569024067185966".into();
    opp.name = ST_MOON.into();
    opp.price_cents = 530;
    {
        let mut s = h.lock();
        s.active.clear();
        s.active.insert(
            opp.id.clone(),
            Active {
                opp: opp.clone(),
                task: other.id.clone(),
                attempted_at: 0,
            },
        );
        s.queue.set_tasks(vec![task.clone(), other.clone()]);
        s.floors.clear(); // As duas versões precisam de leitura neste cenário de cobertura.
    }
    bodies.listings_429.store(true, Ordering::Relaxed);
    let before = bodies.legacy_queries.load(Ordering::Relaxed);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(bodies.legacy_queries.load(Ordering::Relaxed), before + 2);
    assert!(
        h.lock().active.contains_key(&opp.id),
        "a página de outra versão não pode removê-la"
    );
    assert!(
        h.lock().floors.contains_key(&other.id),
        "a segunda versão também foi consultada"
    );
    let (keys, gone): (i64, i64) = h.db.call(|c| c.query_row(
        "SELECT COUNT(DISTINCT query), SUM(gone_at IS NOT NULL) FROM observations WHERE listing_id LIKE 'legacy-%'",
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )).await.unwrap();
    assert_eq!(
        (keys, gone),
        (2, 0),
        "cobertura do registro separada por nome"
    );
}

#[tokio::test]
async fn rare_listing_does_not_use_name_only_legacy_fallback() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let mut rare = lone_moonrise(&h, &mut rx).await;
    rare.code = "G180420B6053004".into();
    rare.id = "r:moonrise-seed".into();
    rare.seeds = vec![(59, 59)];
    let (actions0, legacy0) = (
        bodies.listing_queries.load(Ordering::Relaxed),
        bodies.legacy_queries.load(Ordering::Relaxed),
    );
    bodies.listings_429.store(true, Ordering::Relaxed);

    assert!(scheduler::run_task(&h, &rare).await.is_err());
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), actions0 + 1);
    assert_eq!(bodies.legacy_queries.load(Ordering::Relaxed), legacy0);
}

#[tokio::test]
async fn unsent_watch_does_not_count_as_a_learning_sample() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let tasks: Vec<Task> = (0..10)
        .map(|i| Task {
            id: format!("d:missing-{i}"),
            name: format!("missing item {i}"),
            code: "invalid".into(),
            wear: Some(2),
            seeds: vec![],
            float: None,
            score: 1.0,
        })
        .collect();
    let before: std::collections::HashMap<String, (f64, u64)> = {
        let mut s = h.lock();
        s.queue.set_tasks(tasks);
        for _ in 0..3 {
            s.queue.next(NOW).unwrap(); // deixa exatamente 0,75 de crédito de descoberta
        }
        assert!(!s.queue.may_discover());
        s.queue
            .take_dirty()
            .into_iter()
            .map(|(id, stat)| (id, (stat.n, stat.queries)))
            .collect()
    };

    let sent = h.steam.sent();
    assert_eq!(tick(&h).await, Tick::Idle);
    assert_eq!(
        h.steam.sent(),
        sent,
        "código inválido falha antes de qualquer requisição"
    );
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), 0);
    assert_eq!(bodies.legacy_queries.load(Ordering::Relaxed), 0);
    assert_eq!(bodies.book_queries.load(Ordering::Relaxed), 0);
    let changed = h.lock().queue.take_dirty();
    assert_eq!(changed.len(), 1);
    let (id, after) = &changed[0];
    assert_eq!(after.queries, before.get(id).map_or(0, |s| s.1));
    assert_eq!(after.n, before.get(id).map_or(0.0, |s| s.0));
    assert!(
        !h.lock().queue.may_discover(),
        "o alvo não ganhou crédito sem amostra"
    );
}

#[tokio::test]
async fn seen_listing_is_reevaluated_when_history_changes() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    let found = run_until_two(&h, &mut rx).await;
    let before = found.iter().find(|o| o.id == ID_CHEAP).unwrap().clone();
    while rx.try_recv().is_ok() {}

    // o histórico do item muda (mediana 10 % menor): a oportunidade ativa é reavaliada, mesma posição
    // na lista, preço de venda menor e nenhum aviso novo
    db.call(|c| {
        c.execute(
            "UPDATE price_points SET median_cents = median_cents * 9 / 10",
            [],
        )?;
        c.execute("UPDATE items SET history_at = history_at - 1", [])
            .map(|_| ())
    })
    .await
    .unwrap();
    let mut updated = None;
    for _ in 0..400 {
        tick(&h).await;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Opportunity(o) = ev
                && o.id == ID_CHEAP
            {
                updated = Some(o);
            }
        }
        if updated.is_some() {
            break;
        }
    }
    let updated = updated.expect("oportunidade reavaliada");
    assert_eq!(updated.seen_at, before.seen_at);
    assert!(updated.sell.mid.price_cents < before.sell.mid.price_cents);

    // sem estimativa possível, a oportunidade ativa deixa de valer
    db.call(|c| {
        c.execute("DELETE FROM price_points", [])?;
        c.execute("UPDATE items SET history_at = history_at - 1", [])
            .map(|_| ())
    })
    .await
    .unwrap();
    let mut gone = vec![];
    for _ in 0..400 {
        tick(&h).await;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Gone { id, .. } = ev {
                gone.push(id);
            }
        }
        if gone.contains(&ID_CHEAP.to_string()) {
            break;
        }
    }
    assert!(gone.contains(&ID_CHEAP.to_string()), "{gone:?}");
}

#[tokio::test]
async fn hourly_listings_include_above_balance_without_counting_repeats() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db.clone());
    h.set_saldo(0, SaldoSource::Manual);
    let task = Task {
        id: "d:x".into(),
        name: MOON.into(),
        code: "G1".into(),
        wear: None,
        seeds: vec![],
        float: None,
        score: 1.0,
    };
    let page = crate::steam::listings::parse_page(&listings_body()["data"], 0).unwrap();
    let expected = page.listings.iter().filter(|l| l.name == MOON).count() as i64;
    assert!(expected > 0);
    detect::process(&h, &task, &page).await.unwrap();
    detect::process(&h, &task, &page).await.unwrap();
    let (listings, opportunities): (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT sum(listings), sum(opportunities) FROM hourly_activity",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!((listings, opportunities), (expected, 0));
    // auditoria: cada anúncio acima do saldo entra uma vez no ciclo, com o que o anúncio trazia
    let (rows, with_seed): (i64, i64) = db
        .call(|c| {
            c.query_row(
                "SELECT count(*), count(seed) FROM evaluations WHERE reason = 'acima_do_saldo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!(rows, expected);
    assert!(with_seed > 0);
}

#[tokio::test]
async fn new_listings_teach_the_agenda_once() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let task = Task {
        id: "d:x".into(),
        name: MOON.into(),
        code: "G1".into(),
        wear: None,
        seeds: vec![],
        float: None,
        score: 1.0,
    };
    let page = crate::steam::listings::parse_page(&listings_body()["data"], 0).unwrap();
    let first = detect::process(&h, &task, &page).await.unwrap();
    assert!(first.rewarded.near_credit > 0.0, "{:?}", first.rewarded);
    // a mesma página de novo: nenhum anúncio é novo, então não rende crédito de novo
    let again = detect::process(&h, &task, &page).await.unwrap();
    assert_eq!(again.rewarded.near_credit, 0.0);
    assert_eq!(again.rewarded.finds, 0);
}

#[tokio::test]
async fn closeness_scale_defaults_then_learns_from_evaluations() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db.clone());
    learn::refresh_scale(&h).await.unwrap();
    assert_eq!(h.lock().closeness_scale, learn::DEFAULT_SCALE);
    // 300 avaliações a R$ 10,00 com lucro no médio de 0, −1, −2 … centavos
    db.call(|c| {
        for k in 0..300_i64 {
            c.execute(
                "INSERT INTO evaluations(at, listing_id, name, price_cents, steam_url, mid_profit_cents, reason)
                 VALUES (0, ?1, 'x', 1000, 'u', ?2, 'r')",
                rusqlite::params![k.to_string(), -k],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    learn::refresh_scale(&h).await.unwrap();
    let mut gaps: Vec<f64> = (0..300_i64)
        .map(|k| (-k - catalog::min_profit(1000)) as f64 / 1000.0)
        .collect();
    let expected = learn::tail_scale(&mut gaps).unwrap();
    let scale = h.lock().closeness_scale;
    assert!((scale - expected).abs() < 1e-12, "{scale} {expected}");
    assert_ne!(scale, learn::DEFAULT_SCALE);
}

#[tokio::test]
async fn empty_pages_counter_saturates() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let task = Task {
        id: "d:x".into(),
        name: MOON.into(),
        code: "G1".into(),
        wear: None,
        seeds: vec![],
        float: None,
        score: 1.0,
    };
    let page = crate::steam::listings::ListingsPage {
        start: 0,
        total_count: 0,
        more: false,
        listings: vec![],
    };
    for _ in 0..300 {
        detect::process(&h, &task, &page).await.unwrap();
    }
    assert_eq!(h.lock().empty_streak["d:x"], 255);
}

#[tokio::test]
async fn cycle_summary_is_plain_and_rolls_every_ten_minutes() {
    use std::sync::atomic::{AtomicI64, Ordering};
    static T: AtomicI64 = AtomicI64::new(NOW);
    fn clock2() -> i64 {
        T.load(Ordering::SeqCst)
    }
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (events, _rx) = broadcast::channel(512);
    let steam = Arc::new(SteamClient::with_base(&base).unwrap());
    let h = Hunt::with_clock(
        HuntDeps {
            settings: SettingsStore::new(db.clone()),
            db,
            steam,
            csfloat: Arc::new(CsfloatClient::with_base(&base).unwrap()),
            events,
            telegram: None,
        },
        clock2,
    );
    let near = |name: &str, mid: i64| detect::Near {
        name: name.into(),
        price_cents: 500,
        mid_cents: mid,
        quick_cents: mid - 30,
    };
    detect::roll_cycle(&h); // abre o ciclo
    // Duas páginas iguais não viram o dobro de anúncios distintos nem de anúncios novos.
    let page = ledger_page(&[("x", "a", 500), ("y", "b", 600)], false);
    ledger::record(&h, "q", &page, NOW).await.unwrap();
    ledger::record(&h, "q", &page, NOW + 1).await.unwrap();
    {
        let mut s = h.lock();
        for (n, m) in [("A", -40), ("B", 12), ("B", 12), ("C", -5), ("D", -300)] {
            s.cycle.record(false, Some(near(n, m)));
        }
        s.cycle.record(false, None);
        s.cycle.refuse("sem_lucro");
        s.cycle.refuse("poucas_vendas");
        s.cycle.refuse("sem_lucro");
        s.cycle.over_saldo.insert("x".into());
        s.cycle.over_saldo.insert("x".into());
        assert_eq!(
            s.cycle.refused_text(),
            "poucas_vendas=1 sem_lucro=2 acima_do_saldo=1"
        );
        let names: Vec<_> = s.cycle.near.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["B", "C", "A"]); // o B repetido entra uma vez só
    }
    h.set_message(None);
    // parcial antes de fechar
    assert!(
        h.state()
            .hunt
            .message
            .unwrap()
            .starts_with("1 min · 2 anúncios únicos · 2 novos")
    );
    T.store(NOW + 600, Ordering::SeqCst);
    detect::roll_cycle(&h);
    assert_eq!(
        h.state().hunt.message.as_deref(),
        Some("10 min · 2 anúncios únicos · 2 novos")
    );
    // o ciclo novo começa zerado e o balanço antigo segue até o próximo fechar
    assert!(
        h.lock()
            .cycle
            .message(NOW + 601)
            .unwrap()
            .starts_with("10 min · 2 anúncios únicos")
    );
    T.store(NOW + 1200, Ordering::SeqCst);
    detect::roll_cycle(&h);
    assert!(
        h.state()
            .hunt
            .message
            .unwrap()
            .starts_with("10 min · 0 anúncios únicos · 0 novos")
    );
    let mut cycle = detect::Cycle::default();
    cycle.pages = 1;
    cycle.seen.insert("only".into());
    cycle.new = 1;
    cycle.record(true, None);
    assert_eq!(
        cycle.message(60).as_deref(),
        Some("1 min · 1 anúncio único · 1 novo")
    );
}

#[tokio::test]
async fn watch_row_for_common_item_has_exact_max_buy() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    run_until_two(&h, &mut rx).await; // deixa o histórico do Moonrise FT no banco
    let task = Task {
        id: format!("d:{MOON}"),
        name: MOON.into(),
        code: "G1".into(),
        wear: None,
        seeds: vec![],
        float: None,
        score: 1.0,
    };
    {
        let mut s = h.lock();
        s.queue.set_tasks(vec![task.clone()]);
        s.floors.clear();
        s.books.clear(); // o livro de teste é de outra skin: não concorre aqui
    }
    // antes da primeira consulta: sem piso nem horário, mas com teto e ritmo
    let w = watch::list(&h).await.unwrap();
    let r = &w[0];
    assert_eq!(
        (r.floor_cents, r.last_checked_at, r.kind),
        (None, None, Kind::Desconto)
    );
    assert!(r.sales_per_day > 100.0 && !r.icon_url.is_empty());
    assert!(r.max_buy_cents.is_some());

    let page = crate::steam::listings::ListingsPage {
        start: 0,
        total_count: 2,
        more: false,
        listings: serde_json::from_value::<Vec<Listing>>(json!([])).unwrap(),
    };
    detect::process(&h, &task, &page).await.unwrap(); // página vazia: consultou, nada visto
    let w = watch::list(&h).await.unwrap();
    assert_eq!((w[0].floor_cents, w[0].last_checked_at), (None, Some(NOW)));

    // o teto é exato: nele ainda entra na lista, um centavo acima não
    let max = w[0].max_buy_cents.unwrap();
    let pts = points_for(&h, MOON, NOW).await.unwrap();
    let est = sell::estimate(&pts, NOW, None, None).unwrap();
    assert!(sell::evaluate(max, &est, 0).listable);
    assert!(!sell::evaluate(max + 1, &est, 0).listable);

    // pedido de compra: 1 centavo acima do maior pedido, só se ainda dá lucro e cabe no saldo
    let book_at = |at: i64, top: i64, ask: Option<i64>| {
        (
            at,
            Some(crate::steam::book::Book {
                max_buy_cents: Some(top),
                buy_orders: 5,
                min_sell_cents: ask,
                sell_orders: i64::from(ask.is_some()),
                sell_levels: ask.map(|a| vec![(a, 1)]).unwrap_or_default(),
            }),
        )
    };
    let book = |top| book_at(NOW, top, None);
    let top = max / 2;
    h.lock().books.insert(MOON.into(), book(top));
    let r = watch::list(&h).await.unwrap().remove(0);
    let ev = sell::evaluate(top + 1, &est, 0);
    assert!(ev.ev_profit_cents.unwrap() > 0);
    assert_eq!(
        (r.bid_cents, r.bid_profit_cents, r.book_at),
        (Some(top + 1), Some(ev.sell.mid.profit_cents), Some(NOW))
    );
    h.lock().books.insert(MOON.into(), book(max));
    let r = watch::list(&h).await.unwrap().remove(0);
    assert_eq!((r.bid_cents, r.book_at), (None, Some(NOW)));
    // o menor anúncio do próprio livro também limita: pedido igual a ele não é sugerido
    h.lock()
        .books
        .insert(MOON.into(), book_at(NOW, top, Some(top + 1)));
    assert_eq!(watch::list(&h).await.unwrap()[0].bid_cents, None);
    // e entra na revenda: anúncio logo acima do pedido derruba o lucro (o histórico sozinho prometia mais)
    h.lock()
        .books
        .insert(MOON.into(), book_at(NOW, top, Some(top + 2)));
    assert_eq!(watch::list(&h).await.unwrap()[0].bid_cents, None);
    // livro velho ou leitura que falhou: sem sugestão
    let old = NOW - orders::STALE - 1;
    h.lock().books.insert(MOON.into(), book_at(old, top, None));
    assert_eq!(watch::list(&h).await.unwrap()[0].bid_cents, None);
    h.lock().books.insert(MOON.into(), (NOW, None));
    assert_eq!(watch::list(&h).await.unwrap()[0].bid_cents, None);
    h.lock().books.insert(MOON.into(), book(top));
    h.set_saldo(top, SaldoSource::Manual);
    assert_eq!(watch::list(&h).await.unwrap()[0].bid_cents, None);
}

#[tokio::test]
async fn learned_scores_survive_a_restart_and_show_in_the_watch_row() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    run_until_two(&h, &mut rx).await; // histórico do Moonrise no banco e tarefas na agenda
    // as consultas reais do teste acharam as duas oportunidades; mais 30 consultas tiram o "novo"
    let id = format!("d:{MOON}");
    {
        let mut s = h.lock();
        let only = s
            .queue
            .tasks()
            .into_iter()
            .find(|t| t.id == id)
            .expect("Moonrise na agenda");
        s.queue.set_tasks(vec![only]);
        for _ in 0..30 {
            s.queue.next(NOW);
        }
    }
    let w = watch::list(&h).await.unwrap();
    let row = w
        .iter()
        .find(|r| r.name == MOON)
        .expect("Moonrise na agenda");
    assert!(row.reason.starts_with("rendeu R$ "), "{}", row.reason);
    assert!(row.score > 0.0 && row.mode == "barato");
    scheduler::save_stats(&h).await.unwrap();

    // reinício: outra caça sobre o mesmo banco recupera a nota
    let (h2, _rx2) = hunt_against(&base, db);
    h2.load().await.unwrap();
    let (score, why) = h2.lock().queue.describe(&id, NOW);
    assert_eq!(why, row.reason);
    assert!(score > 0.0);
}

#[tokio::test]
async fn combo_uses_positive_expected_profit_including_rare_without_daily_score() {
    let (base, _) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(3608, SaldoSource::Manual);
    let found = run_until_two(&h, &mut rx).await;
    let mut high_mid = found[0].clone();
    high_mid.id = "high_mid".into();
    high_mid.price_cents = 10_000;
    high_mid.sell.mid.profit_cents = 3000;
    high_mid.ev_profit_cents = Some(-1023);
    let mut rare = found[1].clone();
    rare.id = "rare".into();
    rare.price_cents = 10_000;
    rare.kind = Kind::Raro;
    rare.profit_per_day_cents = None;
    rare.ev_profit_cents = Some(100);
    let mut better = rare.clone();
    better.id = "better".into();
    better.ev_profit_cents = Some(200);
    better.sell.mid.profit_cents = 1;
    {
        let mut s = h.lock();
        s.active.clear();
        for opp in [high_mid.clone(), rare.clone(), better.clone()] {
            s.active.insert(
                opp.id.clone(),
                Active {
                    opp,
                    task: "test".into(),
                    attempted_at: 0,
                },
            );
        }
    }
    h.set_saldo(10_000, SaldoSource::Manual);
    h.recompute_combo();
    assert_eq!(h.state().combo.ids, vec!["better"]);
    assert_eq!(h.state().combo.profit_cents, 200);
    {
        let mut s = h.lock();
        s.active.remove("better");
    }
    h.recompute_combo();
    assert_eq!(h.state().combo.ids, vec!["rare"]);
    {
        let mut s = h.lock();
        s.active.get_mut("rare").unwrap().opp.ev_profit_cents = None;
    }
    h.recompute_combo();
    assert!(h.state().combo.ids.is_empty());
}

#[tokio::test]
async fn activity_table_failure_does_not_stop_detection() {
    let (base, _) = serve().await;
    let (db, _dir) = temp_db();
    db.call(|c| c.execute("DROP TABLE hourly_activity", []).map(|_| ()))
        .await
        .unwrap();
    let (h, mut rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    let found = run_until_two(&h, &mut rx).await;
    assert_eq!(found.len(), 2);
    let count: i64 = db
        .call(|c| {
            c.query_row(
                "SELECT count(*) FROM opportunities WHERE gone_at IS NULL",
                [],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn no_saldo_message_clears_when_saldo_arrives() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    h.set_saldo(0, SaldoSource::Manual);
    assert_eq!(tick(&h).await, Tick::Idle);
    assert!(
        h.lock()
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("sem saldo"))
    );
    // Steam em pausa: a descoberta fica esperando o limitador, e o aviso já tem que ter saído
    h.steam
        .restore_pace(&format!("{}|6000|60000", crate::db::now_unix() + 600));
    h.set_saldo(3608, SaldoSource::Steam);
    while rx.try_recv().is_ok() {}
    let _ = tokio::time::timeout(Duration::from_millis(300), tick(&h)).await;
    let no_saldo = |m: &Option<String>| m.as_deref().is_some_and(|m| m.starts_with("sem saldo"));
    assert!(!no_saldo(&h.lock().message));
    // e o painel recebeu um estado sem o aviso
    let mut sent = false;
    while let Ok(ev) = rx.try_recv() {
        if let Event::State(s) = ev {
            sent = !no_saldo(&s.hunt.message);
        }
    }
    assert!(sent);
}

#[tokio::test]
async fn catalog_scan_is_marked_only_after_every_round() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db.clone());
    h.set_saldo(3608, SaldoSource::Manual);
    let marked = || catalog::get_setting(&db, catalog::SCAN_KEY);
    let mut steps = 0;
    while catalog::step(&h).await.unwrap() {
        // marcar no meio faria um reinício pular o resto da varredura por 24 h
        assert_eq!(marked().await.unwrap(), None, "marcada no passo {steps}");
        steps += 1;
        assert!(steps < 2000);
    }
    assert!(marked().await.unwrap().is_some());
}

#[tokio::test]
async fn orders_read_one_book_per_gap_oldest_common_first() {
    let (base, _bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against(&base, db);
    let task = |name: &str, seeds: Vec<(u16, u16)>| Task {
        id: format!("d:{name}"),
        name: name.into(),
        code: "G1".into(),
        wear: None,
        seeds,
        float: None,
        score: 1.0,
    };
    h.lock().queue.set_tasks(vec![
        task("A", vec![]),
        task("B", vec![]),
        task("R", vec![(1, 2)]),
    ]);
    let fresh = |at| (at, None);
    // ligou agora: o primeiro livro espera a vaga
    assert!(!orders::maybe_step(&h).await);
    h.lock().book_at = NOW - 61;
    h.lock().books.insert("A".into(), fresh(NOW - 3600));
    h.lock().books.insert("B".into(), fresh(NOW - 1800));
    assert!(orders::maybe_step(&h).await); // A, o mais velho
    assert_eq!(h.lock().book_at, NOW);
    assert_eq!(h.lock().books["A"].0, NOW); // lido: A vai para o fim da fila
    assert!(h.lock().books["A"].1.is_some());
    assert!(!orders::maybe_step(&h).await); // dentro da vaga
    h.lock().book_at = NOW - 61;
    assert!(orders::maybe_step(&h).await); // agora B
    assert_eq!(h.lock().books["B"].0, NOW);
    h.lock().book_at = NOW - 61;
    assert!(!orders::maybe_step(&h).await); // tudo novo; o raro nunca entra
}

#[tokio::test]
async fn learned_pattern_listing_is_priced_by_the_sales_that_taught_it() {
    const ID_LEARNED: &str = "341565526995940199"; // seed 269: fora das tabelas do Moonrise
    let (base, bodies) = serve().await;
    {
        let mut b = bodies.listings.lock().unwrap();
        let l = &mut b["data"]["listings"][14];
        l["unPrice"] = json!(1044);
        l["unFee"] = json!(156);
    }
    let (db, _dir) = temp_db();
    // três vendas guardadas pelo coletor (dois desgastes): sobrevivem a reinício pelo banco
    db.call(|c| {
        for (name, ratio, at) in [
            (MOON, 2.0, 1),
            (MOON, 2.4, 2),
            ("Glock-18 | Moonrise (Minimal Wear)", 2.2, 3),
        ] {
            c.execute(
                "INSERT INTO learned_sales(name, seed, ratio, sold_at) VALUES (?1, 269, ?2, ?3)",
                rusqlite::params![name, ratio, at],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let (h, mut rx) = hunt_against(&base, db);
    h.load().await.unwrap();
    assert_eq!(
        patterns::active(&h.lock().learned, "Glock-18 | Moonrise"),
        vec![269]
    );
    h.set_saldo(3608, SaldoSource::Manual);
    let mut hit = None;
    for _ in 0..400 {
        tick(&h).await;
        while let Ok(ev) = rx.try_recv() {
            if let Event::Opportunity(o) = ev
                && o.id == ID_LEARNED
            {
                hit = Some(o);
            }
        }
        if hit.is_some() {
            break;
        }
    }
    // R$ 12,00 no FT comum de ~R$ 11,6 não dá lucro; o pattern aprendido vendeu a 2,0–2,4×
    let o = hit.expect("pattern aprendido vira oportunidade");
    assert_eq!(
        (o.kind, o.group.as_deref()),
        (Kind::Raro, Some("Pattern #269"))
    );
    assert!(
        o.evidence.contains("grupo raro: 3 vendas"),
        "{}",
        o.evidence
    );
    assert!(o.sell.mid.profit_cents >= 100, "{:?}", o.sell);
    // e o seed entra na busca rara do Moonrise
    let tasks = h.lock().queue.tasks();
    assert!(
        tasks
            .iter()
            .any(|t| t.seeds.iter().any(|&(lo, hi)| (lo..=hi).contains(&269)))
    );
}

// ---------------------------------------------------------------------------------------------
// Registro de observações (mina de ouro, Fase 0)

/// Página com anúncios do Moonrise da captura real, renomeados: `(id, item, preço)`.
fn ledger_page(rows: &[(&str, &str, i64)], more: bool) -> crate::steam::listings::ListingsPage {
    let base = crate::steam::listings::parse_page(
        &fixture("steam_listings_moonrise_sorted_price.json")["data"],
        0,
    )
    .unwrap();
    let mut page = base.clone();
    page.more = more;
    page.listings = rows
        .iter()
        .map(|&(id, asset, price)| {
            let mut l = base.listings[0].clone();
            (l.listing_id, l.asset_id, l.price_cents) = (id.into(), asset.into(), price);
            l
        })
        .collect();
    page
}

async fn obs(db: &Db, id: &str) -> (Option<i64>, i64, Option<i64>, Option<String>, bool) {
    let id = id.to_string();
    db.call(move |c| {
        c.query_row(
            "SELECT gone_at, relisted, age_max, relist_of, cheapest FROM observations WHERE listing_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn ledger_marks_gone_only_when_covered() {
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against("http://127.0.0.1:9", db.clone());
    let t0 = NOW;
    let all = [
        ("L0", "A0", 100),
        ("L1", "A1", 200),
        ("L2", "A2", 300),
        ("L3", "A3", 400),
        ("L4", "A4", 500),
    ];
    let r = ledger::record(&h, "q", &ledger_page(&all, true), t0)
        .await
        .unwrap();
    assert_eq!((r.new, r.aged, r.gone), (5, 0, 0));
    assert!(obs(&db, "L0").await.4, "o mais barato da página");
    assert!(!obs(&db, "L1").await.4);
    // L0 (R$ 1) sumiu e a página ainda cobre o preço dele
    let r = ledger::record(&h, "q", &ledger_page(&all[1..], true), t0 + 60)
        .await
        .unwrap();
    assert_eq!(r.gone, 1);
    assert_eq!(obs(&db, "L0").await.0, Some(t0 + 60));
    // L4 está no maior preço da página anterior: com `more`, a página nova (até R$ 4) não o cobre
    let r = ledger::record(&h, "q", &ledger_page(&all[1..4], true), t0 + 120)
        .await
        .unwrap();
    assert_eq!(r.gone, 0);
    // página vazia nunca prova sumiço
    let r = ledger::record(&h, "q", &ledger_page(&[], false), t0 + 180)
        .await
        .unwrap();
    assert_eq!(r, ledger::Recorded::default());
    assert_eq!(obs(&db, "L2").await.0, None);
    // outra consulta não prova nada sobre esta
    let r = ledger::record(&h, "outra", &ledger_page(&all[1..2], false), t0 + 200)
        .await
        .unwrap();
    assert_eq!((r.new, r.gone), (0, 0));
    // sem `more` a página cobre tudo: L2, L3 e L4 sumiram
    let r = ledger::record(&h, "q", &ledger_page(&all[1..2], false), t0 + 240)
        .await
        .unwrap();
    assert_eq!(r.gone, 3);
    // e quem volta (inversão de ordem, leitura parcial) está vivo de novo
    ledger::record(&h, "q", &ledger_page(&all[1..3], false), t0 + 300)
        .await
        .unwrap();
    assert_eq!(obs(&db, "L2").await.0, None);
}

#[tokio::test]
async fn ledger_relist_by_asset_is_not_a_sale_and_age_comes_from_the_cover() {
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against("http://127.0.0.1:9", db.clone());
    let t0 = NOW;
    ledger::record(
        &h,
        "q",
        &ledger_page(&[("L0", "A0", 100), ("L1", "A1", 200)], true),
        t0,
    )
    .await
    .unwrap();
    // o mesmo item A0 volta mais barato em outro anúncio: relistagem; um novo coberto pela consulta anterior
    // (R$ 1,50 < 98 % de R$ 2,00) tem idade ≤ 2 min; um acima do que ela cobria, idade desconhecida
    let p2 = [
        ("L1", "A1", 200),
        ("L9", "A0", 90),
        ("L5", "A5", 150),
        ("L6", "A6", 300),
    ];
    let r = ledger::record(&h, "q", &ledger_page(&p2, false), t0 + 120)
        .await
        .unwrap();
    assert_eq!((r.new, r.aged, r.relisted, r.gone), (3, 2, 1, 0));
    let (gone, relisted, ..) = obs(&db, "L0").await;
    assert_eq!((gone, relisted), (Some(t0 + 120), 1));
    let (_, _, age, of, _) = obs(&db, "L9").await;
    assert_eq!((age, of.as_deref()), (Some(120), Some("L0")));
    assert_eq!(obs(&db, "L5").await.2, Some(120));
    assert_eq!(obs(&db, "L6").await.2, None);
    // item que volta depois de 24 h sumido já conta como venda: não vira relistagem
    ledger::record(&h, "q", &ledger_page(&[("L6", "A6", 300)], false), t0 + 180)
        .await
        .unwrap();
    let late = t0 + 180 + ledger::RELIST_WINDOW + 1;
    let r = ledger::record(
        &h,
        "q",
        &ledger_page(&[("L7", "A5", 150), ("L6", "A6", 300)], false),
        late,
    )
    .await
    .unwrap();
    assert_eq!((r.new, r.relisted), (1, 0));
    assert_eq!(obs(&db, "L5").await.1, 0);
}

#[tokio::test]
async fn book_snapshot_only_on_change() {
    let (db, _dir) = temp_db();
    let (h, _rx) = hunt_against("http://127.0.0.1:9", db.clone());
    let b = crate::steam::book::parse_book(&fixture("steam_orderbook_negev_2026-11.json")["data"])
        .unwrap();
    assert_eq!(
        orders::remember_book(&h, MOON, NOW, Some(b.clone()), false).await,
        None
    );
    // mesmo topo (só a contagem de pedidos de compra mudou): não grava de novo
    let more_bids = crate::steam::book::Book {
        buy_orders: b.buy_orders + 3,
        ..b.clone()
    };
    assert!(
        orders::remember_book(&h, MOON, NOW + 60, Some(more_bids), false)
            .await
            .is_some()
    );
    // falha de leitura não grava e não apaga o que já se sabia
    orders::remember_book(&h, MOON, NOW + 90, None, false).await;
    let mut sold = b.clone();
    sold.sell_levels[0].1 = 1; // um dos dois anúncios de R$ 1,67 vendeu
    orders::remember_book(&h, MOON, NOW + 120, Some(sold), false).await;
    let rows: Vec<(i64, String)> = db
        .call(|c| {
            c.prepare("SELECT at, sell_levels FROM book_snapshots ORDER BY at")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1].0, NOW + 120);
    assert!(rows[1].1.starts_with("[[167,1],[170,1]"), "{}", rows[1].1);
}
