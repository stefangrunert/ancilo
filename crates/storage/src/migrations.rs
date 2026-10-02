//! Versioned schema migrations, tracked in `PRAGMA user_version`.
//! Migrations are append-only: never edit a released one.

use ancilo_core::{Error, Result};
use rusqlite::Connection;

pub(crate) const MIGRATIONS: &[&str] = &[
    // 1: settings, events, model library, roles
    r#"
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE events (
        seq     INTEGER PRIMARY KEY,
        ts      TEXT NOT NULL,
        kind    TEXT NOT NULL,
        subject TEXT,
        data    TEXT NOT NULL
    );
    CREATE INDEX events_kind ON events(kind);
    CREATE INDEX events_subject ON events(subject);
    CREATE TABLE models (
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        source      TEXT NOT NULL,
        file_path   TEXT,
        size_bytes  INTEGER,
        sha256      TEXT,
        quant       TEXT,
        state       TEXT NOT NULL,
        plan        TEXT,
        pinned      INTEGER NOT NULL DEFAULT 0,
        added_at    TEXT NOT NULL,
        meta        TEXT NOT NULL DEFAULT '{}'
    );
    CREATE TABLE roles (
        role     TEXT PRIMARY KEY,
        model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE
    );
    "#,
    // 2: inference log (gateway metrics)
    r#"
    CREATE TABLE inference_log (
        id                INTEGER PRIMARY KEY,
        ts                TEXT NOT NULL,
        model             TEXT NOT NULL,
        api               TEXT NOT NULL,
        priority          TEXT NOT NULL,
        stream            INTEGER NOT NULL,
        tools             INTEGER NOT NULL,
        latency_ms        INTEGER NOT NULL,
        queue_ms          INTEGER NOT NULL,
        load_ms           INTEGER NOT NULL,
        prompt_tokens     INTEGER,
        completion_tokens INTEGER,
        tokens_per_sec    REAL,
        stages            TEXT NOT NULL,
        outcome           TEXT NOT NULL,
        error             TEXT
    );
    CREATE INDEX inference_log_model ON inference_log(model, ts);
    "#,
    // 3: tasks (delegation)
    r#"
    CREATE TABLE tasks (
        id          TEXT PRIMARY KEY,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        status      TEXT NOT NULL,
        mode        TEXT NOT NULL,
        request     TEXT NOT NULL,
        result      TEXT
    );
    CREATE INDEX tasks_status ON tasks(status, created_at);
    "#,
    // 4: routing by task kind, A/B tests, comparisons, recommendations (M4)
    r#"
    CREATE TABLE routes (
        kind     TEXT PRIMARY KEY,
        model_id TEXT NOT NULL REFERENCES models(id) ON DELETE CASCADE
    );
    CREATE TABLE ab_tests (
        id          TEXT PRIMARY KEY,
        role        TEXT NOT NULL,
        model_a     TEXT NOT NULL,
        model_b     TEXT NOT NULL,
        share       REAL NOT NULL,
        seed        INTEGER NOT NULL,
        min_samples INTEGER NOT NULL,
        guard_min   INTEGER NOT NULL,
        shadow      INTEGER NOT NULL,
        status      TEXT NOT NULL,
        next_seq    INTEGER NOT NULL DEFAULT 0,
        started_at  TEXT NOT NULL,
        ended_at    TEXT,
        end_reason  TEXT
    );
    CREATE UNIQUE INDEX ab_tests_running ON ab_tests(role) WHERE status = 'running';
    CREATE TABLE ab_assignments (
        test_id       TEXT NOT NULL REFERENCES ab_tests(id) ON DELETE CASCADE,
        seq           INTEGER NOT NULL,
        arm           TEXT NOT NULL,
        model_id      TEXT NOT NULL,
        source        TEXT NOT NULL,
        subject       TEXT,
        ts            TEXT NOT NULL,
        success       INTEGER,
        latency_ms    INTEGER,
        error         INTEGER,
        interventions INTEGER,
        PRIMARY KEY (test_id, seq, arm)
    );
    CREATE TABLE comparisons (
        id         TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        status     TEXT NOT NULL,
        request    TEXT NOT NULL,
        state      TEXT NOT NULL
    );
    CREATE TABLE compare_runs (
        id            INTEGER PRIMARY KEY,
        comparison_id TEXT NOT NULL REFERENCES comparisons(id) ON DELETE CASCADE,
        ts            TEXT NOT NULL,
        model_id      TEXT NOT NULL,
        label         TEXT NOT NULL,
        kind          TEXT NOT NULL,
        suite_task    TEXT,
        idx           INTEGER NOT NULL,
        success       INTEGER NOT NULL,
        data          TEXT NOT NULL
    );
    CREATE INDEX compare_runs_model ON compare_runs(model_id, kind);
    CREATE TABLE recommendations (
        id         TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        status     TEXT NOT NULL,
        data       TEXT NOT NULL
    );
    "#,
    // 5: coding sessions (M8)
    r#"
    CREATE TABLE sessions (
        id         TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        project    TEXT NOT NULL,
        status     TEXT NOT NULL,
        meta       TEXT NOT NULL,
        history    TEXT NOT NULL
    );
    CREATE INDEX sessions_project ON sessions(project, updated_at);
    "#,
    // 6: "retry with another model" – which result the user took (M8, H8-2)
    r#"
    CREATE TABLE variant_choices (
        at      TEXT NOT NULL,
        session TEXT NOT NULL,
        chosen  TEXT NOT NULL,
        over    TEXT NOT NULL
    );
    "#,
    // 7: conversations with the assistant and the app's project list
    r#"
    CREATE TABLE conversations (
        id         TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        title      TEXT NOT NULL,
        messages   TEXT NOT NULL
    );
    CREATE INDEX conversations_updated ON conversations(updated_at);
    CREATE TABLE projects (
        root      TEXT PRIMARY KEY,
        opened_at TEXT NOT NULL
    );
    "#,
    // 8: what a conversation was started for (chat, setup, write, …)
    r#"
    ALTER TABLE conversations ADD COLUMN kind TEXT NOT NULL DEFAULT 'setup';
    "#,
    // 9: projects and sessions renamed and ordered by hand
    r#"
    ALTER TABLE projects ADD COLUMN name TEXT;
    ALTER TABLE projects ADD COLUMN position INTEGER;
    ALTER TABLE sessions ADD COLUMN position INTEGER;
    "#,
];

#[cfg(test)]
pub const LATEST: i64 = MIGRATIONS.len() as i64;

pub fn apply(conn: &mut Connection) -> Result<()> {
    apply_with(conn, MIGRATIONS, None)
}

/// Applies `migrations` from the current version on. Each one runs in a
/// transaction: a failing step changes nothing and leaves the database at the
/// previous version. An existing database file is backed up first
/// (`backup`: the path to use), so even a logically wrong migration can be
/// undone by hand.
pub(crate) fn apply_with(
    conn: &mut Connection,
    migrations: &[&str],
    backup: Option<&std::path::Path>,
) -> Result<()> {
    let latest = migrations.len() as i64;
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| Error::internal(format!("reading schema version: {e}")))?;
    if current > latest {
        return Err(Error::Conflict(format!(
            "database schema version {current} is newer than this Ancilo ({latest}); please update Ancilo"
        )));
    }
    if current > 0
        && current < latest
        && let Some(b) = backup
    {
        std::fs::remove_file(b).ok();
        conn.execute("VACUUM INTO ?1", [b.display().to_string()])
            .map_err(|e| {
                Error::internal(format!("backing up the database before the update: {e}"))
            })?;
        tracing::info!(from = current, to = latest, backup = %b.display(), "database backed up before migration");
    }
    for (i, sql) in migrations.iter().enumerate().skip(current as usize) {
        let version = i as i64 + 1;
        let tx = conn
            .transaction()
            .map_err(|e| Error::internal(format!("migration {version}: {e}")))?;
        tx.execute_batch(sql)
            .and_then(|_| tx.execute_batch(&format!("PRAGMA user_version = {version}")))
            .and_then(|_| tx.commit())
            .map_err(|e| {
                Error::internal(format!(
                    "database update to version {version} failed – the database is unchanged at version {}{}: {e}",
                    version - 1,
                    backup.map(|b| format!(" (backup: {})", b.display())).unwrap_or_default()
                ))
            })?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}
