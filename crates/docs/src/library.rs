//! The documents of a chat project (decision `2026-10-03-drei-bereiche`): a
//! folder Ancilo only reads. Every document in it is read once – in the
//! sandboxed reader – and kept as text; changed files are read again, gone
//! ones forgotten. What could not be read is listed with the reason, never
//! silently left out. Chats in the project take the passages that fit, each
//! with its source (file and page or sheet).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ancilo_core::{Error, EventBus, Result};
use ancilo_storage::Db;
use chrono::Utc;
use rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::extract::{self, Part};
use crate::{Extractor, PASSAGE_BUDGET};

/// Documents read in one folder at most.
pub const MAX_FILES: usize = 5_000;
/// Folders never looked into.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    "Library",
];

/// A file that was not read, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NotRead {
    /// Relative to the folder.
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LibraryStatus {
    pub folder: PathBuf,
    /// Documents read (with text).
    pub read: usize,
    /// Documents still to read in a running pass.
    pub pending: usize,
    /// Reading is going on now.
    pub reading: bool,
    /// Characters of text kept.
    pub chars: usize,
    /// Files that could not be read – with the reason.
    pub not_read: Vec<NotRead>,
    /// More documents than Ancilo reads in one folder ([`MAX_FILES`]).
    #[serde(default)]
    pub too_many: bool,
}

#[derive(Clone)]
pub struct Library {
    db: Db,
    extractor: Arc<Extractor>,
    bus: Option<EventBus>,
    /// Passes running now (folder → files left).
    running: Arc<Mutex<HashMap<PathBuf, usize>>>,
    /// What the last pass found besides the database (folder → too many).
    too_many: Arc<Mutex<HashSet<PathBuf>>>,
}

/// A supported document found in a folder.
struct Found {
    rel: String,
    path: PathBuf,
    size: u64,
    mtime: i64,
}

impl Library {
    pub fn new(db: Db, extractor: Arc<Extractor>, bus: Option<EventBus>) -> Self {
        Self {
            db,
            extractor,
            bus,
            running: Arc::default(),
            too_many: Arc::default(),
        }
    }

    fn folder(folder: &Path) -> Result<PathBuf> {
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(Error::invalid(format!(
                "not a folder: {}",
                folder.display()
            )));
        }
        Ok(std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf()))
    }

    /// The documents in a folder (hidden and generated folders left out).
    fn walk(folder: &Path) -> (Vec<Found>, Vec<NotRead>, bool) {
        let mut found = Vec::new();
        let mut not_read = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        let mut too_many = false;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                let rel = rel_of(folder, &dir);
                not_read.push(NotRead {
                    path: rel,
                    reason: "folder cannot be opened".into(),
                });
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(e.path());
                    }
                    continue;
                }
                if extract::kind_of(&name).is_none() {
                    continue;
                }
                let rel = rel_of(folder, &e.path());
                if not_here(&meta) {
                    not_read.push(NotRead {
                        path: rel,
                        reason: "not on this computer (only in iCloud) – download it first".into(),
                    });
                    continue;
                }
                if meta.len() > extract::MAX_BYTES {
                    not_read.push(NotRead {
                        path: rel,
                        reason: format!("larger than {} MB", extract::MAX_BYTES / 1024 / 1024),
                    });
                    continue;
                }
                if found.len() >= MAX_FILES {
                    too_many = true;
                    continue;
                }
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as i64);
                found.push(Found {
                    rel,
                    path: e.path(),
                    size: meta.len(),
                    mtime,
                });
            }
        }
        (found, not_read, too_many)
    }

    /// Reads what is new or changed in `folder` and forgets what is gone.
    /// One pass per folder at a time; a second call waits for nothing and
    /// returns the status.
    pub async fn refresh(&self, folder: &Path) -> Result<LibraryStatus> {
        let folder = Self::folder(folder)?;
        {
            let mut running = self.running.lock().unwrap();
            if running.contains_key(&folder) {
                drop(running);
                return self.status(&folder);
            }
            running.insert(folder.clone(), 0);
        }
        let result = self.pass(&folder).await;
        self.running.lock().unwrap().remove(&folder);
        self.emit(&folder);
        result?;
        self.status(&folder)
    }

    /// Starts a pass in the background (if none is running).
    pub fn refresh_soon(&self, folder: &Path) {
        let me = self.clone();
        let folder = folder.to_path_buf();
        tokio::spawn(async move {
            if let Err(e) = me.refresh(&folder).await {
                tracing::warn!(folder = %folder.display(), error = %e.message(), "reading the folder failed");
            }
        });
    }

    async fn pass(&self, folder: &Path) -> Result<()> {
        let f = folder.to_path_buf();
        let (found, not_read, too_many) = tokio::task::spawn_blocking(move || Self::walk(&f))
            .await
            .map_err(Error::internal)?;
        if too_many {
            self.too_many.lock().unwrap().insert(folder.to_path_buf());
        } else {
            self.too_many.lock().unwrap().remove(folder);
        }
        let key = folder.display().to_string();
        let known: HashMap<String, (i64, i64)> = self.db.with(|c| {
            let mut s = c.prepare("SELECT path, size, mtime FROM library WHERE folder = ?1")?;
            s.query_map(params![key], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
                .collect()
        })?;
        // Gone or no longer readable here: forgotten.
        let present: HashSet<&str> = found.iter().map(|f| f.rel.as_str()).collect();
        for rel in known.keys().filter(|k| !present.contains(k.as_str())) {
            self.db.with(|c| {
                c.execute(
                    "DELETE FROM library WHERE folder = ?1 AND path = ?2",
                    params![key, rel],
                )
                .map(|_| ())
            })?;
        }
        // What could not even be tried is kept with its reason.
        for n in &not_read {
            self.store(&key, &n.path, 0, 0, None, Some(&n.reason))?;
        }
        let todo: Vec<&Found> = found
            .iter()
            .filter(|f| known.get(&f.rel) != Some(&(f.size as i64, f.mtime)))
            .collect();
        let total = todo.len();
        for (i, f) in todo.into_iter().enumerate() {
            self.running
                .lock()
                .unwrap()
                .insert(folder.to_path_buf(), total - i);
            if i % 5 == 0 {
                self.emit(folder);
            }
            let dir = self.extractor.workdir()?;
            match self.extractor.read(&f.path, &dir).await {
                Ok(doc) => self.store(&key, &f.rel, f.size, f.mtime, Some(&doc.parts), None)?,
                Err(e) => self.store(&key, &f.rel, f.size, f.mtime, None, Some(&e.message()))?,
            }
        }
        Ok(())
    }

    fn store(
        &self,
        folder: &str,
        rel: &str,
        size: u64,
        mtime: i64,
        parts: Option<&[Part]>,
        error: Option<&str>,
    ) -> Result<()> {
        let parts = parts.map(serde_json::to_string).transpose()?;
        self.db.with(|c| {
            c.execute(
                "INSERT INTO library(folder, path, size, mtime, parts, error, read_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(folder, path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, parts = excluded.parts, error = excluded.error, read_at = excluded.read_at",
                params![folder, rel, size as i64, mtime, parts, error, Utc::now().to_rfc3339()],
            )
            .map(|_| ())
        })
    }

    fn emit(&self, folder: &Path) {
        if let (Some(bus), Ok(s)) = (&self.bus, self.status(folder)) {
            bus.emit(
                "library.progress",
                None,
                json!({"folder": folder, "read": s.read, "pending": s.pending, "reading": s.reading}),
            );
        }
    }

    pub fn status(&self, folder: &Path) -> Result<LibraryStatus> {
        let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let key = folder.display().to_string();
        type Row = (String, Option<i64>, Option<String>);
        let rows: Vec<Row> = self.db.with(|c| {
            let mut s = c.prepare(
                "SELECT path, CASE WHEN parts IS NULL THEN NULL ELSE length(parts) END, error FROM library WHERE folder = ?1 ORDER BY path",
            )?;
            s.query_map(params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect()
        })?;
        let mut st = LibraryStatus {
            folder: folder.clone(),
            ..Default::default()
        };
        for (path, len, error) in rows {
            match (len, error) {
                (Some(n), _) => {
                    st.read += 1;
                    st.chars += n.max(0) as usize;
                }
                (None, Some(reason)) => st.not_read.push(NotRead { path, reason }),
                (None, None) => {}
            }
        }
        if let Some(left) = self.running.lock().unwrap().get(&folder) {
            st.reading = true;
            st.pending = *left;
        }
        st.too_many = self.too_many.lock().unwrap().contains(&folder);
        Ok(st)
    }

    /// The text for a question: the passages of the folder's documents that
    /// fit it best (all of them when they are short), each with its source.
    pub fn passages(&self, folder: &Path, query: &str) -> Result<Vec<String>> {
        let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let key = folder.display().to_string();
        let rows: Vec<(String, String)> = self.db.with(|c| {
            let mut s = c.prepare(
                "SELECT path, parts FROM library WHERE folder = ?1 AND parts IS NOT NULL ORDER BY path",
            )?;
            s.query_map(params![key], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })?;
        let mut docs = Vec::new();
        for (path, parts) in rows {
            let parts: Vec<Part> = serde_json::from_str(&parts)?;
            docs.push((path, parts));
        }
        Ok(crate::choose(&docs, query, PASSAGE_BUDGET))
    }

    /// The project is gone from the list: its text goes too.
    pub fn forget(&self, folder: &Path) -> Result<()> {
        let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        let key = folder.display().to_string();
        self.db.with(|c| {
            c.execute("DELETE FROM library WHERE folder = ?1", params![key])
                .map(|_| ())
        })
    }
}

fn rel_of(folder: &Path, path: &Path) -> String {
    path.strip_prefix(folder)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// A file whose content is only in iCloud (a "dataless" placeholder):
/// reading it would download it – Ancilo does not do that by itself.
#[cfg(target_os = "macos")]
fn not_here(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn not_here(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::tests::{docx_of, pdf_of};

    fn lib(dir: &Path) -> Library {
        let ex = Extractor::new(None, dir.join("scratch"), Vec::new());
        Library::new(Db::in_memory().unwrap(), Arc::new(ex), None)
    }

    // covers: M10-AC-03
    #[tokio::test]
    async fn a_folder_is_read_once_kept_current_and_says_what_it_could_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("Steuer");
        std::fs::create_dir_all(folder.join("Belege/2025")).unwrap();
        std::fs::create_dir_all(folder.join(".hidden")).unwrap();
        std::fs::create_dir_all(folder.join("node_modules")).unwrap();
        std::fs::write(
            folder.join("Belege/2025/Rechnung.pdf"),
            pdf_of(&["Rechnung Nr. 7", "Betrag 120 Euro"]),
        )
        .unwrap();
        std::fs::write(
            folder.join("Notizen.docx"),
            docx_of(&["Steuerberater anrufen"]),
        )
        .unwrap();
        std::fs::write(folder.join("kaputt.pdf"), b"not a pdf").unwrap();
        std::fs::write(folder.join("foto.jpg"), b"\xff\xd8").unwrap();
        std::fs::write(folder.join(".hidden/geheim.txt"), "x").unwrap();
        std::fs::write(folder.join("node_modules/readme.md"), "x").unwrap();
        let l = lib(tmp.path());
        let st = l.refresh(&folder).await.unwrap();
        assert_eq!(st.read, 2, "{st:?}");
        assert!(!st.reading);
        assert_eq!(st.not_read.len(), 1);
        assert_eq!(st.not_read[0].path, "kaputt.pdf");
        assert!(
            st.not_read[0].reason.contains("cannot read the PDF"),
            "{st:?}"
        );
        // Sources: relative path and page.
        let p = l.passages(&folder, "Betrag").unwrap();
        assert!(
            p.iter()
                .any(|x| x.starts_with("[Belege/2025/Rechnung.pdf, page 2]")
                    && x.contains("120 Euro")),
            "{p:?}"
        );
        // Changed, new and gone files are noticed.
        std::fs::remove_file(folder.join("Notizen.docx")).unwrap();
        std::fs::write(folder.join("kaputt.pdf"), pdf_of(&["Jetzt lesbar"])).unwrap();
        let st = l.refresh(&folder).await.unwrap();
        assert_eq!((st.read, st.not_read.len()), (2, 0), "{st:?}");
        assert!(
            l.passages(&folder, "Steuerberater")
                .unwrap()
                .iter()
                .all(|x| !x.contains("Steuerberater"))
        );
        l.forget(&folder).unwrap();
        assert_eq!(l.status(&folder).unwrap().read, 0);
    }
}
