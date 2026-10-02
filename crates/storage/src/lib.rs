//! SQLite storage with versioned migrations.
//!
//! One database file per Ancilo home. Access is serialized through a mutex –
//! write volume is low and SQLite in WAL mode handles it well. Async callers
//! use [`Db::call`], which runs the closure on the blocking thread pool.

use std::path::Path;
use std::sync::{Arc, Mutex};

use ancilo_core::{Error, Event, EventSink, Result};
use rusqlite::{Connection, OptionalExtension, params};

mod migrations;

pub use rusqlite;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::Internal(format!("database error: {e}"))
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path).map_err(db_err)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )
        .map_err(db_err)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_err)?;
        let backup = path.with_file_name(format!(
            "{}.bak-v{version}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        migrations::apply_with(&mut conn, migrations::MIGRATIONS, Some(&backup))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(db_err)?;
        Self::init(conn)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        migrations::apply(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs a closure with exclusive access to the connection (blocking).
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| Error::internal("database lock poisoned"))?;
        f(&conn).map_err(db_err)
    }

    /// Async variant of [`Db::with`] on the blocking pool.
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.with(f))
            .await
            .map_err(Error::internal)?
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.with(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
    }

    // ---- settings -------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }

    // ---- events ---------------------------------------------------------

    pub fn last_event_seq(&self) -> Result<i64> {
        self.with(|c| c.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0)))
    }

    /// Events after `after_seq`, oldest first, optionally filtered by kind prefix.
    pub fn events_since(
        &self,
        after_seq: i64,
        kind_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let prefix = kind_prefix.map(|p| format!("{p}%")).unwrap_or("%".into());
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, ts, kind, subject, data FROM events
                 WHERE seq > ?1 AND kind LIKE ?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![after_seq, prefix, limit as i64], row_to_event)?;
            rows.collect()
        })
    }
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let ts: String = r.get(1)?;
    let data: String = r.get(4)?;
    Ok(Event {
        seq: r.get(0)?,
        ts: chrono::DateTime::parse_from_rfc3339(&ts)
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default(),
        kind: r.get(2)?,
        subject: r.get(3)?,
        data: serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
    })
}

impl EventSink for Db {
    fn persist(&self, e: &Event) -> Result<()> {
        let data = e.data.to_string();
        let ts = e.ts.to_rfc3339();
        self.with(|c| {
            c.execute(
                "INSERT INTO events(seq, ts, kind, subject, data) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![e.seq, ts, e.kind, e.subject, data],
            )
            .map(|_| ())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ancilo_core::EventBus;
    use serde_json::json;

    #[test]
    fn migrates_fresh_database() {
        let db = Db::in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::LATEST);
    }

    /// The knowledge index (M5) relies on FTS5 in the bundled SQLite.
    #[test]
    fn bundled_sqlite_has_fts5() {
        let db = Db::in_memory().unwrap();
        let n: i64 = db
            .with(|c| {
                c.execute_batch(
                    "CREATE VIRTUAL TABLE t USING fts5(body); INSERT INTO t(body) VALUES('fn verify_token() {}');",
                )?;
                c.query_row("SELECT count(*) FROM t WHERE t MATCH 'verify_token'", [], |r| r.get(0))
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn reopening_keeps_data_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.db");
        Db::open(&path).unwrap().set_setting("k", "v").unwrap();
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("v"));
        assert_eq!(db.schema_version().unwrap(), migrations::LATEST);
    }

    // covers: M9-AC-03
    #[test]
    fn an_older_database_is_backed_up_then_migrated_with_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ancilo.db");
        {
            // A database of the first release: schema version 1 with a setting.
            let mut c = Connection::open(&path).unwrap();
            migrations::apply_with(&mut c, &migrations::MIGRATIONS[..1], None).unwrap();
            c.execute("INSERT INTO settings(key, value) VALUES('k', 'v')", [])
                .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), migrations::LATEST);
        assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("v"));
        let backup = Connection::open(dir.path().join("ancilo.db.bak-v1")).unwrap();
        let v: i64 = backup
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
        // Opening an up-to-date database makes no new backup.
        drop(db);
        Db::open(&path).unwrap();
        assert!(
            !dir.path()
                .join(format!("ancilo.db.bak-v{}", migrations::LATEST))
                .exists()
        );
    }

    // covers: M9-AC-03
    #[test]
    fn a_failing_migration_leaves_the_database_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.db");
        let mut c = Connection::open(&path).unwrap();
        migrations::apply_with(&mut c, &["CREATE TABLE a(x);"], None).unwrap();
        c.execute("INSERT INTO a(x) VALUES(1)", []).unwrap();
        let backup = dir.path().join("x.db.bak");
        let err = migrations::apply_with(
            &mut c,
            &[
                "CREATE TABLE a(x);",
                "CREATE TABLE b(y); INSERT INTO nope VALUES(1);",
            ],
            Some(&backup),
        )
        .unwrap_err();
        assert!(
            err.message().contains("unchanged at version 1"),
            "{}",
            err.message()
        );
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
        let b: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b, 0, "the failed step left nothing behind");
        assert!(backup.exists());
        // A newer database is refused by an older Ancilo.
        let err = migrations::apply_with(&mut c, &[], None).unwrap_err();
        assert!(err.message().contains("newer than this Ancilo"));
    }

    #[test]
    fn persists_events_from_bus() {
        let db = Db::in_memory().unwrap();
        let bus = EventBus::new(Some(Arc::new(db.clone())), db.last_event_seq().unwrap());
        bus.emit("download.progress", Some("m1"), json!({"bytes": 10}));
        bus.emit("instance.ready", Some("m1"), json!({}));
        let all = db.events_since(0, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        let only = db.events_since(0, Some("instance."), 10).unwrap();
        assert_eq!(only[0].kind, "instance.ready");
        assert_eq!(db.last_event_seq().unwrap(), 2);
    }
}
