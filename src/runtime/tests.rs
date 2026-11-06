//! Runtime contra um servidor local que imita a Steam (`/market/` logado, `buylisting`) e o Telegram.
//! Nenhuma compra real: o `buylisting` responde com as formas documentadas.

use super::*;
use crate::csfloat::CsfloatClient;
use crate::db::temp_db;
use crate::hunt::HuntDeps;
use crate::web::api::SettingsInput;
use axum::Router;
use axum::extract::State as AxState;
use axum::routing::{get, post};
use serde_json::json;

const ID: &str = "6624491228774232473";
const NAME: &str = "Glock-18 | Moonrise (Field-Tested)";
const PAGE: &str = r#"<html><script>
g_steamID = "76561198000000001"; g_bLoggedIn = true; g_sessionID = "sess123";
g_rgWalletInfo = {"wallet_balance":"3608","wallet_currency":7};
</script></html>"#;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Respostas do `buylisting`, uma por chamada; a última se repete.
struct Fake {
    buy: Mutex<Vec<(u16, String)>>,
    buys: Mutex<u32>,
}

async fn serve(responses: Vec<(u16, String)>) -> (String, Arc<Fake>) {
    let f = Arc::new(Fake {
        buy: Mutex::new(responses),
        buys: Mutex::new(0),
    });
    let app = Router::new()
        .route("/market/", get(|| async { PAGE }))
        .route(
            "/market/buylisting/{id}",
            post(|AxState(f): AxState<Arc<Fake>>| async move {
                *f.buys.lock().unwrap() += 1;
                let mut v = f.buy.lock().unwrap();
                let (code, body) = if v.len() > 1 {
                    v.remove(0)
                } else {
                    v[0].clone()
                };
                (StatusCode::from_u16(code).unwrap(), body)
            }),
        )
        .route(
            "/bot{token}/getMe",
            get(|| async { json!({"ok": true, "result": {"username": "fakebot"}}).to_string() }),
        )
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, f)
}

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

fn input(f: impl FnOnce(&mut SettingsInput)) -> SettingsInput {
    let mut i = SettingsInput::default();
    f(&mut i);
    i
}

async fn insert_opportunity(rt: &Runtime) {
    let tier = |p: i64| {
        json!({"price_cents": p, "net_cents": p * 85 / 100,
               "profit_cents": p * 85 / 100 - 400, "eta_hours": null})
    };
    let opp = json!({
        "id": ID, "name": NAME, "icon_url": "https://i/x.png",
        "steam_url": "https://s/x", "inspect_url": null, "wear": "Field-Tested", "float": 0.2,
        "seed": 151, "group": null, "kind": "desconto", "price_cents": 400,
        "sell": {"high": tier(591), "mid": tier(578), "quick": tier(561)},
        "margin_pct": 25.75, "safe": true, "confidence": "alta", "evidence": "x", "seen_at": 1
    });
    let listing = json!({
        "listing_id": ID, "name": NAME, "price_cents": 400,
        "seller_cents": 348, "fee_cents": 52, "currency": 7, "float": 0.2, "seed": 151,
        "paint_index": null, "icon_url": "https://i/x.png", "inspect_url": null,
        "stickers": [], "keychains": [], "market_tradable_restriction": null,
        "market_marketable_restriction": null
    });
    let (o, l) = (opp.to_string(), listing.to_string());
    rt.db
        .call(move |c| {
            c.execute(
                "INSERT INTO opportunities(id, json, created_at, listing_json) VALUES (?1, ?2, 1, ?3)",
                params![ID, o, l],
            )?;
            c.execute(
                "INSERT INTO price_points(name, ts, median_cents, purchases) VALUES (?1, ?2, 590, 12)",
                params![NAME, now_unix() - 3600],
            )
            .map(|_| ())
        })
        .await
        .unwrap();
}

async fn set_cookie(rt: &Arc<Runtime>) {
    rt.settings
        .apply(input(|i| {
            i.steam_login_secure = Some(Some("76561198000000001%7C%7Cabcdef".into()));
        }))
        .await
        .unwrap();
    rt.apply_local().await.unwrap();
    rt.verify_session().await;
}

async fn purchase_row(rt: &Runtime) -> Purchase {
    let j: String = rt
        .db
        .call(|c| {
            c.query_row("SELECT json FROM purchases WHERE id = ?1", [ID], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    serde_json::from_str(&j).unwrap()
}

#[tokio::test]
async fn settings_drive_state_and_balance() {
    let (base, _f) = serve(vec![(200, "{}".into())]).await;
    let (rt, _rx, _d) = runtime(&base);
    assert_eq!(rt.hunt.state().saldo_source, SaldoSource::None);

    set_cookie(&rt).await;
    let s = rt.hunt.state();
    assert_eq!((s.saldo_cents, s.saldo_source), (3608, SaldoSource::Steam));
    assert_eq!((s.keys.steam.set, s.keys.steam.ok), (true, Some(true)));

    // saldo manual vence a carteira; apagar volta à carteira
    rt.settings.set_saldo(Some(1500)).await.unwrap();
    rt.apply_local().await.unwrap();
    assert_eq!(rt.hunt.state().saldo_cents, 1500);
    rt.settings.set_saldo(None).await.unwrap();
    rt.apply_local().await.unwrap();
    assert_eq!(rt.hunt.state().saldo_cents, 3608);

    // Telegram verificado pelo getMe
    rt.settings
        .apply(input(|i| {
            i.telegram_token = Some(Some("123:abc".into()));
            i.telegram_chat_id = Some(Some("42".into()));
        }))
        .await
        .unwrap();
    rt.apply_local().await.unwrap();
    rt.verify_telegram().await;
    let k = rt.hunt.state().keys;
    assert_eq!(
        (k.telegram.ok, k.telegram.detail.as_deref()),
        (Some(true), Some("@fakebot"))
    );
    // cookie colado com o prefixo do navegador
    assert_eq!(clean_cookie(" steamLoginSecure=abc; "), "abc");

    // cookie apagado: sem carteira, saldo zera
    rt.settings
        .apply(input(|i| i.steam_login_secure = Some(None)))
        .await
        .unwrap();
    rt.apply_local().await.unwrap();
    let s = rt.hunt.state();
    assert_eq!((s.saldo_cents, s.keys.steam.set), (0, false));
}

#[tokio::test]
async fn buy_success_records_purchase_and_removes_listing() {
    let (base, f) = serve(vec![(
        200,
        fixture("steam_buy_success_documented_form.json"),
    )])
    .await;
    let (rt, mut rx, _d) = runtime(&base);
    insert_opportunity(&rt).await;
    rt.settings.set_saldo(Some(3608)).await.unwrap();
    set_cookie(&rt).await;

    let p = rt.buy(ID).await.ok().unwrap();
    assert_eq!(p.status, PurchaseStatus::Bought);
    assert_eq!(p.tradable_at, p.bought_at.map(|t| t + 7 * 86_400));
    assert_eq!(p.sell.mid.price_cents, 578); // previsão de venda guardada
    assert_eq!(purchase_row(&rt).await.status, PurchaseStatus::Bought);
    // o anúncio sai da lista e o saldo manual desconta o que foi pago
    let gone: Option<i64> = rt
        .db
        .call(|c| {
            c.query_row(
                "SELECT gone_at FROM opportunities WHERE id = ?1",
                [ID],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert!(gone.is_some());
    assert_eq!(rt.hunt.state().saldo_cents, 3208);
    let mut saw_purchase = false;
    while let Ok(ev) = rx.try_recv() {
        saw_purchase |= matches!(ev, Event::Purchase(_));
    }
    assert!(saw_purchase);
    // não compra de novo
    assert_eq!(rt.buy(ID).await.err().unwrap().0, StatusCode::NOT_FOUND);
    assert_eq!(*f.buys.lock().unwrap(), 1);
}

#[tokio::test]
async fn buy_needs_confirmation_then_completes_in_background() {
    let (base, _f) = serve(vec![
        (
            200,
            fixture("steam_buy_need_confirmation_documented_form.json"),
        ),
        (200, fixture("steam_buy_success_documented_form.json")),
    ])
    .await;
    let (rt, mut rx, _d) = runtime(&base);
    insert_opportunity(&rt).await;
    set_cookie(&rt).await;

    let p = rt.buy(ID).await.ok().unwrap();
    assert_eq!(p.status, PurchaseStatus::NeedConfirmation);
    assert!(p.message.unwrap().contains("app da Steam"));
    // esperando no celular, outra compra não passa nem usa o mesmo saldo
    let busy = rt.buy(ID).await.err().unwrap();
    assert_eq!(busy.0, StatusCode::CONFLICT);
    assert!(busy.1.contains("termine a compra em andamento primeiro"));
    // o reenvio acontece 2 s depois, sem segurar a requisição
    let done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::Purchase(p)) = rx.recv().await
                && p.status == PurchaseStatus::Bought
            {
                break p;
            }
        }
    })
    .await
    .expect("compra confirmada");
    assert!(done.bought_at.is_some());
    assert_eq!(purchase_row(&rt).await.status, PurchaseStatus::Bought);
    // terminado, a trava solta: a próxima tentativa chega à checagem normal (anúncio já saiu da lista)
    let mut next = None;
    for _ in 0..50 {
        next = rt.buy(ID).await.err();
        if next.as_ref().is_some_and(|e| e.0 != StatusCode::CONFLICT) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(next.unwrap().0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uncertain_purchase_says_so_and_is_not_repeated() {
    let (base, f) = serve(vec![(500, "oops".into())]).await;
    let (rt, _rx, _d) = runtime(&base);
    insert_opportunity(&rt).await;
    set_cookie(&rt).await;
    let p = rt.buy(ID).await.ok().unwrap();
    assert_eq!(p.status, PurchaseStatus::Pending);
    assert_eq!(p.message.as_deref(), Some(UNCERTAIN));
    // incerto não repete o pedido às cegas
    let q = rt.buy(ID).await.ok().unwrap();
    assert_eq!(q.status, PurchaseStatus::Pending);
    assert_eq!(*f.buys.lock().unwrap(), 1);
}

#[tokio::test]
async fn buy_guards_and_history() {
    let (base, f) = serve(vec![(200, "{}".into())]).await;
    let (rt, _rx, _d) = runtime(&base);
    assert_eq!(rt.buy("nope").await.err().unwrap().0, StatusCode::NOT_FOUND);
    insert_opportunity(&rt).await;
    // sem cookie não há compra e nada sai pela rede
    assert_eq!(rt.buy(ID).await.err().unwrap().0, StatusCode::BAD_REQUEST);
    assert_eq!(*f.buys.lock().unwrap(), 0);

    let h = rt.history(ID).await.ok().unwrap();
    assert_eq!(h.points.len(), 1);
    assert_eq!((h.points[0].1, h.points[0].2), (590, 12));
    assert_eq!(h.sell.quick.price_cents, 561);
    assert_eq!(
        rt.history("nope").await.err().unwrap().0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn cached_wallet_stands_in_while_live_read_is_blocked() {
    let (base, _f) = serve(vec![(200, "{}".into())]).await;
    let (rt, _rx, _d) = runtime(&base);
    let cache = |id: &str, age: i64| format!("{id},1234,7,{}", now_unix() - age);
    let start = |rt: Arc<Runtime>, c: String| async move {
        rt.settings
            .apply(input(|i| {
                i.steam_login_secure = Some(Some("76561198000000001%7C%7Cabcdef".into()));
            }))
            .await
            .unwrap();
        rt.settings
            .set_raw(WALLET_CACHE_KEY, Some(c))
            .await
            .unwrap();
        rt.apply_local().await.unwrap(); // sem rede: a leitura ao vivo ainda não aconteceu
        let s = rt.hunt.state();
        (s.saldo_cents, s.saldo_source)
    };
    // reinício logo após a leitura: o saldo guardado vale
    let fresh = start(rt.clone(), cache("76561198000000001", 600)).await;
    assert_eq!(fresh, (1234, SaldoSource::Steam));
    // velho demais ou de outra conta: não vale
    let old = start(rt.clone(), cache("76561198000000001", 7 * 3600)).await;
    assert_eq!(old, (0, SaldoSource::None));
    let other = start(rt.clone(), cache("76561198999999999", 600)).await;
    assert_eq!(other, (0, SaldoSource::None));
    // o manual continua mandando
    rt.settings.set_saldo(Some(1500)).await.unwrap();
    rt.settings
        .set_raw(WALLET_CACHE_KEY, Some(cache("76561198000000001", 60)))
        .await
        .unwrap();
    rt.apply_local().await.unwrap();
    assert_eq!(rt.hunt.state().saldo_cents, 1500);
    rt.settings.set_saldo(None).await.unwrap();
    // a leitura ao vivo substitui o guardado e o regrava
    rt.verify_session().await;
    assert_eq!(rt.hunt.state().saldo_cents, 3608);
    assert!(
        rt.settings
            .get(WALLET_CACHE_KEY)
            .await
            .unwrap()
            .unwrap()
            .contains(",3608,7,")
    );
}
