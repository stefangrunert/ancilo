//! One index = one SQLite file: files, chunks (with embeddings as BLOBs) and
//! an FTS5 table over path, symbol and text.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ancilo_core::{Error, Result};
use ancilo_storage::rusqlite::{Connection, OptionalExtension, params};

use crate::chunk::{Chunk, split_identifier};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS files (
    path  TEXT PRIMARY KEY,
    size  INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    hash  TEXT NOT NULL,
    lang  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS chunks (
    id         INTEGER PRIMARY KEY,
    path       TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    symbol     TEXT,
    kind       TEXT NOT NULL,
    text       TEXT NOT NULL,
    embedding  BLOB
);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(path, symbol, body);
"#;

/// A stored chunk.
#[derive(Debug, Clone)]
pub struct Row {
    pub id: i64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileState {
    pub size: u64,
    pub mtime: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

fn db_err(e: impl std::fmt::Display) -> Error {
    Error::internal(format!("index database: {e}"))
}

/// Text for the full-text index: the chunk plus its symbol and path as words
/// (`parseHttpRequest` is also found by "parse request").
fn fts_body(path: &str, c: &Chunk) -> String {
    format!(
        "{}\n{}\n{}",
        c.text,
        c.symbol
            .as_deref()
            .map(split_identifier)
            .unwrap_or_default(),
        split_identifier(path)
    )
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")
            .map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> ancilo_storage::rusqlite::Result<T>,
    ) -> Result<T> {
        let mut c = self.conn.lock().unwrap();
        f(&mut c).map_err(db_err)
    }

    /// A consistent copy of the whole index (seed for a worktree's index).
    pub fn copy_to(&self, path: &Path) -> Result<()> {
        let target = path.display().to_string();
        self.with(|c| c.execute("VACUUM INTO ?1", params![target]).map(|_| ()))
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
        })
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
        })
    }

    /// path → (size, mtime, hash)
    pub fn files(&self) -> Result<HashMap<String, (FileState, String)>> {
        self.with(|c| {
            let mut s = c.prepare("SELECT path, size, mtime, hash FROM files")?;
            let rows = s.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (
                        FileState {
                            size: r.get::<_, i64>(1)? as u64,
                            mtime: r.get(2)?,
                        },
                        r.get::<_, String>(3)?,
                    ),
                ))
            })?;
            rows.collect()
        })
    }

    /// Replaces the chunks of a file.
    pub fn put_file(
        &self,
        path: &str,
        lang: &str,
        state: FileState,
        hash: &str,
        chunks: &[Chunk],
    ) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            delete_chunks(&tx, path)?;
            for ch in chunks {
                tx.execute(
                    "INSERT INTO chunks(path, start_line, end_line, symbol, kind, text) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![path, ch.start_line as i64, ch.end_line as i64, ch.symbol, ch.kind, ch.text],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO chunks_fts(rowid, path, symbol, body) VALUES(?1, ?2, ?3, ?4)",
                    params![id, path, ch.symbol.clone().unwrap_or_default(), fts_body(path, ch)],
                )?;
            }
            tx.execute(
                "INSERT INTO files(path, size, mtime, hash, lang) VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, hash = excluded.hash, lang = excluded.lang",
                params![path, state.size as i64, state.mtime, hash, lang],
            )?;
            tx.commit()
        })
    }

    /// Only the file state changed (same content).
    pub fn touch_file(&self, path: &str, state: FileState) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE files SET size = ?2, mtime = ?3 WHERE path = ?1",
                params![path, state.size as i64, state.mtime],
            )
            .map(|_| ())
        })
    }

    pub fn remove_file(&self, path: &str) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            delete_chunks(&tx, path)?;
            tx.execute("DELETE FROM files WHERE path = ?1", params![path])?;
            tx.commit()
        })
    }

    /// Chunks without an embedding: (id, text to embed).
    pub fn unembedded(&self, limit: usize) -> Result<Vec<(i64, String)>> {
        self.with(|c| {
            let mut s = c.prepare(
                "SELECT id, path, symbol, text FROM chunks WHERE embedding IS NULL ORDER BY id LIMIT ?1",
            )?;
            let rows = s.query_map(params![limit as i64], |r| {
                let (path, symbol, text): (String, Option<String>, String) =
                    (r.get(1)?, r.get(2)?, r.get(3)?);
                Ok((r.get(0)?, embed_text(&path, symbol.as_deref(), &text)))
            })?;
            rows.collect()
        })
    }

    pub fn set_embeddings(&self, items: &[(i64, Vec<f32>)]) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            for (id, v) in items {
                tx.execute(
                    "UPDATE chunks SET embedding = ?2 WHERE id = ?1",
                    params![id, to_blob(v)],
                )?;
            }
            tx.commit()
        })
    }

    pub fn clear_embeddings(&self) -> Result<()> {
        self.with(|c| {
            c.execute("UPDATE chunks SET embedding = NULL", [])
                .map(|_| ())
        })
    }

    pub fn vectors(&self) -> Result<Vec<(i64, Vec<f32>)>> {
        self.with(|c| {
            let mut s =
                c.prepare("SELECT id, embedding FROM chunks WHERE embedding IS NOT NULL")?;
            let rows = s.query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?)))
            })?;
            rows.collect()
        })
    }

    /// Full-text candidates, best first (BM25; path and symbol weigh more).
    pub fn fts(&self, query: &str, limit: usize) -> Result<Vec<i64>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        self.with(|c| {
            let mut s = c.prepare(
                "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts, 2.0, 4.0, 1.0) LIMIT ?2",
            )?;
            let rows = s.query_map(params![query, limit as i64], |r| r.get(0))?;
            rows.collect()
        })
    }

    pub fn rows(&self, ids: &[i64]) -> Result<Vec<Row>> {
        self.with(|c| {
            let mut s = c.prepare(
                "SELECT id, path, start_line, end_line, symbol, kind, text FROM chunks WHERE id = ?1",
            )?;
            let mut out = Vec::new();
            for id in ids {
                if let Some(row) = s
                    .query_row(params![id], |r| {
                        Ok(Row {
                            id: r.get(0)?,
                            path: r.get(1)?,
                            start_line: r.get::<_, i64>(2)? as usize,
                            end_line: r.get::<_, i64>(3)? as usize,
                            symbol: r.get(4)?,
                            kind: r.get(5)?,
                            text: r.get(6)?,
                        })
                    })
                    .optional()?
                {
                    out.push(row);
                }
            }
            Ok(out)
        })
    }

    /// Chunks whose symbol (or its last segment) equals `name`, case-insensitively.
    pub fn by_symbol(&self, name: &str) -> Result<Vec<i64>> {
        let lower = name.to_lowercase();
        self.with(|c| {
            let mut s = c.prepare(
                "SELECT id FROM chunks WHERE lower(symbol) = ?1 OR lower(symbol) LIKE ?2 OR lower(symbol) LIKE ?3 LIMIT 20",
            )?;
            let rows = s.query_map(
                params![lower, format!("%::{lower}"), format!("%.{lower}")],
                |r| r.get(0),
            )?;
            rows.collect()
        })
    }

    /// (files, chunks, chunks with an embedding)
    pub fn counts(&self) -> Result<(u64, u64, u64)> {
        self.with(|c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM files), (SELECT count(*) FROM chunks), (SELECT count(*) FROM chunks WHERE embedding IS NOT NULL)",
                [],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64)),
            )
        })
    }
}

fn delete_chunks(
    tx: &ancilo_storage::rusqlite::Transaction<'_>,
    path: &str,
) -> ancilo_storage::rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE path = ?1)",
        params![path],
    )?;
    tx.execute("DELETE FROM chunks WHERE path = ?1", params![path])?;
    Ok(())
}

/// What is embedded for a chunk: location and symbol give context; the text
/// is cut to what small embedding models read (~250 tokens).
pub fn embed_text(path: &str, symbol: Option<&str>, text: &str) -> String {
    let head: String = text.chars().take(1000).collect();
    match symbol {
        Some(s) => format!("{path} {s}\n{head}"),
        None => format!("{path}\n{head}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::chunk_file;

    #[test]
    fn files_can_be_replaced_searched_and_removed() {
        let s = Store::in_memory().unwrap();
        let src = "/// Checks it.\npub fn verify_token(t: &str) -> bool { !t.is_empty() }\n";
        let st = FileState { size: 10, mtime: 1 };
        s.put_file(
            "src/auth.rs",
            "rust",
            st,
            "h1",
            &chunk_file("src/auth.rs", src),
        )
        .unwrap();
        assert_eq!(s.counts().unwrap(), (1, 1, 0));
        let hit = s.fts("\"verify\"* OR \"token\"*", 10).unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(s.by_symbol("VERIFY_TOKEN").unwrap(), hit);
        // Replacing a file replaces its chunks (no duplicates in FTS).
        s.put_file(
            "src/auth.rs",
            "rust",
            st,
            "h2",
            &chunk_file("src/auth.rs", "pub fn other() {}\n"),
        )
        .unwrap();
        assert!(s.fts("\"verify\"", 10).unwrap().is_empty());
        assert_eq!(s.counts().unwrap().1, 1);
        let pending = s.unembedded(10).unwrap();
        assert!(pending[0].1.starts_with("src/auth.rs other\n"));
        s.set_embeddings(&[(pending[0].0, vec![0.5, -1.0])])
            .unwrap();
        assert_eq!(s.vectors().unwrap()[0].1, vec![0.5, -1.0]);
        s.remove_file("src/auth.rs").unwrap();
        assert_eq!(s.counts().unwrap(), (0, 0, 0));
        assert!(s.fts("\"other\"", 10).unwrap().is_empty());
    }
}
