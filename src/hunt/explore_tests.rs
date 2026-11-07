use super::{
    Active, Hunt, HuntDeps,
    catalog::{self, Ctx, Item},
    explore,
    patterns::Learned,
    trial,
};
use crate::{
    csfloat::CsfloatClient,
    db::{self, Db},
    settings::SettingsStore,
    steam::client::SteamClient,
    web::api::Opportunity,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::broadcast;

const NOW: i64 = 1_800_000_000;
fn clock() -> i64 {
    NOW
}
fn hunt(db: Db) -> Arc<Hunt> {
    let (events, _) = broadcast::channel(16);
    Hunt::with_clock(
        HuntDeps {
            settings: SettingsStore::new(db.clone()),
            db,
            steam: Arc::new(SteamClient::with_base("http://127.0.0.1:1").unwrap()),
            csfloat: Arc::new(CsfloatClient::with_base("http://127.0.0.1:1").unwrap()),
            events,
            telegram: None,
        },
        clock,
    )
}
fn set_pilot(h: &Hunt, pilot: bool) {
    h.lock().trial = Some(trial::Info {
        enabled: true,
        pilot,
        block_id: Some(1),
        phase: if pilot { "pilot" } else { "control" }.into(),
        base_policy: "ucb".into(),
        reason: String::new(),
    });
}
fn items() -> Vec<Item> {
    (0..42)
        .map(|i| Item {
            name: format!("Family {i:02} | Test (Field-Tested)"),
            code: Some(format!("G{i:08X}1")),
            floor_usd_cents: Some(1000),
            rank: i,
            sales_per_day: Some(if i < 40 { 10.0 } else { 1.0 }),
            dispersion: Some(0.2),
            history_at: Some(NOW - 100),
        })
        .collect()
}
fn opp(id: &str) -> Opportunity {
    serde_json::from_value(serde_json::json!({
        "id": id, "name":"x", "icon_url":"", "steam_url":"", "kind":"desconto",
        "price_cents":1000, "sell":{"high":{"price_cents":1200,"net_cents":1000,"profit_cents":0,"eta_hours":null},"mid":{"price_cents":1200,"net_cents":1000,"profit_cents":0,"eta_hours":null},"quick":{"price_cents":1200,"net_cents":1000,"profit_cents":0,"eta_hours":null}},
        "margin_pct":0.0,"safe":false,"confidence":"baixa","evidence":"", "seen_at":NOW
    })).unwrap()
}

#[tokio::test]
async fn eligible_candidate_outside_top_forty_and_persisted_rotation() {
    let (db, _dir) = db::temp_db();
    let h = hunt(db.clone());
    set_pilot(&h, true);
    let mut pool = items();
    // Candidatas sem histórico fresco e acima do saldo não podem ocupar a vaga.
    pool[40].history_at = None;
    let mut too_expensive = pool[41].clone();
    too_expensive.name = "Expensive | Test (Field-Tested)".into();
    too_expensive.floor_usd_cents = Some(4000);
    pool.push(too_expensive);
    let boost = HashMap::new();
    let floors = HashMap::new();
    let learned = Learned::new();
    let ctx = Ctx {
        ratio: 1.0,
        saldo: 2000,
        now: NOW,
        boost: &boost,
        floors: &floors,
        learned: &learned,
    };
    let normal = catalog::select(&pool, &ctx);
    assert_eq!(normal.len(), 40);
    let selected = explore::select(&h, &pool, &ctx, normal.clone())
        .await
        .unwrap();
    let candidate = h
        .lock()
        .exploration
        .candidate
        .as_ref()
        .unwrap()
        .task
        .clone();
    assert_eq!(candidate.name, pool[41].name);
    assert_eq!(selected.len(), 40);
    assert!(selected.iter().any(|t| t.id == candidate.id));
    explore::visited(&h, &candidate, 1).await.unwrap();
    let again = hunt(db);
    again.load().await.unwrap();
    set_pilot(&again, true);
    assert_eq!(
        again.lock().exploration.candidate.as_ref().unwrap().visits,
        1
    );
    assert_eq!(
        again
            .lock()
            .exploration
            .candidate
            .as_ref()
            .unwrap()
            .last_pick_at,
        NOW
    );
    // Uma janela sem achados deve girar para outra candidata, mesmo com muitos créditos de quase.
    pool[40].history_at = Some(NOW - 100);
    for i in 0..5 {
        assert!(
            explore::first_credit(&again, &format!("near{i}"))
                .await
                .unwrap()
        );
    }
    {
        let mut s = again.lock();
        let c = s.exploration.candidate.as_mut().unwrap();
        c.entered_at = NOW - 25 * 3600;
        c.visits = 3;
    }
    let next = explore::select(&again, &pool, &ctx, normal).await.unwrap();
    let current = again.lock().exploration.candidate.as_ref().unwrap().clone();
    assert!(!current.retained);
    assert_ne!(current.task.id, candidate.id);
    assert!(next.iter().any(|t| t.id == current.task.id));
}

#[tokio::test]
async fn active_task_survives_control_even_when_all_slots_are_protected() {
    let (db, _dir) = db::temp_db();
    let h = hunt(db);
    set_pilot(&h, false);
    let pool = items();
    let boost = HashMap::new();
    let floors = HashMap::new();
    let learned = Learned::new();
    let ctx = Ctx {
        ratio: 1.0,
        saldo: 2000,
        now: NOW,
        boost: &boost,
        floors: &floors,
        learned: &learned,
    };
    let normal = catalog::select(&pool, &ctx);
    let missing = catalog::common_candidates(&pool, &ctx)[40].clone();
    {
        let mut s = h.lock();
        s.queue.set_tasks(vec![missing.clone()]);
        for task in normal.iter().chain(std::iter::once(&missing)) {
            s.active.insert(
                task.id.clone(),
                Active {
                    opp: opp(&task.id),
                    task: task.id.clone(),
                },
            );
        }
    }
    let selected = explore::select(&h, &pool, &ctx, normal.clone())
        .await
        .unwrap();
    assert!(
        selected.iter().any(|t| t.id == missing.id),
        "não pode abandonar acompanhamento ativo"
    );
    assert!(normal.iter().all(|t| selected.iter().any(|x| x.id == t.id)));
}

#[tokio::test]
async fn unique_credits_and_finds_survive_reload() {
    let (db, _dir) = db::temp_db();
    let h = hunt(db.clone());
    assert!(explore::first_credit(&h, "listing").await.unwrap());
    assert!(!explore::first_credit(&h, "listing").await.unwrap());
    assert!(explore::found(&h, "listing", "d:one", 200).await.unwrap());
    let again = hunt(db);
    again.load().await.unwrap();
    assert!(!explore::first_credit(&again, "listing").await.unwrap());
    assert!(
        !explore::found(&again, "listing", "d:another", 999)
            .await
            .unwrap()
    );
    let (n, profit): (i64, i64) = again
        .db
        .call(|c| {
            c.query_row(
                "SELECT count(*),sum(profit_cents) FROM research_finds",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!((n, profit), (1, 200));
}

#[tokio::test]
async fn parallel_find_and_attempt_are_counted_once_in_common_reference() {
    let (db, _dir) = db::temp_db();
    db.call(|c| {
        let decision = |source: &str, target: &str| -> rusqlite::Result<i64> {
            c.execute("INSERT INTO research_decisions(at,source,policy,target,scale,block_id) VALUES (?1,?2,?2,?3,1,NULL)", (NOW, source, target))?;
            Ok(c.last_insert_rowid())
        };
        let parallel = decision("paralela", "d:skin")?;
        let normal = decision("normal", "d:skin")?;
        let external = decision("exploracao_externa", "d:externa")?;
        let rare = decision("normal", "r:rara")?;
        for (id, source, route, outcome) in [(parallel,"paralela","alt","ok"), (normal,"normal","main","ok"), (external,"exploracao_externa","alt","ok"), (rare,"normal","main","ok"), (parallel,"paralela","alt","not_sent")] {
            c.execute("INSERT INTO research_attempts(decision_id,at,source,endpoint,route,outcome) VALUES (?1,?2,?3,'legacy',?4,?5)", (id,NOW,source,route,outcome))?;
        }
        assert!(explore::record_find(c, "listing", Some(parallel), NOW, "d:skin", 100)?);
        assert!(!explore::record_find(c, "listing", Some(normal), NOW, "d:skin", 100)?);
        assert!(explore::record_find(c, "external", Some(external), NOW, "d:externa", 100)?);
        assert!(explore::record_find(c, "rare", Some(rare), NOW, "r:rara", 100)?);
        assert_eq!(explore::reference(c, NOW - 1)?, (1, 2));
        let (source, block): (String, Option<i64>) = c.query_row("SELECT source,block_id FROM research_decisions WHERE id=?1", [parallel], |r| Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!((source.as_str(), block), ("paralela", None));
        Ok(())
    }).await.unwrap();
}
