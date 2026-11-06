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
    search: Mutex<Value>,
    listing_queries: AtomicUsize,
    csfloat: Value,
    /// Livro de ofertas (captura real do Negev; o menor anúncio é trocado pelos testes).
    book: Mutex<Value>,
    book_queries: AtomicUsize,
    /// A leitura de anúncios responde 429 (bloqueio de horas da Steam).
    listings_429: AtomicBool,
}

async fn serve() -> (String, Arc<Bodies>) {
    let b = Arc::new(Bodies {
        listings: Mutex::new(listings_body()),
        history: history_body(),
        search: Mutex::new(search_body()),
        listing_queries: AtomicUsize::new(0),
        csfloat: csfloat_body(),
        book: Mutex::new(fixture("steam_orderbook_negev_2026-11.json")),
        book_queries: AtomicUsize::new(0),
        listings_429: AtomicBool::new(false),
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
            Some("QueryPriceHistory") => Json(b.history.clone()).into_response(),
            other => panic!("consulta inesperada: {other:?}"),
        }
    }
    let app = Router::new()
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
                Json(b.search.lock().unwrap().clone())
            }),
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
            telegram: None,
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
    // mais perto de compensar primeiro
    let gaps: Vec<i64> = watch
        .iter()
        .filter_map(|r| r.floor_cents.zip(r.max_buy_cents).map(|(f, m)| f - m))
        .collect();
    assert!(gaps.windows(2).all(|w| w[0] <= w[1]), "{gaps:?}");

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
            if let Event::Gone { id } = ev {
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
            if let Event::Gone { id } = ev {
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
async fn book_skips_listings_when_the_lowest_ask_is_above_the_ceiling() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let task = lone_moonrise(&h, &mut rx).await;
    let listings = || bodies.listing_queries.load(Ordering::Relaxed);
    let books = || bodies.book_queries.load(Ordering::Relaxed);
    let (l0, b0) = (listings(), books());

    set_lowest_ask(&bodies, 20000); // R$ 200: nenhum anúncio cabe no teto
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!((listings(), books()), (l0, b0 + 1), "só o livro");
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
    assert_eq!((listings(), books()), (l0 + 1, b0 + 2));
}

#[tokio::test]
async fn blocked_listings_keep_the_book_lane_without_waiting() {
    let (base, bodies) = serve().await;
    let (db, _dir) = temp_db();
    let (h, mut rx) = hunt_against(&base, db);
    let task = lone_moonrise(&h, &mut rx).await;
    let listings = || bodies.listing_queries.load(Ordering::Relaxed);
    let books = || bodies.book_queries.load(Ordering::Relaxed);

    // a leitura de anúncios leva 429: pausa só esse balde
    bodies.listings_429.store(true, Ordering::Relaxed);
    set_lowest_ask(&bodies, 200);
    assert!(scheduler::run_task(&h, &task).await.is_err());
    assert!(h.steam.blocked(crate::steam::client::Endpoint::Listings));
    let (l0, b0) = (listings(), books());

    // o livro segue; preço bom não tenta os anúncios barrados nem segura o passo
    let quick = Duration::from_secs(5);
    tokio::time::timeout(quick, scheduler::run_task(&h, &task))
        .await
        .expect("não espera a pausa dos anúncios")
        .unwrap();
    assert_eq!((listings(), books()), (l0, b0 + 1));
    set_lowest_ask(&bodies, 20000);
    tokio::time::timeout(quick, scheduler::run_task(&h, &task))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((listings(), books()), (l0, b0 + 2));

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
            if let Event::Gone { id } = ev {
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
            .starts_with("6 anúncios conferidos em 1 min")
    );
    T.store(NOW + 600, Ordering::SeqCst);
    detect::roll_cycle(&h);
    assert_eq!(
        h.state().hunt.message.as_deref(),
        Some(
            "6 anúncios conferidos em 10 min; nenhum deu lucro, o mais perto dava R$ 0,12 de lucro"
        )
    );
    // o ciclo novo começa zerado e o balanço antigo segue até o próximo fechar
    assert!(
        h.lock()
            .cycle
            .message(NOW + 601)
            .unwrap()
            .starts_with("6 anúncios")
    );
    T.store(NOW + 1200, Ordering::SeqCst);
    detect::roll_cycle(&h);
    assert!(
        h.state()
            .hunt
            .message
            .unwrap()
            .starts_with("0 anúncios conferidos em 10 min")
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
        orders::remember_book(&h, MOON, NOW, Some(b.clone())).await,
        None
    );
    // mesmo topo (só a contagem de pedidos de compra mudou): não grava de novo
    let more_bids = crate::steam::book::Book {
        buy_orders: b.buy_orders + 3,
        ..b.clone()
    };
    assert!(
        orders::remember_book(&h, MOON, NOW + 60, Some(more_bids))
            .await
            .is_some()
    );
    // falha de leitura não grava e não apaga o que já se sabia
    orders::remember_book(&h, MOON, NOW + 90, None).await;
    let mut sold = b.clone();
    sold.sell_levels[0].1 = 1; // um dos dois anúncios de R$ 1,67 vendeu
    orders::remember_book(&h, MOON, NOW + 120, Some(sold)).await;
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
