//! Teste de ponta a ponta: um servidor HTTP local (axum) serve as capturas reais no lugar da Steam e do
//! CSFloat, e a caça roda do banco vazio até o evento `opportunity`. É teste; não entra no produto.
//!
//! As capturas reais são usadas como estão, com estas alterações feitas no teste (a captura de 03/11 não
//! tem anúncio com lucro): o primeiro anúncio da página de listagens passa a custar R$ 4,00, o segundo
//! recebe seed 59 (tier 1 do Moonrise) e custa R$ 7,00, e a primeira entrada da busca vira o Moonrise FT
//! a US$ 1,00. No CSFloat, três vendas sem adesivo ganham seeds do tier 1 e preço ×1,5.

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
use std::sync::atomic::{AtomicUsize, Ordering};
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
    search: Mutex<Value>,
    listing_queries: AtomicUsize,
    csfloat: Value,
}

async fn serve() -> (String, Arc<Bodies>) {
    let b = Arc::new(Bodies {
        listings: Mutex::new(listings_body()),
        history: fixture("steam_pricehistory_moonrise_ft.json"),
        search: Mutex::new(search_body()),
        listing_queries: AtomicUsize::new(0),
        csfloat: csfloat_body(),
    });
    async fn actions(
        AxState(b): AxState<Arc<Bodies>>,
        Query(q): Query<HashMap<String, String>>,
    ) -> Json<Value> {
        match q.get("q").map(String::as_str) {
            Some("QueryListingsForItem") => {
                b.listing_queries.fetch_add(1, Ordering::Relaxed);
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
                Json(body)
            }
            Some("QueryPriceHistory") => Json(b.history.clone()),
            other => panic!("consulta inesperada: {other:?}"),
        }
    }
    let app = Router::new()
        .route("/market/actions", get(actions))
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
    assert!(st.hunt.watching >= 10, "watching {}", st.hunt.watching);

    // o que está sendo vigiado: piso visto, teto de compra (de onde ainda sobra R$ 0,20 e 5 %) e ritmo
    let watch = watch::list(&h).await.unwrap();
    assert_eq!(watch.len(), st.hunt.watching as usize);
    // o Moonrise (aqui raro por seed) aparece com o piso visto e o teto do prêmio do grupo: 3 vendas do
    // CSFloat → mid R$ 8,87 → líquido 7,72 → compraria até 7,35 (5 % de margem)
    let raro = watch
        .iter()
        .find(|r| r.kind == Kind::Raro && r.floor_cents == Some(700))
        .unwrap_or_else(|| panic!("piso R$ 7,00 visto na faixa 58-59: {watch:#?}"));
    assert_eq!((raro.kind, raro.max_buy_cents), (Kind::Raro, Some(735)));
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
    bodies.search.lock().unwrap()["results"][0]["sell_price"] = json!(10000);
    let before = bodies.listing_queries.load(Ordering::Relaxed);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), before);
    assert_eq!(h.lock().cycle.saved_listings, 2);

    bodies.search.lock().unwrap()["results"][0]["sell_price"] = json!(100);
    scheduler::run_task(&h, &task).await.unwrap();
    assert_eq!(bodies.listing_queries.load(Ordering::Relaxed), before + 1);
    let s = h.lock();
    assert!(s.floors.contains_key(&task.id));
    assert!(s.floors.contains_key(&duplicate.id));
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
        score: 1.0,
    };
    {
        let mut s = h.lock();
        s.queue.set_tasks(vec![task.clone()]);
        s.floors.clear();
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
