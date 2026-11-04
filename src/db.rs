//! db — SQLite (WAL) com migrações por `user_version` e retenção de hora em hora.
//! Uma conexão só, sempre usada dentro de `spawn_blocking`: o runtime nunca faz I/O síncrono.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;
/// Teto de notas de alvos guardadas (cada uma é uma linha pequena).
const ARM_STATS_MAX: i64 = 2_000;

/// Cada item é uma migração; `user_version` = quantas já foram aplicadas.
const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE items (
        name TEXT PRIMARY KEY, g_code TEXT, paint_index INTEGER,
        floor_cents INTEGER, sales_per_day REAL, updated_at INTEGER NOT NULL
    );
    CREATE TABLE price_points (
        name TEXT NOT NULL, ts INTEGER NOT NULL,
        median_cents INTEGER NOT NULL, purchases INTEGER NOT NULL,
        PRIMARY KEY (name, ts)
    );
    CREATE INDEX price_points_ts ON price_points(ts);
    CREATE TABLE listings_seen (
        listing_id TEXT PRIMARY KEY, name TEXT NOT NULL, price_cents INTEGER NOT NULL,
        seed INTEGER, "float" REAL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL
    );
    CREATE INDEX listings_seen_last ON listings_seen(last_seen);
    CREATE TABLE opportunities (
        id TEXT PRIMARY KEY, json TEXT NOT NULL, created_at INTEGER NOT NULL, gone_at INTEGER
    );
    CREATE TABLE purchases (id TEXT PRIMARY KEY, json TEXT NOT NULL, created_at INTEGER NOT NULL);
    CREATE TABLE outcomes (purchase_id TEXT NOT NULL, sold_cents INTEGER NOT NULL, sold_at INTEGER NOT NULL);
    "#,
    // Caça: catálogo (varredura por popularidade, histórico) e o que a compra precisa para refazer o pedido.
    r#"
    ALTER TABLE items ADD COLUMN floor_usd_cents INTEGER;
    ALTER TABLE items ADD COLUMN dispersion REAL;
    ALTER TABLE items ADD COLUMN history_at INTEGER;
    ALTER TABLE items ADD COLUMN icon_url TEXT;
    ALTER TABLE items ADD COLUMN rank INTEGER;
    ALTER TABLE opportunities ADD COLUMN task TEXT;
    ALTER TABLE opportunities ADD COLUMN listing_json TEXT;
    "#,
    r#"
    CREATE TABLE hourly_activity (hour_utc INTEGER PRIMARY KEY, listings INTEGER NOT NULL DEFAULT 0, opportunities INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE activity_seen (listing_id TEXT PRIMARY KEY, last_seen INTEGER NOT NULL);
    CREATE INDEX activity_seen_last ON activity_seen(last_seen);
    INSERT INTO activity_seen(listing_id, last_seen) SELECT listing_id, last_seen FROM listings_seen;
    "#,
    // Agenda que aprende: nota de cada alvo (lucro esperado por consulta gasta), para sobreviver a reinício.
    r#"
    CREATE TABLE arm_stats (
        id TEXT PRIMARY KEY, n REAL NOT NULL, r REAL NOT NULL, queries INTEGER NOT NULL,
        finds INTEGER NOT NULL, gain_cents INTEGER NOT NULL, near_hits INTEGER NOT NULL,
        first_at INTEGER NOT NULL, reward_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
    );
    "#,
];

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    /// Bloqueante: chame de dentro de `spawn_blocking` (ou em teste).
    pub fn open(path: &Path) -> Result<Db> {
        let mut conn =
            Connection::open(path).with_context(|| format!("abrir {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // auto_vacuum só vale antes da primeira tabela; permite devolver espaço sem VACUUM inteiro.
        conn.execute_batch(
            "PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )?;
        migrate(&mut conn)?;
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let db = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut conn).map_err(anyhow::Error::from)
        })
        .await?
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Retained {
    pub price_points: usize,
    pub listings_seen: usize,
    pub opportunities: usize,
    pub arm_stats: usize,
}

/// price_points > 31 dias, listings_seen > 3 dias, oportunidades encerradas há > 7 dias, notas da agenda sem uso
/// há > 14 dias (e no máximo `ARM_STATS_MAX`, as mais recentes).
pub fn retain(conn: &Connection, now: i64) -> rusqlite::Result<Retained> {
    let r = Retained {
        price_points: conn.execute("DELETE FROM price_points WHERE ts < ?1", [now - 31 * DAY])?,
        listings_seen: conn.execute(
            "DELETE FROM listings_seen WHERE last_seen < ?1",
            [now - 3 * DAY],
        )?,
        opportunities: conn.execute(
            "DELETE FROM opportunities WHERE gone_at IS NOT NULL AND gone_at < ?1",
            [now - 7 * DAY],
        )?,
        arm_stats: conn.execute(
            "DELETE FROM arm_stats WHERE updated_at < ?1 OR id NOT IN
             (SELECT id FROM arm_stats ORDER BY updated_at DESC LIMIT ?2)",
            rusqlite::params![now - 14 * DAY, ARM_STATS_MAX],
        )?,
    };
    conn.execute(
        "DELETE FROM hourly_activity WHERE hour_utc < ?1",
        [(now - 30 * DAY).div_euclid(3600)],
    )?;
    conn.execute(
        "DELETE FROM activity_seen WHERE last_seen < ?1",
        [now - 3 * DAY],
    )?;
    conn.execute_batch("PRAGMA incremental_vacuum;")?;
    Ok(r)
}

/// Roda a retenção agora e depois a cada hora.
pub fn spawn_retention(db: Db) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match db.call(|c| retain(c, now_unix())).await {
                Ok(r) => tracing::info!(?r, "retenção do banco"),
                Err(e) => tracing::warn!("retenção do banco falhou: {e:#}"),
            }
        }
    });
}

#[cfg(test)]
pub fn temp_db() -> (Db, tempfile_dir::Dir) {
    let dir = tempfile_dir::Dir::new();
    (Db::open(&dir.path().join("t.db")).unwrap(), dir)
}

/// Diretório temporário sem crate: apagado ao sair do escopo.
#[cfg(test)]
pub mod tempfile_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub struct Dir(PathBuf);
    impl Dir {
        pub fn new() -> Dir {
            static N: AtomicU32 = AtomicU32::new(0);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!(
                "sniper-test-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                timestamp
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_and_is_idempotent() {
        let dir = tempfile_dir::Dir::new();
        let path = dir.path().join("m.db");
        drop(Db::open(&path).unwrap());
        let db = Db::open(&path).unwrap(); // segunda abertura não reaplica
        let conn = db.0.lock().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        for t in [
            "settings",
            "items",
            "price_points",
            "listings_seen",
            "opportunities",
            "purchases",
            "outcomes",
            "arm_stats",
        ] {
            conn.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap();
        }
    }

    #[test]
    fn migration_preserves_seen_listings_for_activity() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.execute(
            "INSERT INTO listings_seen VALUES ('existing', 'x', 100, NULL, NULL, 1, 2)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        migrate(&mut conn).unwrap();
        let last: i64 = conn
            .query_row(
                "SELECT last_seen FROM activity_seen WHERE listing_id = 'existing'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last, 2);
    }

    #[test]
    fn retention_drops_only_old_rows() {
        let (db, _dir) = temp_db();
        let conn = db.0.lock().unwrap();
        let now = 100 * DAY;
        for (ts, n) in [(now - 32 * DAY, "old"), (now - 30 * DAY, "new")] {
            conn.execute("INSERT INTO price_points VALUES (?1, ?2, 100, 1)", (n, ts))
                .unwrap();
        }
        for (id, seen) in [("a", now - 4 * DAY), ("b", now - 2 * DAY)] {
            conn.execute(
                "INSERT INTO listings_seen VALUES (?1, 'x', 1, NULL, NULL, ?2, ?2)",
                (id, seen),
            )
            .unwrap();
        }
        for (id, gone) in [
            ("gone-old", Some(now - 8 * DAY)),
            ("gone-new", Some(now - DAY)),
            ("active", None),
        ] {
            conn.execute(
                "INSERT INTO opportunities(id, json, created_at, gone_at) VALUES (?1, '{}', 0, ?2)",
                (id, gone),
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO hourly_activity VALUES (?1, 1, 0)",
            [(now - 31 * DAY).div_euclid(3600)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO hourly_activity VALUES (?1, 2, 1)",
            [(now - 29 * DAY).div_euclid(3600)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO activity_seen VALUES ('old', ?1)",
            [now - 4 * DAY],
        )
        .unwrap();
        conn.execute("INSERT INTO activity_seen VALUES ('new', ?1)", [now - DAY])
            .unwrap();
        for (id, at) in [("arm-old", now - 15 * DAY), ("arm-new", now - DAY)] {
            conn.execute(
                "INSERT INTO arm_stats VALUES (?1, 1, 1, 1, 0, 0, 0, 0, 0, ?2)",
                (id, at),
            )
            .unwrap();
        }
        let r = retain(&conn, now).unwrap();
        assert_eq!(
            r,
            Retained {
                price_points: 1,
                listings_seen: 1,
                opportunities: 1,
                arm_stats: 1
            }
        );
        let left: i64 = conn
            .query_row("SELECT count(*) FROM opportunities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 2);
        let activity: i64 = conn
            .query_row("SELECT count(*) FROM hourly_activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(activity, 1);
        let seen: i64 = conn
            .query_row("SELECT count(*) FROM activity_seen", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seen, 1);
    }
}
