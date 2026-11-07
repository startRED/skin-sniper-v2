//! A saída pública busca uma página em paralelo; só a agenda principal processa o resultado.

use super::{Hunt, catalog, detect, ledger, orders, scheduler::Task};
use crate::research::{self, Context, Decision};
use crate::steam::{
    client::Result as SteamResult,
    listings::{self, ListingsPage},
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

// shortcut: uma página a cada 12 s e cada alvo no máximo uma vez por minuto; ajustar após medir 429 e achados.
const GAP: Duration = Duration::from_secs(12);
const TARGET_GAP: Duration = Duration::from_secs(60);
const MAX_AGE: Duration = Duration::from_secs(20);

struct Completed {
    task: Task,
    started_at: i64,
    started: Instant,
    revision: (u64, u64),
    context: Context,
    attempts: i64,
    page: Option<SteamResult<ListingsPage>>,
}

#[derive(Default)]
pub(super) struct Reader {
    pending: Option<JoinHandle<Result<Completed>>>,
    last_start: Option<Instant>,
    recent: HashMap<String, Instant>,
}

impl Reader {
    pub async fn step(&mut self, h: &Arc<Hunt>) {
        if self.pending.as_ref().is_some_and(JoinHandle::is_finished) {
            let handle = self.pending.take().expect("tarefa concluída");
            match handle.await {
                Ok(Ok(completed)) => consume(h, completed).await,
                Ok(Err(e)) => tracing::warn!("leitura paralela: {e:#}"),
                Err(e) => tracing::warn!("tarefa paralela terminou: {e}"),
            }
        }
        if self.pending.is_some()
            || self.last_start.is_some_and(|at| at.elapsed() < GAP)
            || !h.steam.public_alt_available()
        {
            return;
        }
        self.recent.retain(|_, at| at.elapsed() < TARGET_GAP);
        let task = loop {
            let Some(task) = pick(h, &self.recent) else {
                return;
            };
            if history_fresh(h, &task.name).await.unwrap_or(false) {
                break task;
            }
            self.recent.insert(task.id, Instant::now());
        };
        let started_at = h.now();
        let started = Instant::now();
        let revision = h.lock().queue.revision(&task);
        self.recent.insert(task.id.clone(), started);
        self.last_start = Some(started);
        let h = h.clone();
        self.pending = Some(tokio::spawn(async move {
            let decision = Decision {
                source: "paralela".into(),
                policy: "paralela".into(),
                target: task.id.clone(),
                scale: h.lock().closeness_scale,
                block_id: None,
            };
            let (result, context) = research::scope_tracked(h.db.clone(), decision, async {
                Ok(listings::query_public_alt(&h.steam, &task.name, &task.code).await)
            })
            .await?;
            let page = result?;
            let attempts = research::attempts(h.db.clone(), context.id).await?;
            Ok(Completed {
                task,
                started_at,
                started,
                revision,
                context,
                attempts,
                page,
            })
        }));
    }
}

fn pick(h: &Hunt, recent: &HashMap<String, Instant>) -> Option<Task> {
    let s = h.lock();
    if s.saldo <= 0 || !s.label_books.is_empty() {
        return None;
    }
    let now = h.now();
    if s.active
        .values()
        .any(|a| s.queue.last_at(&a.task).is_none_or(|at| now - at >= 600))
    {
        return None;
    }
    let tasks = s.queue.tasks();
    // Reserva a próxima vaga ao livro periódico ou ao professor quando houver trabalho pendente.
    let book_due = s.book_at > 0
        && now - s.book_at >= orders::GAP - 6
        && tasks.iter().any(|t| {
            !t.rare()
                && s.books
                    .get(&t.name)
                    .is_none_or(|(at, _)| now - at >= orders::TTL)
        });
    if book_due {
        return None;
    }
    tasks
        .into_iter()
        .filter(|t| !t.rare())
        .filter(|t| s.queue.external.as_ref() != Some(&t.id))
        .filter(|t| !s.active.values().any(|a| a.task == t.id))
        .filter(|t| !recent.contains_key(&t.id))
        .min_by_key(|t| {
            (
                s.floors.get(&t.floor_key()).map_or(0, |(_, at)| *at),
                t.id.clone(),
            )
        })
}

async fn history_fresh(h: &Hunt, name: &str) -> Result<bool> {
    let name = name.to_string();
    let cutoff = h.now() - catalog::HISTORY_TTL + 60;
    h.db.call(move |c| {
        let at: Option<i64> = c
            .query_row("SELECT history_at FROM items WHERE name=?1", [name], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        Ok(at.is_some_and(|at| at >= cutoff))
    })
    .await
}

fn current(h: &Hunt, completed: &Completed) -> bool {
    let s = h.lock();
    let t = &completed.task;
    completed.started.elapsed() < MAX_AGE
        && s.saldo > 0
        && s.queue.external.as_ref() != Some(&t.id)
        && s.queue.revision(t) == completed.revision
        && !s.active.values().any(|a| a.task == t.id)
        && s.queue.tasks().iter().any(|v| {
            v.id == t.id && !v.rare() && v.name == t.name && v.code == t.code && v.wear == t.wear
        })
        && s.floors
            .get(&t.floor_key())
            .is_none_or(|(_, at)| *at < completed.started_at)
}

async fn consume(h: &Hunt, mut completed: Completed) {
    if completed.attempts != 1 || !current(h, &completed) {
        return;
    }
    let Some(page) = completed.page.take() else {
        return;
    };
    let page = match page {
        Ok(page) => page,
        Err(e) => {
            tracing::warn!("anúncios na saída extra: {e}");
            return;
        }
    };
    if !history_fresh(h, &completed.task.name)
        .await
        .unwrap_or(false)
        || !current(h, &completed)
    {
        return;
    }
    h.steam.note_legacy_ok();
    h.lock()
        .queue
        .record_scan(&completed.task.id, completed.started_at);
    research::with_context(completed.context, async {
        let now = h.now();
        let key = format!(
            "{}|legacy:{}",
            ledger::query_key(&completed.task),
            completed.task.name
        );
        ledger::count_hour(h, now, "queries", 1).await;
        if let Err(e) = ledger::record(h, &key, &page, now).await {
            tracing::warn!("registro de observações falhou: {e:#}");
        }
        match detect::process(h, &completed.task, &page).await {
            Ok(out) => {
                ledger::count_hour(h, now, "near", out.rewarded.near_hits).await;
                h.lock().queue.reward(&completed.task.id, now, out.rewarded);
            }
            Err(e) => tracing::warn!("análise paralela: {e:#}"),
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csfloat::CsfloatClient;
    use crate::db::{Db, temp_db, tempfile_dir};
    use crate::hunt::HuntDeps;
    use crate::settings::SettingsStore;
    use crate::steam::client::{Endpoint, SteamClient};
    use crate::web::api::SaldoSource;
    use axum::{Router, http::HeaderMap, routing::get};
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::{Notify, broadcast, oneshot};

    const NOW: i64 = 1_793_754_060;
    const NAME: &str = "Glock-18 | Moonrise (Field-Tested)";
    const CODE: &str = "G180420B6053004";
    fn clock() -> i64 {
        NOW
    }

    async fn setup(base: &str, alt: reqwest::Client) -> (Arc<Hunt>, Db, tempfile_dir::Dir, Task) {
        let (db, dir) = temp_db();
        db.call(|c| {
            c.execute(
                "INSERT INTO items(name,updated_at,history_at) VALUES (?1,?2,?2)",
                (NAME, NOW),
            )?;
            for day in 0..30 {
                c.execute(
                    "INSERT INTO price_points(name,ts,median_cents,purchases) VALUES (?1,?2,?3,?4)",
                    (NAME, NOW - day * 86_400, 1200, 50),
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let steam = SteamClient::with_base(base)
            .unwrap()
            .with_public_alt_for_test(alt);
        steam.set_gap_for_test(Duration::from_millis(5));
        let (events, _) = broadcast::channel(64);
        let h = Hunt::with_clock(
            HuntDeps {
                settings: SettingsStore::new(db.clone()),
                db: db.clone(),
                steam: Arc::new(steam),
                csfloat: Arc::new(CsfloatClient::with_base(base).unwrap()),
                events,
                telegram: None,
            },
            clock,
        );
        h.set_saldo(3_608, SaldoSource::Manual);
        let task = Task {
            id: format!("d:{NAME}"),
            name: NAME.into(),
            code: CODE.into(),
            wear: Some(2),
            seeds: vec![],
            float: None,
            score: 1.0,
        };
        {
            let mut s = h.lock();
            s.queue.set_tasks(vec![task.clone()]);
            s.csfloat_cache.insert(NAME.into(), (NOW, vec![]));
        }
        (h, db, dir, task)
    }

    async fn fake_server() -> (String, oneshot::Receiver<()>, Arc<Notify>, Arc<AtomicUsize>) {
        let (started_tx, started_rx) = oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started_tx)));
        let release = Arc::new(Notify::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/book", get(|| async { "{}" }))
            .route(
                "/market/listings/730/{name}/render/",
                get({
                    let started = started.clone();
                    let release = release.clone();
                    move |headers: HeaderMap| {
                        let started = started.clone();
                        let release = release.clone();
                        async move {
                            assert_eq!(headers.get("x-test-alt").unwrap(), "yes");
                            assert_eq!(headers.get("cookie").unwrap(), "bMarketOptOut=1");
                            if let Some(tx) = started.lock().unwrap().take() {
                                let _ = tx.send(());
                            }
                            release.notified().await;
                            let mut page: serde_json::Value = serde_json::from_str(include_str!(
                                "../../testdata/steam_legacy_render_moonrise_2026-11.json"
                            ))
                            .unwrap();
                            let first = page["listinginfo"]
                                .as_object_mut()
                                .unwrap()
                                .values_mut()
                                .next()
                                .unwrap();
                            first["converted_price"] = serde_json::json!(100);
                            first["converted_fee"] = serde_json::json!(15);
                            axum::Json(page)
                        }
                    }
                }),
            )
            .route(
                "/ping",
                get({
                    let hits = hits.clone();
                    move |headers: HeaderMap| {
                        let hits = hits.clone();
                        async move {
                            assert!(!headers.contains_key("x-test-alt"));
                            hits.fetch_add(1, Ordering::Relaxed);
                            "{}"
                        }
                    }
                }),
            )
            .route(
                "/market/actions",
                get(|headers: HeaderMap| async move {
                    assert!(!headers.contains_key("x-test-alt"));
                    include_str!("../../testdata/steam_listings_moonrise_sorted_price.json")
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), started_rx, release, hits)
    }

    fn alt_client() -> reqwest::Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-test-alt", "yes".parse().unwrap());
        reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn book_traffic_does_not_starve_the_next_parallel_page() {
        let (base, started, release, main_hits) = fake_server().await;
        let (h, db, _dir, _task) = setup(&base, alt_client()).await;
        // O laço principal acabou de gastar a vaga extra com o livro.
        h.steam
            .get_json(Endpoint::Book, "/book", &[])
            .await
            .unwrap();
        let mut reader = Reader::default();
        reader.step(&h).await;
        assert!(
            reader.pending.is_some(),
            "o leitor deve reservar a próxima vaga, mesmo depois de uma consulta do livro"
        );
        tokio::time::timeout(
            Duration::from_secs(2),
            h.steam.get_json(Endpoint::Listings, "/ping", &[]),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(main_hits.load(Ordering::Relaxed), 1);
        tokio::time::timeout(Duration::from_secs(8), started)
            .await
            .expect("o livro não pode tomar indefinidamente a vaga de anúncios")
            .unwrap();
        release.notify_one();
        let done = reader.pending.take().unwrap().await.unwrap().unwrap();
        consume(&h, done).await;
        let count: i64 = db
            .call(|c| c.query_row("SELECT count(*) FROM observations", [], |r| r.get(0)))
            .await
            .unwrap();
        assert!(count > 0, "a página aguardada deve ser aproveitada");
    }

    #[tokio::test]
    async fn stale_history_does_not_starve_a_fresh_target() {
        let (base, started, release, _) = fake_server().await;
        let (h, _db, _dir, fresh) = setup(&base, alt_client()).await;
        let mut stale = fresh.clone();
        stale.id = "d:A".into();
        stale.name = "A".into();
        h.lock().queue.set_tasks(vec![stale, fresh]);
        let mut reader = Reader::default();
        reader.step(&h).await;
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .expect("o alvo atualizado deve ser consultado no mesmo ciclo")
            .unwrap();
        release.notify_one();
        reader.pending.take().unwrap().await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn parallel_page_does_not_block_primary_and_is_processed_once() {
        let (base, started, release, main_hits) = fake_server().await;
        let (h, db, _dir, task) = setup(&base, alt_client()).await;
        let mut reader = Reader::default();
        reader.step(&h).await;
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            h.steam.get_json(Endpoint::Search, "/ping", &[]),
        )
        .await
        .expect("a saída extra não deve atrasar a principal")
        .unwrap();
        assert_eq!(main_hits.load(Ordering::Relaxed), 1);
        release.notify_one();
        let done = reader.pending.take().unwrap().await.unwrap().unwrap();
        assert_eq!(done.attempts, 1);
        assert!(done.page.as_ref().unwrap().is_ok());
        let page = done.page.as_ref().unwrap().as_ref().unwrap().clone();
        let mut events_rx = h.events.subscribe();
        consume(&h, done).await;
        let (observations, queries, events): (i64, i64, i64) = db.call(|c| {
            Ok((c.query_row("SELECT count(*) FROM observations WHERE query LIKE '%|legacy:%'", [], |r| r.get(0))?,
                c.query_row("SELECT coalesce(sum(queries),0) FROM hourly_activity", [], |r| r.get(0))?,
                c.query_row("SELECT count(*) FROM research_attempts WHERE route='alt' AND source='paralela' AND outcome='ok'", [], |r| r.get(0))?))
        }).await.unwrap();
        assert_eq!((observations, queries, events), (3, 1, 1));
        let found: i64 = db
            .call(|c| c.query_row("SELECT count(*) FROM opportunities", [], |r| r.get(0)))
            .await
            .unwrap();
        let reasons: Vec<String> = db
            .call(|c| {
                c.prepare("SELECT reason FROM evaluations")?
                    .query_map([], |r| r.get(0))?
                    .collect()
            })
            .await
            .unwrap();
        assert!(
            found > 0,
            "a página paralela deve gerar oportunidades no teste: {reasons:?}"
        );
        let attributed: i64 = db.call(|c| c.query_row("SELECT count(*) FROM research_finds f JOIN research_decisions d ON d.id=f.decision_id WHERE d.source='paralela'", [], |r| r.get(0))).await.unwrap();
        assert_eq!(
            attributed, found,
            "cada achado pertence à decisão que o encontrou"
        );
        let mut alerts = 0;
        while let Ok(event) = events_rx.try_recv() {
            if matches!(event, crate::web::api::Event::Opportunity(_)) {
                alerts += 1;
            }
        }
        assert_eq!(alerts as i64, found);
        let normal = Decision {
            source: "normal".into(),
            policy: "baseline".into(),
            target: task.id.clone(),
            scale: 0.1,
            block_id: None,
        };
        research::scope(db.clone(), normal, async {
            detect::process(&h, &task, &page).await?;
            Ok(())
        })
        .await
        .unwrap();
        assert!(
            events_rx.try_recv().is_err(),
            "a repetição não deve notificar duas vezes"
        );
        let still_found: i64 = db
            .call(|c| c.query_row("SELECT count(*) FROM research_finds", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(still_found, found);
        assert!(h.lock().floors.contains_key(&task.floor_key()));
        super::super::scheduler::save_stats(&h).await.unwrap();
        let scans: i64 = db
            .call(move |c| {
                c.query_row(
                    "SELECT queries FROM arm_stats WHERE id=?1",
                    [task.id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(scans, 1);
    }

    #[tokio::test]
    async fn page_is_discarded_after_a_newer_rare_query_of_the_same_skin() {
        let (base, started, release, _) = fake_server().await;
        let (h, db, _dir, task) = setup(&base, alt_client()).await;
        let mut rare = task.clone();
        rare.id = format!("r:{CODE}");
        rare.name = "Glock-18 | Moonrise (Minimal Wear)".into();
        rare.seeds = vec![(59, 59)];
        rare.wear = None;
        h.lock().queue.set_tasks(vec![task.clone(), rare.clone()]);
        let mut reader = Reader::default();
        reader.step(&h).await;
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            super::super::scheduler::run_task(&h, &rare),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!h.lock().floors.contains_key(&task.floor_key()));
        let before: i64 = db
            .call(|c| c.query_row("SELECT count(*) FROM observations", [], |r| r.get(0)))
            .await
            .unwrap();
        release.notify_one();
        let done = reader.pending.take().unwrap().await.unwrap().unwrap();
        consume(&h, done).await;
        let count: i64 = db
            .call(|c| c.query_row("SELECT count(*) FROM observations", [], |r| r.get(0)))
            .await
            .unwrap();
        assert_eq!(
            count, before,
            "página antiga não altera o registro nem marca sumiço"
        );
        assert!(!h.lock().floors.contains_key(&task.floor_key()));
        super::super::scheduler::save_stats(&h).await.unwrap();
        let scans: i64 = db
            .call(move |c| {
                c.query_row(
                    "SELECT count(*) FROM arm_stats WHERE id=?1",
                    [task.id],
                    |r| r.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(scans, 0, "página descartada não penaliza o aprendizado");
    }

    #[tokio::test]
    async fn picks_only_normal_common_targets_and_preserves_due_books() {
        let (h, _db, _dir, task) = setup("http://127.0.0.1:9", alt_client()).await;
        assert_eq!(pick(&h, &HashMap::new()), Some(task.clone()));
        h.lock().queue.external = Some(task.id.clone());
        assert!(pick(&h, &HashMap::new()).is_none());
        {
            let mut s = h.lock();
            s.queue.external = None;
            s.book_at = NOW - orders::GAP;
        }
        assert!(pick(&h, &HashMap::new()).is_none());
        {
            let mut s = h.lock();
            s.books.insert(task.name.clone(), (NOW, None));
        }
        assert_eq!(pick(&h, &HashMap::new()), Some(task));
    }

    #[tokio::test]
    async fn parallel_evaluation_never_fetches_stale_history_or_uncached_sales() {
        let (h, db, _dir, task) = setup("http://127.0.0.1:9", alt_client()).await;
        db.call(|c| {
            c.execute(
                "UPDATE items SET history_at=?1 WHERE name=?2",
                (NOW - catalog::HISTORY_TTL, NAME),
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let decision = Decision {
            source: "paralela".into(),
            policy: "paralela".into(),
            target: task.id.clone(),
            scale: 0.1,
            block_id: None,
        };
        let before = h.steam.sent();
        research::scope(db.clone(), decision.clone(), async {
            assert!(detect::points_for(&h, NAME, NOW).await.is_err());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            h.steam.sent(),
            before,
            "histórico vencido não deve usar a conexão principal"
        );
        db.call(|c| {
            c.execute("UPDATE items SET history_at=?1 WHERE name=?2", (NOW, NAME))?;
            Ok(())
        })
        .await
        .unwrap();
        h.lock().csfloat_cache.clear();
        let raw: serde_json::Value = serde_json::from_str(include_str!(
            "../../testdata/steam_legacy_render_moonrise_2026-11.json"
        ))
        .unwrap();
        let mut page = crate::steam::legacy::parse_page(&raw, NAME, CODE, 0).unwrap();
        page.listings.truncate(1);
        page.listings[0].seed = Some(59);
        let listing = page.listings[0].clone();
        assert!(
            crate::pricing::groups::group_of(NAME, Some(59), listing.paint_index, listing.float)
                .is_some()
        );
        research::scope(db.clone(), decision, async {
            detect::process(&h, &task, &page).await?;
            Ok(())
        })
        .await
        .unwrap();
        let (calls, seen): (i64, i64) = db
            .call(move |c| {
                Ok((
                    c.query_row(
                        "SELECT count(*) FROM research_attempts WHERE route='csfloat'",
                        [],
                        |r| r.get(0),
                    )?,
                    c.query_row(
                        "SELECT count(*) FROM listings_seen WHERE listing_id=?1",
                        [listing.listing_id],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(
            (calls, seen),
            (0, 0),
            "sem cache, o raro espera a consulta normal sem gastar CSFloat nem marcar visto"
        );
    }
}
