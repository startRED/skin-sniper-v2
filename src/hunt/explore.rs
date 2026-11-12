//! Entrada e rodízio de candidatas fora do ranking; a cota física pertence a `research`.
use super::{
    Hunt,
    catalog::{self, Ctx, Item},
    scheduler::{Mode, Task},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const KEY: &str = "exploration_state";
// Hipóteses operacionais: visitas espaçadas, uma janela curta e retorno futuro sem banimento.
const VISIT_GAP: i64 = 3600;
const TEST_WINDOW: i64 = 6 * 3600;
const MAX_WINDOW: i64 = 24 * 3600;
pub const SCHEMA: &str = r#"
CREATE TABLE research_targets(id TEXT PRIMARY KEY, json TEXT NOT NULL, last_selected_at INTEGER NOT NULL);
CREATE TABLE research_tests(id TEXT PRIMARY KEY, family TEXT NOT NULL, last_test_at INTEGER NOT NULL, visits INTEGER NOT NULL DEFAULT 0);
CREATE TABLE research_finds(listing_id TEXT PRIMARY KEY, decision_id INTEGER, at INTEGER NOT NULL, target TEXT NOT NULL, profit_cents INTEGER NOT NULL);
CREATE INDEX research_finds_decision ON research_finds(decision_id);
CREATE TABLE research_credits(listing_id TEXT PRIMARY KEY, at INTEGER NOT NULL);
"#;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub candidate: Option<Candidate>,
    pub last_family: String,
    #[serde(skip)]
    pub refreshed_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub task: Task,
    pub entered_at: i64,
    pub last_pick_at: i64,
    pub visits: u32,
    pub retained: bool,
}

pub async fn load(h: &Hunt) -> Result<()> {
    if let Some(json) = catalog::get_setting(&h.db, KEY).await? {
        match serde_json::from_str(&json) {
            Ok(state) => h.lock().exploration = state,
            Err(e) => tracing::warn!("estado da exploração inválido; retoma pelo registro: {e}"),
        }
    }
    Ok(())
}

async fn save(h: &Hunt) -> Result<()> {
    let state = h.lock().exploration.clone();
    catalog::set_setting(&h.db, KEY, serde_json::to_string(&state)?).await
}

pub async fn refresh(h: &Hunt) -> Result<()> {
    let now = h.now();
    if h.lock().exploration.refreshed_at != 0 && now - h.lock().exploration.refreshed_at < 60 {
        return Ok(());
    }
    h.lock().exploration.refreshed_at = now;
    let was_pilot = h.lock().trial.as_ref().is_some_and(|t| t.pilot);
    super::trial::step(h).await?;
    let info = h.db.call(move |c| super::trial::info(c, now)).await?;
    let budget = h.db.call(move |c| crate::research::summary(c, now)).await?;
    let mut s = h.lock();
    let changed = was_pilot != info.pilot;
    if changed {
        s.selected = None;
    }
    s.trial = Some(info);
    let active: HashSet<_> = s.active.values().map(|a| a.task.clone()).collect();
    let candidate = s.exploration.candidate.clone();
    s.queue.external_ready = budget.available > 0
        && s.trial.as_ref().is_some_and(|t| t.pilot)
        && candidate
            .as_ref()
            .is_some_and(|c| now - c.last_pick_at >= VISIT_GAP);
    s.queue.external = candidate
        .as_ref()
        .filter(|c| !active.contains(&c.task.id))
        .filter(|_| s.trial.as_ref().is_some_and(|t| t.pilot))
        .map(|c| c.task.id.clone());
    Ok(())
}

fn family(name: &str) -> &str {
    name.split(" | ").next().unwrap_or(name)
}

fn insert_slot(tasks: &mut Vec<Task>, task: Task, protected: &HashSet<String>) -> bool {
    if tasks.iter().any(|t| t.id == task.id) {
        return true;
    }
    if tasks.iter().filter(|t| t.mode() == Mode::Cheap).count() >= catalog::WATCH_MAX {
        let Some(pos) = tasks
            .iter()
            .rposition(|t| t.mode() == Mode::Cheap && !protected.contains(&t.id))
        else {
            return false;
        };
        tasks.remove(pos);
    }
    tasks.push(task);
    true
}

pub async fn select(
    h: &Hunt,
    items: &[Item],
    ctx: &Ctx<'_>,
    mut tasks: Vec<Task>,
) -> Result<Vec<Task>> {
    let now = h.now();
    let (mut state, active, old, pilot) = {
        let s = h.lock();
        (
            s.exploration.clone(),
            s.active
                .values()
                .map(|a| a.task.clone())
                .collect::<HashSet<_>>(),
            s.queue.tasks(),
            s.trial.as_ref().is_some_and(|i| i.pilot),
        )
    };
    // As tarefas ficam gravadas para restaurar o acompanhamento inclusive depois de reiniciar.
    let saved: Vec<Task> =
        h.db.call(|c| {
            c.prepare("SELECT json FROM research_targets")?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| match r {
                    Ok(json) => serde_json::from_str(&json).ok().map(Ok),
                    Err(e) => Some(Err(e)),
                })
                .collect()
        })
        .await?;
    // Na primeira atualização ainda não existe research_targets para os ativos da versão anterior.
    let inherited: Vec<Task> = {
        let s = h.lock();
        s.active
            .values()
            .filter_map(|a| {
                let name = a.task.strip_prefix("d:").unwrap_or(&a.opp.name);
                let code = items
                    .iter()
                    .find(|i| i.name == name)
                    .and_then(|i| i.code.clone())
                    .or_else(|| {
                        crate::steam::listings::parse_item_code_location(&a.opp.steam_url).ok()
                    })?;
                let seeds = if a.task.starts_with("r:") {
                    let mut seeds: Vec<_> = s
                        .active
                        .values()
                        .filter(|b| b.task == a.task)
                        .filter_map(|b| b.opp.seed.and_then(|v| u16::try_from(v).ok()))
                        .map(|seed| (seed, seed))
                        .collect();
                    seeds.sort_unstable();
                    seeds.dedup();
                    seeds
                } else {
                    vec![]
                };
                Some(Task {
                    id: a.task.clone(),
                    name: name.to_owned(),
                    code,
                    wear: (!a.task.starts_with("r:"))
                        .then(|| catalog::wear_of(name))
                        .flatten(),
                    seeds,
                    float: None,
                    score: 0.0,
                })
            })
            .collect()
    };
    for task in old
        .into_iter()
        .chain(saved)
        .chain(inherited)
        .filter(|t| active.contains(&t.id))
    {
        if task.mode() == Mode::Cheap {
            if !insert_slot(&mut tasks, task.clone(), &active) {
                tasks.push(task);
            }
        } else if !tasks.iter().any(|t| t.id == task.id) {
            tasks.push(task);
        }
    }
    let normal: HashSet<_> = tasks.iter().map(|t| t.id.clone()).collect();
    if pilot {
        let all = catalog::common_candidates(items, ctx);
        let expired = state.candidate.as_ref().is_some_and(|c| {
            !active.contains(&c.task.id)
                && (now - c.entered_at >= MAX_WINDOW
                    || (now - c.entered_at >= TEST_WINDOW && c.visits >= 3))
        });
        let retain_candidate = if expired && state.candidate.as_ref().is_some_and(|c| !c.retained) {
            let id = state.candidate.as_ref().unwrap().task.id.clone();
            h.db.call(move |c| {
                let (finds, attempts): (i64,i64) = c.query_row("SELECT (SELECT count(*) FROM research_finds WHERE target=?1 AND at>=?2),(SELECT count(*) FROM research_attempts a JOIN research_decisions d ON d.id=a.decision_id WHERE d.target=?1 AND a.route IN ('main','alt') AND a.outcome != 'not_sent' AND a.at>=?2)",params![id,now-MAX_WINDOW],|r|Ok((r.get(0)?,r.get(1)?)))?;
                let (reference_finds, reference_attempts) = reference(c, now - MAX_WINDOW)?;
                Ok(finds >= 3 && attempts >= 12 && reference_attempts >= 12 && i128::from(finds)*i128::from(reference_attempts) > i128::from(reference_finds)*i128::from(attempts))
            }).await?
        } else {
            false
        };
        if retain_candidate {
            if let Some(c) = state.candidate.as_mut() {
                c.retained = true;
                c.entered_at = now;
                c.visits = 0;
            }
        } else if expired {
            state.last_family = state
                .candidate
                .as_ref()
                .map_or(String::new(), |c| family(&c.task.name).to_string());
            state.candidate = None;
        }
        if state
            .candidate
            .as_ref()
            .is_some_and(|c| !all.iter().any(|t| t.id == c.task.id) && !active.contains(&c.task.id))
        {
            state.candidate = None;
        }
        if state.candidate.is_none() {
            let mut pool: Vec<_> = all
                .into_iter()
                .filter(|t| !normal.contains(&t.id))
                .filter(|t| {
                    items.iter().find(|i| i.name == t.name).is_some_and(|i| {
                        i.history_at
                            .is_some_and(|at| now - at < catalog::HISTORY_TTL)
                    })
                })
                .collect();
            let seen =
                h.db.call(|c| {
                    c.prepare("SELECT id,last_test_at FROM research_tests")?
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                        .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
                })
                .await?;
            pool.sort_by(|a, b| {
                seen.get(&a.id)
                    .copied()
                    .unwrap_or(0)
                    .cmp(&seen.get(&b.id).copied().unwrap_or(0))
                    .then_with(|| {
                        (family(&a.name) == state.last_family)
                            .cmp(&(family(&b.name) == state.last_family))
                    })
                    .then(a.id.cmp(&b.id))
            });
            if let Some(task) = pool.into_iter().next() {
                state.candidate = Some(Candidate {
                    task,
                    entered_at: now,
                    last_pick_at: 0,
                    visits: 0,
                    retained: false,
                });
            }
        }
        if let Some(candidate) = &state.candidate
            && !insert_slot(&mut tasks, candidate.task.clone(), &active)
        {
            state.candidate = None;
        }
    }
    let external = state
        .candidate
        .as_ref()
        .filter(|_| pilot)
        .filter(|c| !active.contains(&c.task.id))
        .map(|c| c.task.id.clone());
    {
        let mut s = h.lock();
        s.exploration = state;
        s.queue.external = external;
    }
    let rows = tasks.clone();
    h.db.call(move |c| {
        let tx = c.transaction()?;
        for task in rows {
            let json = serde_json::to_string(&task).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            tx.execute("INSERT INTO research_targets VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET json=excluded.json,last_selected_at=excluded.last_selected_at", params![task.id,json,now])?;
        }
        tx.commit()
    }).await?;
    save(h).await?;
    Ok(tasks)
}

pub async fn visited(h: &Hunt, task: &Task, attempts: i64) -> Result<()> {
    if attempts == 0 {
        return Ok(());
    }
    let now = h.now();
    let update = {
        let mut s = h.lock();
        let candidate = s
            .exploration
            .candidate
            .as_mut()
            .filter(|c| c.task.id == task.id);
        if let Some(c) = candidate {
            c.last_pick_at = now;
            c.visits += 1;
            s.queue.external_ready = false;
            Some((task.id.clone(), family(&task.name).to_owned()))
        } else {
            None
        }
    };
    if let Some((id, family)) = update {
        h.db.call(move |c| { c.execute("INSERT INTO research_tests(id,family,last_test_at,visits) VALUES (?1,?2,?3,1) ON CONFLICT(id) DO UPDATE SET last_test_at=excluded.last_test_at,visits=visits+1", params![id,family,now])?; Ok(()) }).await?;
        save(h).await?;
    }
    Ok(())
}

/// Compara a exploração com as leituras comuns, inclusive as feitas pela saída paralela.
pub(super) fn reference(c: &Connection, since: i64) -> rusqlite::Result<(i64, i64)> {
    c.query_row("SELECT (SELECT count(*) FROM research_finds f JOIN research_decisions d ON d.id=f.decision_id WHERE d.source IN ('normal','paralela') AND d.target LIKE 'd:%' AND f.at>=?1),(SELECT count(*) FROM research_attempts a JOIN research_decisions d ON d.id=a.decision_id WHERE d.source IN ('normal','paralela') AND d.target LIKE 'd:%' AND a.route IN ('main','alt') AND a.outcome != 'not_sent' AND a.at>=?1)", [since], |r| Ok((r.get(0)?, r.get(1)?)))
}

pub async fn first_credit(h: &Hunt, id: &str) -> Result<bool> {
    let (id, now) = (id.to_owned(), h.now());
    Ok(h.db
        .call(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO research_credits VALUES (?1,?2)",
                params![id, now],
            )
        })
        .await?
        == 1)
}

pub fn record_find(
    c: &Connection,
    id: &str,
    decision: Option<i64>,
    now: i64,
    target: &str,
    profit: i64,
) -> rusqlite::Result<bool> {
    Ok(c.execute(
        "INSERT OR IGNORE INTO research_finds VALUES (?1,?2,?3,?4,?5)",
        params![id, decision, now, target, profit],
    )? == 1)
}

#[cfg(test)]
pub async fn found(h: &Hunt, id: &str, target: &str, profit: i64) -> Result<bool> {
    let (id, target, now, decision) = (
        id.to_owned(),
        target.to_owned(),
        h.now(),
        crate::research::current().map(|c| c.id),
    );
    h.db.call(move |c| record_find(c, &id, decision, now, &target, profit))
        .await
}

pub fn retain(c: &Connection, now: i64) -> rusqlite::Result<()> {
    let before = now - 60 * 86400;
    for table in ["research_finds", "research_credits"] {
        c.execute(&format!("DELETE FROM {table} WHERE at < ?1"), [before])?;
    }
    c.execute("DELETE FROM research_targets WHERE last_selected_at < ?1 AND id NOT IN (SELECT task FROM opportunities WHERE gone_at IS NULL AND task IS NOT NULL)", [before])?;
    c.execute(
        "DELETE FROM research_tests WHERE last_test_at < ?1",
        [before],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Info {
    pub candidate: Option<String>,
    pub visits: u32,
    pub retained: bool,
    pub targets_tested: i64,
    pub unique_finds: i64,
}

pub fn info(c: &Connection) -> rusqlite::Result<Info> {
    let json: Option<String> = c
        .query_row("SELECT value FROM settings WHERE key=?1", [KEY], |r| {
            r.get(0)
        })
        .optional()?;
    let state = json
        .as_deref()
        .and_then(|j| serde_json::from_str::<State>(j).ok())
        .unwrap_or_default();
    let mut info = Info::default();
    if let Some(candidate) = state.candidate {
        info.candidate = Some(candidate.task.name);
        info.visits = candidate.visits;
        info.retained = candidate.retained;
    }
    info.targets_tested = c.query_row("SELECT count(*) FROM research_tests", [], |r| r.get(0))?;
    info.unique_finds = c.query_row("SELECT count(*) FROM research_finds f JOIN research_decisions d ON d.id=f.decision_id WHERE d.source='exploracao_externa'",[],|r|r.get(0))?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            name: id.into(),
            code: "G1234".into(),
            wear: Some(2),
            seeds: vec![],
            float: None,
            score: 0.0,
        }
    }
    #[test]
    fn candidate_uses_one_existing_slot_and_never_evicts_an_active() {
        let mut tasks: Vec<_> = (0..40).map(|i| task(&i.to_string())).collect();
        let protected: HashSet<_> = tasks.iter().map(|t| t.id.clone()).collect();
        assert!(!insert_slot(&mut tasks, task("challenger"), &protected));
        let mut protected = protected;
        protected.remove("39");
        assert!(insert_slot(&mut tasks, task("challenger"), &protected));
        assert_eq!(tasks.len(), 40);
        assert!(protected.iter().all(|id| tasks.iter().any(|t| &t.id == id)));
    }
    #[test]
    fn state_keeps_visit_cursor_and_identity_across_restart() {
        let state = State {
            candidate: Some(Candidate {
                task: task("outside"),
                entered_at: 1,
                last_pick_at: 2,
                visits: 3,
                retained: false,
            }),
            last_family: "Glock".into(),
            refreshed_at: 7,
        };
        let restored: State =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(restored.candidate.unwrap().last_pick_at, 2);
        assert_eq!(restored.last_family, "Glock");
        assert_eq!(restored.refreshed_at, 0);
    }
}
