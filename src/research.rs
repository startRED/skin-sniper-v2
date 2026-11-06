use crate::db::{Db, now_unix};
use rusqlite::{Connection, params};
use std::future::Future;

// Hipóteses conservadoras, não medidas de mercado.
pub const WINDOW_SECONDS: i64 = 3600;
pub const MAX_EXPLORATORY: i64 = 6;
pub const MAX_PER_DECISION: i64 = 3;
pub const RETAIN_DAYS: i64 = 60;
pub const MAX_ROWS: i64 = 500_000;
// Coortes recentes ficam completas durante a maturação, mesmo acima do teto.
pub const MATURATION_DAYS: i64 = 14;

pub const SCHEMA: &str = r#"
CREATE TABLE research_decisions (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, source TEXT NOT NULL, policy TEXT NOT NULL, target TEXT NOT NULL, scale REAL NOT NULL, block_id INTEGER);
CREATE INDEX research_decisions_at ON research_decisions(at);
CREATE TABLE research_attempts (id INTEGER PRIMARY KEY, decision_id INTEGER, at INTEGER NOT NULL, source TEXT NOT NULL, endpoint TEXT NOT NULL, route TEXT NOT NULL, outcome TEXT NOT NULL);
CREATE INDEX research_attempts_at ON research_attempts(at);
CREATE INDEX research_attempts_decision ON research_attempts(decision_id);
"#;

#[derive(Clone, Debug)]
pub struct Decision {
    pub source: String,
    pub policy: String,
    pub target: String,
    pub scale: f64,
    pub block_id: Option<i64>,
}
#[derive(Clone)]
pub struct Context {
    pub id: i64,
    pub source: String,
    pub policy: String,
    pub block_id: Option<i64>,
    db: Db,
}

tokio::task_local! { static CONTEXT: Context; }

pub fn current() -> Option<Context> {
    CONTEXT.try_with(Clone::clone).ok()
}

pub async fn scope<T>(
    db: Db,
    decision: Decision,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    scope_tracked(db, decision, future).await?.0
}

pub async fn scope_tracked<T>(
    db: Db,
    decision: Decision,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<(anyhow::Result<T>, Context)> {
    let d = decision.clone();
    let id = db.call(move |c| {
        c.execute("INSERT INTO research_decisions(at,source,policy,target,scale,block_id) VALUES (?1,?2,?3,?4,?5,?6)", params![now_unix(), d.source, d.policy, d.target, d.scale, d.block_id])?;
        Ok(c.last_insert_rowid())
    }).await?;
    let context = Context {
        id,
        source: decision.source,
        policy: decision.policy,
        block_id: decision.block_id,
        db,
    };
    let result = CONTEXT.scope(context.clone(), future).await;
    Ok((result, context))
}

pub async fn attempts(db: Db, id: i64) -> anyhow::Result<i64> {
    db.call(move |c| c.query_row("SELECT count(*) FROM research_attempts WHERE decision_id=?1 AND route IN ('main','alt') AND outcome != 'not_sent'", [id], |r| r.get(0))).await
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    pub reads_1h: i64,
    pub exploratory_1h: i64,
    pub exploratory_limit: i64,
    pub available: i64,
    pub csfloat_1h: i64,
    pub limited_1h: i64,
    pub failed_1h: i64,
    pub followup_1h: i64,
}

pub fn summary(c: &Connection, now: i64) -> rusqlite::Result<Summary> {
    let (normal, exploratory, csfloat): (i64,i64,i64) = c.query_row("SELECT COALESCE(SUM(route != 'csfloat' AND source != 'exploracao_externa'),0), COALESCE(SUM(route != 'csfloat' AND source = 'exploracao_externa'),0), COALESCE(SUM(route = 'csfloat'),0) FROM research_attempts WHERE at >= ?1 AND outcome != 'not_sent'", [now - WINDOW_SECONDS], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let (limited, failed, followup): (i64, i64, i64) = c.query_row("SELECT COALESCE(SUM(outcome = 'rate_limited'),0), COALESCE(SUM(outcome IN ('error','network_error')),0), COALESCE(SUM(source = 'acompanhamento'),0) FROM research_attempts WHERE at >= ?1 AND route != 'csfloat' AND outcome != 'not_sent'", [now - WINDOW_SECONDS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(Summary {
        reads_1h: normal + exploratory,
        exploratory_1h: exploratory,
        exploratory_limit: MAX_EXPLORATORY,
        available: (normal / 15 - exploratory)
            .max(0)
            .min(MAX_EXPLORATORY - exploratory),
        csfloat_1h: csfloat,
        limited_1h: limited,
        failed_1h: failed,
        followup_1h: followup,
    })
}

#[derive(Debug)]
pub(crate) enum ReserveError {
    Budget,
    Database(anyhow::Error),
}

fn eligible(c: &Connection, id: i64) -> rusqlite::Result<bool> {
    let (normal, exploratory): (i64, i64) = c.query_row("SELECT COALESCE(SUM(source != 'exploracao_externa'),0), COALESCE(SUM(source = 'exploracao_externa'),0) FROM research_attempts WHERE at >= ?1 AND route != 'csfloat' AND outcome != 'not_sent'", [now_unix() - WINDOW_SECONDS], |r| Ok((r.get(0)?,r.get(1)?)))?;
    let per_decision: i64 = c.query_row("SELECT count(*) FROM research_attempts WHERE decision_id = ?1 AND route != 'csfloat' AND outcome != 'not_sent'", [id], |r| r.get(0))?;
    Ok(exploratory < MAX_EXPLORATORY
        && normal >= 15 * (exploratory + 1)
        && per_decision < MAX_PER_DECISION)
}

pub(crate) async fn check_budget() -> Result<(), ReserveError> {
    let Some(context) = current().filter(|c| c.source == "exploracao_externa") else {
        return Ok(());
    };
    let id = context.id;
    let allowed = context
        .db
        .call(move |c| eligible(c, id))
        .await
        .map_err(ReserveError::Database)?;
    if allowed {
        Ok(())
    } else {
        Err(ReserveError::Budget)
    }
}

pub(crate) async fn reserve(
    db: Option<Db>,
    endpoint: &'static str,
    route: &'static str,
) -> Result<Option<(Db, i64)>, ReserveError> {
    let context = current();
    let db = context.as_ref().map(|c| c.db.clone()).or(db);
    let Some(db) = db else { return Ok(None) };
    let (id, source) = context
        .map(|c| (Some(c.id), c.source))
        .unwrap_or((None, "background".to_owned()));
    let db2 = db.clone();
    let attempt = db.call(move |c| {
        let tx = c.transaction()?;
        let now = now_unix();
        if source == "exploracao_externa" && !eligible(&tx, id.expect("exploração requer contexto"))? {
            return Ok(None);
        }
        tx.execute("INSERT INTO research_attempts(decision_id,at,source,endpoint,route,outcome) VALUES (?1,?2,?3,?4,?5,'started')", params![id,now,source,endpoint,route])?;
        let row = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(row))
    }).await.map_err(ReserveError::Database)?;
    attempt
        .map(|id| Some((db2, id)))
        .ok_or(ReserveError::Budget)
}

pub(crate) async fn finish(attempt: Option<(Db, i64)>, outcome: &'static str) {
    if let Some((db, id)) = attempt
        && let Err(e) = db
            .call(move |c| {
                c.execute(
                    "UPDATE research_attempts SET outcome=?1 WHERE id=?2",
                    params![outcome, id],
                )?;
                Ok(())
            })
            .await
    {
        tracing::warn!("registro de pesquisa falhou: {e:#}");
    }
}

/// Contagem separada do transporte CSFloat; chamar imediatamente antes do envio.
pub async fn record_external(db: Db) -> anyhow::Result<()> {
    let c = current();
    let (id, source) = c
        .map(|x| (Some(x.id), x.source))
        .unwrap_or((None, "background".into()));
    db.call(move |c| { c.execute("INSERT INTO research_attempts(decision_id,at,source,endpoint,route,outcome) VALUES (?1,?2,?3,'sales','csfloat','started')", params![id,now_unix(),source])?; Ok(()) }).await
}

pub fn retain(c: &Connection, now: i64) -> rusqlite::Result<()> {
    let cutoff = now - RETAIN_DAYS * 86400;
    let mature = now - MATURATION_DAYS * 86400;
    c.execute("DELETE FROM research_attempts WHERE at < ?1 OR (at < ?2 AND id NOT IN (SELECT id FROM research_attempts ORDER BY id DESC LIMIT ?3) AND (decision_id IS NULL OR decision_id NOT IN (SELECT decision_id FROM ghost_decisions)))", params![cutoff,mature,MAX_ROWS])?;
    c.execute("DELETE FROM research_decisions WHERE (at < ?1 OR (at < ?2 AND id NOT IN (SELECT id FROM research_decisions ORDER BY id DESC LIMIT ?3) AND id NOT IN (SELECT decision_id FROM ghost_decisions))) AND id NOT IN (SELECT decision_id FROM research_attempts WHERE decision_id IS NOT NULL)", params![cutoff,mature,MAX_ROWS])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exploratory() -> Decision {
        Decision {
            source: "exploracao_externa".into(),
            policy: "test".into(),
            target: "x".into(),
            scale: 1.0,
            block_id: Some(1),
        }
    }

    #[tokio::test]
    async fn budget_is_atomic_and_survives_reopen() {
        let (db, dir) = crate::db::temp_db();
        for _ in 0..15 {
            reserve(Some(db.clone()), "listings", "main").await.unwrap();
        }
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let db = db.clone();
            tasks.push(tokio::spawn(async move {
                scope(db.clone(), exploratory(), async move {
                    Ok(reserve(Some(db), "listings", "main").await.is_ok())
                })
                .await
                .unwrap()
            }));
        }
        let mut allowed = 0;
        for task in tasks {
            allowed += i32::from(task.await.unwrap());
        }
        assert_eq!(allowed, 1);
        drop(db);
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        assert!(
            !scope(db.clone(), exploratory(), async move {
                Ok(reserve(Some(db), "listings", "main").await.is_ok())
            })
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn decision_cap_applies_to_retries_and_followups_are_exempt() {
        let (db, _dir) = crate::db::temp_db();
        for _ in 0..90 {
            reserve(Some(db.clone()), "history", "main").await.unwrap();
        }
        let (_, context) = scope_tracked(db.clone(), exploratory(), async {
            for route in ["alt", "main", "main"] {
                assert!(reserve(None, "history", route).await.is_ok());
            }
            assert!(matches!(
                reserve(None, "history", "main").await,
                Err(ReserveError::Budget)
            ));
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(attempts(db.clone(), context.id).await.unwrap(), 3);
        let mut d = exploratory();
        d.source = "acompanhamento".into();
        scope(db, d, async {
            for _ in 0..4 {
                assert!(reserve(None, "history", "main").await.is_ok());
            }
            Ok(())
        })
        .await
        .unwrap();
    }
}
