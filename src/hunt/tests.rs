//! Teste de ponta a ponta: um servidor HTTP local (axum) serve as capturas reais no lugar da Steam e do
//! CSFloat, e a caça roda do banco vazio até o evento `opportunity`. É teste; não entra no produto.
//!
//! As capturas reais são usadas como estão, com estas alterações feitas no teste (a captura de 03/11 não
//! tem anúncio com lucro): o primeiro anúncio da página de listagens passa a custar R$ 4,00, o segundo
//! recebe seed 59 (tier 1 do Moonrise) e custa R$ 7,00, e a primeira entrada da busca vira o Moonrise FT
//! a US$ 1,00. No CSFloat, três vendas sem adesivo ganham seeds do tier 1 e preço ×1,5.

use super::scheduler::{Tick, tick};
use super::*;
use crate::db::temp_db;
use crate::steam::listings::Listing;
use crate::web::api::{Confidence, Kind};
use axum::extract::{Path, Query, State as AxState};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;

const NOW: i64 = 1_793_754_060; // um minuto depois do último ponto da captura de histórico
const MOON: &str = "Glock-18 | Moonrise (Field-Tested)";
const ID_CHEAP: &str = "6624491228774232473";
const ID_T1: &str = "8709556594461768769";

fn clock() -> i64 {
    NOW
}

fn fixture(name: &str) -> Value {
    let p = format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn listings_body() -> Value {
    let mut v = fixture("steam_listings_moonrise_sorted_price.json");
    let l = &mut v["data"]["listings"];
    l[0]["unPrice"] = json!(348);
    l[0]["unFee"] = json!(52);
    l[1]["unPrice"] = json!(609);
    l[1]["unFee"] = json!(91);
    l[1]["asset"]["asset_properties"][0]["int_value"] = json!("59");
    v
}

fn search_body() -> Value {
    let mut v = fixture("steam_search_render_pistol_p1900.json");
    v["results"][0]["hash_name"] = json!(MOON);
    v["results"][0]["name"] = json!(MOON);
    v["results"][0]["sell_price"] = json!(100);
    v
}

fn csfloat_body() -> Value {
    let mut v = fixture("csfloat_sales_moonrise_ft.json");
    for (i, seed) in [(0, 58), (2, 224), (3, 601)] {
        let price = v[i]["price"].as_i64().unwrap();
        v[i]["price"] = json!(price * 3 / 2);
        v[i]["item"]["paint_seed"] = json!(seed);
    }
    v
}

struct Bodies {
    listings: Mutex<Value>,
    history: Value,
    search: Value,
    csfloat: Value,
}

async fn serve() -> (String, Arc<Bodies>) {
    let b = Arc::new(Bodies {
        listings: Mutex::new(listings_body()),
        history: fixture("steam_pricehistory_moonrise_ft.json"),
        search: search_body(),
        csfloat: csfloat_body(),
    });
    async fn actions(
        AxState(b): AxState<Arc<Bodies>>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        match q.get("q").map(String::as_str) {
            Some("QueryListingsForItem") => Json(b.listings.lock().unwrap().clone()),
            Some("QueryPriceHistory") => Json(b.history.clone()),
            other => panic!("consulta inesperada: {other:?}"),
        }
    }
    let app = Router::new()
        .route("/market/actions", get(actions))
        .route(
            "/market/search/render/",
            get(|AxState(b): AxState<Arc<Bodies>>| async move { Json(b.search.clone()) }),
        )
        .route(
            "/history/{name}/sales",
            get(
                |AxState(b): AxState<Arc<Bodies>>, Path(_n): Path<String>| async move {
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
        },
        clock,
    );
    (h, rx)
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

    // razão medida: R$ 4,00 (menor anúncio) ÷ US$ 1,00
    assert_eq!(h.lock().ratio.map(|r| r.0), Some(4.0));

    let (cheap, t1) = (&found[0], &found[1]);
    assert_eq!((cheap.id.as_str(), cheap.price_cents), (ID_CHEAP, 400));
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
    // números conferidos à mão: mediana ~R$ 5,9 com queda de 2 %; 5,78 → líquido 5,03 (taxa 15 %)
    let tiers = |o: &Opportunity| {
        [&o.sell.quick, &o.sell.mid, &o.sell.high]
            .map(|t| (t.price_cents, t.net_cents, t.profit_cents))
    };
    assert_eq!(
        tiers(cheap),
        [(561, 489, 89), (578, 503, 103), (591, 515, 115)]
    );
    assert_eq!((cheap.margin_pct, cheap.safe), (25.75, true));
    assert_eq!(cheap.confidence, Confidence::Alta);
    assert_eq!(
        cheap.evidence,
        "339 vendas/dia nos últimos 7 dias · preço em queda, descontado 2%"
    );
    assert_eq!(tiers(t1), [(854, 743, 43), (887, 772, 72), (911, 793, 93)]);
    assert!((t1.margin_pct - 10.2857).abs() < 1e-3 && t1.safe);
    assert_eq!(t1.confidence, Confidence::Media);
    for o in &found {
        assert_eq!(o.name, MOON);
        assert!(o.steam_url.contains(&format!("detail={}", o.id)));
        assert!(o.sell.mid.profit_cents >= 20 && o.margin_pct >= 5.0);
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
        (ID_CHEAP, 400, 348)
    );
    assert!(task.starts_with("d:") || task.starts_with("r:"));

    // combo com os dois anúncios (R$ 11,00 ≤ R$ 36,08)
    let st = h.state();
    assert_eq!(st.combo.cost_cents, 1100);
    assert_eq!(st.combo.ids.len(), 2);
    assert!(st.hunt.watching >= 10);

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
            if let Event::Gone { id } = ev {
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
}
