//! A copy of a folder of documents to work in (decision
//! `2026-10-03-drei-bereiche`, Codex's review): the agent changes only the
//! copy; the user keeps the changes – or not.
//!
//! - **Copy on change**: nothing is copied up front – a task starts at once,
//!   whatever the folder's size. The task sees the folder through the copy:
//!   a file it has not changed is read from the folder (never written
//!   there); before it changes a file, the folder's file is noted – its
//!   SHA-256 is the baseline – and, if the change needs its content, cloned
//!   into the copy (on APFS a clone that costs no space). A file is deleted
//!   in the copy only when its entry says so (`gone`) – a crash in the
//!   middle of a step never turns into a deletion. Every step notes first
//!   and touches files after, writes through a temporary file, and never
//!   replaces a file of the copy from the folder. Hidden files, links and
//!   iCloud-only placeholders are never part of it; each path has one
//!   spelling.
//! - **Changes**: added, modified, deleted, renamed (same content, new
//!   place), compared by content, not by size or time – only the files the
//!   task touched are looked at.
//! - **Apply** is planned from exactly the changes shown, checked against
//!   the folder as it is now (anything changed meanwhile is a conflict and
//!   nothing is written), then done step by step: originals backed up first,
//!   each step in a journal, files written next to their target and renamed
//!   into place. A failing step rolls back all before it; a crash is rolled
//!   back on the next start.
//! - **Undo** puts an applied batch back – if the folder still holds what was
//!   applied.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Files a folder (with its own folders) moved as one may hold.
pub const MAX_MOVE: usize = 2_000;

/// One file of the baseline (the folder's file as the task first touched
/// it, or as last applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    size: u64,
    hash: String,
    /// Deleted in the copy (removed or moved away). Only this marks a
    /// deletion: an entry without a file in the copy is otherwise untouched.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    gone: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// A change in the copy, compared with the folder as it was copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    pub kind: ChangeKind,
    /// Relative path (the new one for a rename).
    pub path: String,
    /// The old path of a renamed file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Size in bytes now (0 for a deleted file).
    pub size: u64,
}

/// What an apply did – kept for undo.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Applied {
    pub id: String,
    pub changes: Vec<Change>,
    pub at: chrono::DateTime<chrono::Utc>,
    /// What the folder holds after it (path → SHA-256): what an undo
    /// expects there – whatever the copy did since.
    #[serde(default)]
    pub after: BTreeMap<String, String>,
}

/// The folders being written now: one apply or undo per folder at a time
/// (within Ancilo; other programs are caught by the checks before each step).
static BUSY: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

struct Writing(PathBuf);

impl Writing {
    fn start(folder: &Path) -> Result<Self> {
        if !BUSY.lock().unwrap().insert(folder.to_path_buf()) {
            return Err(Error::Conflict(
                "another session is writing to this folder right now – try again in a moment"
                    .into(),
            ));
        }
        Ok(Self(folder.to_path_buf()))
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        BUSY.lock().unwrap().remove(&self.0);
    }
}

/// One operation on the user's folder, with what must be there before and
/// what is there after – so it can be checked right before it runs, and put
/// back only while the folder still holds what it wrote.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Writes `rel` with the content of `content` (inside the session's
    /// place). `before`: the content that must be there now (`None`: nothing
    /// may be there – a new file never replaces one that appeared).
    Put {
        rel: String,
        content: PathBuf,
        before: Option<String>,
        after: String,
    },
    /// Removes `rel`, which must hold `before`. `prune`: folders left empty
    /// go too (an undo removing what an apply made).
    Remove {
        rel: String,
        before: String,
        #[serde(default)]
        prune: bool,
    },
    /// Moves `from` (holding `hash`) to `to`, which must not exist.
    Move {
        from: String,
        to: String,
        hash: String,
        #[serde(default)]
        prune: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Running,
    /// All operations done; the new baseline and history are written as
    /// `*.next` and only need to be moved into place.
    Committed,
}

/// Write-ahead journal: written – and synced – before each operation runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    id: String,
    ops: Vec<Op>,
    /// Operations finished.
    done: usize,
    /// The operation that may be half done (crash in the middle).
    started: Option<usize>,
    phase: Phase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkCopy {
    /// The user's folder.
    pub source: PathBuf,
    /// The session's own place: `work/`, `backup/`, the baseline.
    pub dir: PathBuf,
}

/// SHA-256 of a file.
fn hash_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// A file whose content is only in iCloud: not copied, never touched.
#[cfg(target_os = "macos")]
fn only_in_cloud(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    meta.st_flags() & 0x4000_0000 != 0
}

#[cfg(not(target_os = "macos"))]
fn only_in_cloud(_meta: &std::fs::Metadata) -> bool {
    false
}

/// One entry of a folder as the task sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub dir: bool,
    /// Bytes (0 for a folder).
    pub size: u64,
}

/// `rel` in the one spelling a task uses: plain parts joined by `/` – no
/// empty, `.`, `..` or hidden part. `None` for anything else.
pub fn clean(rel: &str) -> Option<String> {
    let rel = rel.trim();
    let rel = rel.strip_prefix("./").unwrap_or(rel).trim_end_matches('/');
    let ok = !rel.is_empty() && rel.split('/').all(|p| !p.is_empty() && !p.starts_with('.'));
    ok.then(|| rel.to_string())
}

/// A path in its one spelling (see [`clean`]) – an alias like `a/./b` would
/// be a second key for the same file.
fn valid(rel: &str) -> std::io::Result<()> {
    if clean(rel).as_deref() == Some(rel) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{rel}: not a plain path inside the folder"),
        ))
    }
}

/// `rel` under `root` – `None` if a link is anywhere on the way (not even a
/// broken one is followed).
fn plain_under(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for c in Path::new(rel).components() {
        let std::path::Component::Normal(n) = c else {
            return None;
        };
        at.push(n);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => return None,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Some(root.join(rel))
}

/// Writes `to` through a temporary file next to it: never half written.
fn put_file(to: &Path, fill: impl FnOnce(&Path) -> std::io::Result<()>) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_next_to(to);
    fill(&tmp)
        .and_then(|_| std::fs::rename(&tmp, to))
        .inspect_err(|_| {
            std::fs::remove_file(&tmp).ok();
        })
}

fn io_err(e: Error) -> std::io::Error {
    std::io::Error::other(e.message())
}

/// The regular files under `root` (relative path → size), hidden files,
/// links and iCloud-only placeholders left out.
fn files(root: &Path) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| Error::invalid(format!("cannot open {}: {e}", dir.display())))?;
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() && !only_in_cloud(&meta) {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(&e.path())
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, meta.len());
            }
        }
    }
    Ok(out)
}

/// Copies `from` to `to` (a clone on APFS: `std::fs::copy` uses one there).
fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).map(|_| ())
}

fn tmp_next_to(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.ancilo-{}", uuid::Uuid::new_v4().simple()))
}

/// Makes a file's content and its directory entry durable.
fn sync(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Renames, but never over an existing file.
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

fn conflict(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::AlreadyExists, msg)
}

impl WorkCopy {
    pub fn work(&self) -> PathBuf {
        self.dir.join("work")
    }

    fn baseline_file(&self) -> PathBuf {
        self.dir.join("baseline.json")
    }

    fn journal_file(&self) -> PathBuf {
        self.dir.join("journal.json")
    }

    fn applied_file(&self) -> PathBuf {
        self.dir.join("applied.json")
    }

    fn backup(&self, id: &str) -> PathBuf {
        self.dir.join("backup").join(id)
    }

    /// A copy of `source` in `dir/work` – empty at first: files come into
    /// it only when the task changes them.
    pub fn create(source: &Path, dir: &Path) -> Result<Self> {
        let source = std::fs::canonicalize(source)
            .map_err(|e| Error::invalid(format!("cannot open {}: {e}", source.display())))?;
        if !source.is_dir() {
            return Err(Error::invalid(format!(
                "not a folder: {}",
                source.display()
            )));
        }
        std::fs::create_dir_all(dir).map_err(Error::internal)?;
        let dir = std::fs::canonicalize(dir).map_err(Error::internal)?;
        let wc = Self { source, dir };
        std::fs::create_dir_all(wc.work()).map_err(Error::internal)?;
        wc.save_baseline(&BTreeMap::new())?;
        Ok(wc)
    }

    /// `rel` as the copy or the folder spell it. A disk that ignores case or
    /// Unicode normalisation (APFS by default) finds `brief.txt` as
    /// `Brief.txt` – one file must have one key, so each part takes the name
    /// of the entry that is that file.
    fn spelled(&self, rel: &str) -> String {
        use std::os::unix::fs::MetadataExt;
        if valid(rel).is_err() {
            return rel.to_string();
        }
        let mut out = String::new();
        for part in rel.split('/') {
            let mut name = part.to_string();
            for (root, ours) in [(self.work(), true), (self.source.clone(), false)] {
                // Never a look through a link – not even at names.
                if !ours && !self.root_ok() {
                    continue;
                }
                let dir = if out.is_empty() {
                    Some(root)
                } else {
                    plain_under(&root, &out)
                };
                let Some(dir) = dir else {
                    continue;
                };
                // Nothing there by any spelling: a new name, kept as given.
                let Ok(meta) = std::fs::symlink_metadata(dir.join(part)) else {
                    continue;
                };
                let Ok(names) = std::fs::read_dir(&dir)
                    .map(|rd| rd.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
                else {
                    continue;
                };
                if names.iter().any(|n| n.to_string_lossy() == part) {
                    break;
                }
                if let Some(n) = names.iter().find(|n| {
                    std::fs::symlink_metadata(dir.join(n))
                        .is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino())
                }) {
                    name = n.to_string_lossy().into_owned();
                    break;
                }
            }
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&name);
        }
        out
    }

    /// The folder is still where it was – not moved, not replaced by a link
    /// (then nothing of it is read or written).
    fn root_ok(&self) -> bool {
        std::fs::canonicalize(&self.source).is_ok_and(|c| c == self.source)
    }

    /// The folder's own file at `rel`: a plain file, no link on the way, not
    /// only in iCloud.
    fn folder_file(&self, rel: &str) -> Option<(PathBuf, std::fs::Metadata)> {
        if !self.root_ok() {
            return None;
        }
        let p = plain_under(&self.source, rel)?;
        let m = std::fs::symlink_metadata(&p).ok()?;
        (m.is_file() && !only_in_cloud(&m)).then_some((p, m))
    }

    /// The copy's own file at `rel`.
    fn copy_file_at(&self, rel: &str) -> Option<PathBuf> {
        let p = plain_under(&self.work(), rel)?;
        std::fs::symlink_metadata(&p)
            .is_ok_and(|m| m.is_file())
            .then_some(p)
    }

    /// The file the task sees at `rel` – the copy's, or else the folder's
    /// (unless deleted in the copy). `None`: no such file.
    pub fn file(&self, rel: &str) -> Option<PathBuf> {
        valid(rel).ok()?;
        let rel = &self.spelled(rel);
        if let Some(p) = self.copy_file_at(rel) {
            return Some(p);
        }
        if self.baseline().ok()?.get(rel).is_some_and(|e| e.gone) {
            return None;
        }
        self.folder_file(rel).map(|(p, _)| p)
    }

    /// Whether the task sees a folder at `rel` (`""`: the folder itself).
    pub fn is_dir(&self, rel: &str) -> bool {
        if rel.is_empty() {
            return true;
        }
        let rel = &self.spelled(rel);
        let at = |root: &Path| {
            plain_under(root, rel)
                .and_then(|p| std::fs::symlink_metadata(p).ok())
                .is_some_and(|m| m.is_dir())
        };
        valid(rel).is_ok() && (at(&self.work()) || (self.root_ok() && at(&self.source)))
    }

    /// What the task sees in the folder `rel` (`""`: the top), by name.
    pub fn list(&self, rel: &str) -> Vec<Item> {
        if !rel.is_empty() && valid(rel).is_err() {
            return Vec::new();
        }
        let rel = &self.spelled(rel);
        let base = self.baseline().unwrap_or_default();
        let mut items: BTreeMap<String, Item> = BTreeMap::new();
        let mut roots = vec![(self.work(), true)];
        if self.root_ok() {
            // The folder first; the copy's own files then take their place.
            roots.insert(0, (self.source.clone(), false));
        }
        for (root, ours) in roots {
            let dir = if rel.is_empty() {
                Some(root.clone())
            } else {
                plain_under(&root, rel)
            };
            let Some(Ok(rd)) = dir.map(std::fs::read_dir) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                    continue;
                };
                let path = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{rel}/{name}")
                };
                if m.file_type().is_symlink() {
                    continue;
                } else if m.is_dir() {
                    items.entry(name.clone()).or_insert(Item {
                        name,
                        dir: true,
                        size: 0,
                    });
                } else if m.is_file() && !only_in_cloud(&m) {
                    if !ours && base.get(&path).is_some_and(|e| e.gone) {
                        continue;
                    }
                    items.insert(
                        name.clone(),
                        Item {
                            name,
                            dir: false,
                            size: m.len(),
                        },
                    );
                }
            }
        }
        items.into_values().collect()
    }

    /// Before the task changes `rel`: the folder's file is noted in the
    /// baseline first – with `clone`, its content then goes into the copy
    /// (and if the folder's file changed meanwhile, the clone is what the
    /// change starts from). A file the copy already has is never replaced
    /// from the folder.
    fn take_over(&self, rel: &str, clone: bool) -> std::io::Result<()> {
        if self.copy_file_at(rel).is_some() {
            return Ok(());
        }
        let mut base = self.baseline().map_err(io_err)?;
        if base.get(rel).is_some_and(|e| e.gone) {
            return Ok(());
        }
        // Not in the copy: whatever was noted is stale – the folder now.
        let Some((from, meta)) = self.folder_file(rel) else {
            if base.remove(rel).is_some() {
                self.save_baseline(&base).map_err(io_err)?;
            }
            return Ok(());
        };
        let hash = hash_file(&from)?;
        base.insert(
            rel.to_string(),
            Entry {
                size: meta.len(),
                hash: hash.clone(),
                gone: false,
            },
        );
        self.save_baseline(&base).map_err(io_err)?;
        if clone {
            let to = self.work_path(rel)?;
            put_file(&to, |tmp| copy_file(&from, tmp))?;
            let got = hash_file(&to)?;
            if got != hash {
                base.insert(
                    rel.to_string(),
                    Entry {
                        size: std::fs::metadata(&to)?.len(),
                        hash: got,
                        gone: false,
                    },
                );
                self.save_baseline(&base).map_err(io_err)?;
            }
        }
        Ok(())
    }

    /// Sets the `gone` mark of the noted entries.
    fn mark(&self, marks: &[(&str, bool)]) -> std::io::Result<()> {
        let mut base = self.baseline().map_err(io_err)?;
        let mut changed = false;
        for (rel, gone) in marks {
            if let Some(e) = base.get_mut(*rel)
                && e.gone != *gone
            {
                e.gone = *gone;
                changed = true;
            }
        }
        if changed {
            self.save_baseline(&base).map_err(io_err)?;
        }
        Ok(())
    }

    /// Where `rel` goes in the copy – no link on the way, in the copy nor in
    /// the folder (where it would go when kept).
    fn work_path(&self, rel: &str) -> std::io::Result<PathBuf> {
        if !self.root_ok() || plain_under(&self.source, rel).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{rel}: a link is in the way"),
            ));
        }
        self.own_path(rel)
    }

    /// `rel` in the copy (no link on the way there).
    fn own_path(&self, rel: &str) -> std::io::Result<PathBuf> {
        valid(rel)?;
        plain_under(&self.work(), rel).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{rel}: a link is in the way"),
            )
        })
    }

    /// Writes `rel` in the copy (new, or replacing the file the task sees).
    pub fn write(&self, rel: &str, bytes: &[u8]) -> std::io::Result<()> {
        let rel = &self.spelled(rel);
        let to = self.work_path(rel)?;
        if self.is_dir(rel) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::IsADirectory,
                format!("{rel} is a folder"),
            ));
        }
        // What it replaces is noted – its content is not needed.
        self.take_over(rel, false)?;
        put_file(&to, |tmp| std::fs::write(tmp, bytes))?;
        // Written where one was deleted: it is there again.
        self.mark(&[(rel, false)])
    }

    /// Deletes the file `rel` in the copy.
    pub fn delete(&self, rel: &str) -> std::io::Result<()> {
        let rel = &self.spelled(rel);
        if self.file(rel).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{rel} is not a file"),
            ));
        }
        self.take_over(rel, false)?;
        match std::fs::remove_file(self.own_path(rel)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        // Only a file of the folder is deleted in it; the copy's own new
        // file is simply gone.
        self.mark(&[(rel, true)])
    }

    /// Makes the folder `rel` in the copy.
    pub fn make_dir(&self, rel: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.work_path(&self.spelled(rel))?)
    }

    /// Moves a file – or a folder with everything in it (up to
    /// [`MAX_MOVE`] files) – to `to`, which must not exist.
    pub fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        valid(from)?;
        valid(to)?;
        let (from, to) = (&self.spelled(from), &self.spelled(to));
        if self.file(to).is_some() || self.is_dir(to) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{to} exists already"),
            ));
        }
        if self.file(from).is_some() {
            return self.move_one(from, to);
        }
        if !self.is_dir(from) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{from} does not exist"),
            ));
        }
        if Path::new(to).starts_with(from) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{from} cannot go into itself"),
            ));
        }
        // A folder: every file in it, each moved like one file.
        let mut files = Vec::new();
        let mut stack = vec![from.to_string()];
        while let Some(dir) = stack.pop() {
            for item in self.list(&dir) {
                let rel = format!("{dir}/{}", item.name);
                if item.dir {
                    stack.push(rel);
                } else {
                    files.push(rel);
                }
            }
            if files.len() > MAX_MOVE {
                return Err(std::io::Error::other(format!(
                    "{from} holds more than {MAX_MOVE} files – move its folders one by one"
                )));
            }
        }
        for rel in &files {
            let rest = &rel[from.len()..];
            self.move_one(rel, &format!("{to}{rest}"))?;
        }
        std::fs::create_dir_all(self.work_path(to)?)
    }

    fn move_one(&self, from: &str, to: &str) -> std::io::Result<()> {
        let dst = self.work_path(to)?;
        self.take_over(from, true)?;
        let src = self.own_path(from)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        rename_new(&src, &dst)?;
        self.mark(&[(from, true), (to, false)])
    }

    fn baseline(&self) -> Result<BTreeMap<String, Entry>> {
        let text = std::fs::read_to_string(self.baseline_file()).map_err(Error::internal)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn save_baseline(&self, b: &BTreeMap<String, Entry>) -> Result<()> {
        let tmp = self.dir.join("baseline.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(b)?).map_err(Error::internal)?;
        std::fs::rename(&tmp, self.baseline_file()).map_err(Error::internal)
    }

    /// The changes in the copy, compared by content, each with the copy's
    /// content hash (for the plan's version).
    fn changes_hashed(&self) -> Result<Vec<(Change, Option<String>)>> {
        let base = self.baseline()?;
        let now = files(&self.work())?;
        let mut added: Vec<(String, u64, String)> = Vec::new();
        let mut out = Vec::new();
        for (rel, size) in &now {
            let hash = hash_file(&self.work().join(rel)).map_err(Error::internal)?;
            match base.get(rel) {
                Some(e) if e.hash == hash => {}
                Some(_) => out.push((
                    Change {
                        kind: ChangeKind::Modified,
                        path: rel.clone(),
                        from: None,
                        size: *size,
                    },
                    Some(hash),
                )),
                None => added.push((rel.clone(), *size, hash)),
            }
        }
        let mut deleted: Vec<(&String, &Entry)> = base
            .iter()
            .filter(|(rel, e)| e.gone && !now.contains_key(*rel))
            .collect();
        // A deleted file whose content reappears elsewhere was moved.
        for (rel, size, hash) in added {
            let kind_from = deleted
                .iter()
                .position(|(_, e)| e.hash == hash)
                .map(|i| deleted.remove(i).0.clone());
            out.push((
                Change {
                    kind: if kind_from.is_some() {
                        ChangeKind::Renamed
                    } else {
                        ChangeKind::Added
                    },
                    path: rel,
                    from: kind_from,
                    size,
                },
                Some(hash),
            ));
        }
        for (rel, _) in deleted {
            out.push((
                Change {
                    kind: ChangeKind::Deleted,
                    path: rel.clone(),
                    from: None,
                    size: 0,
                },
                None,
            ));
        }
        out.sort_by(|a, b| a.0.path.cmp(&b.0.path));
        Ok(out)
    }

    /// The changes in the copy, compared by content.
    pub fn changes(&self) -> Result<Vec<Change>> {
        Ok(self.changes_hashed()?.into_iter().map(|(c, _)| c).collect())
    }

    /// A short id of exactly these changes (paths, kinds and contents): what
    /// the user saw is what gets applied.
    pub fn version(&self) -> Result<String> {
        Ok(version_of(&self.changes_hashed()?))
    }

    /// The folder for `rel`, checked part by part: no link anywhere on the
    /// way, the folder itself still where it was. Creates missing folders.
    fn target(&self, rel: &str, create_dirs: bool) -> std::io::Result<PathBuf> {
        if std::fs::canonicalize(&self.source)? != self.source {
            return Err(conflict("the folder moved or became a link".into()));
        }
        let rel_path = Path::new(rel);
        let mut at = self.source.clone();
        let parts: Vec<_> = rel_path.components().collect();
        for (i, c) in parts.iter().enumerate() {
            let std::path::Component::Normal(name) = c else {
                return Err(conflict(format!("{rel}: not a plain path")));
            };
            at.push(name);
            let last = i + 1 == parts.len();
            match std::fs::symlink_metadata(&at) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(conflict(format!("{rel}: a link is in the way")));
                }
                Ok(m) if !last && !m.is_dir() => {
                    return Err(conflict(format!(
                        "{rel}: a file is where a folder should be"
                    )));
                }
                Ok(m) if last && m.is_dir() => {
                    return Err(conflict(format!(
                        "{rel}: a folder is where the file should be"
                    )));
                }
                Ok(_) => {}
                Err(_) if !last && create_dirs => std::fs::create_dir(&at)?,
                Err(_) => {}
            }
        }
        Ok(at)
    }

    /// The hash of what is at `rel` now (`None`: nothing there).
    fn current(&self, rel: &str) -> std::io::Result<Option<String>> {
        let p = self.target(rel, false)?;
        match std::fs::symlink_metadata(&p) {
            Err(_) => Ok(None),
            Ok(m) if only_in_cloud(&m) => Err(conflict(format!("{rel} is only in iCloud now"))),
            Ok(_) => hash_file(&p).map(Some),
        }
    }

    /// Checks that `rel` holds `want` (what the operation expects there).
    fn expect(&self, rel: &str, want: Option<&str>) -> std::io::Result<()> {
        let now = self.current(rel)?;
        if now.as_deref() == want {
            return Ok(());
        }
        Err(conflict(match (want, now) {
            (None, Some(_)) => {
                format!("{rel}: a file with this name appeared in the folder meanwhile")
            }
            (Some(_), None) => format!("{rel}: it was deleted in the folder meanwhile"),
            _ => format!("{rel}: it was changed in the folder meanwhile"),
        }))
    }

    /// Runs one operation – after checking, right before, that the folder
    /// still holds what it expects.
    fn run_op(&self, op: &Op) -> std::io::Result<()> {
        match op {
            Op::Put {
                rel,
                content,
                before,
                after,
            } => {
                self.expect(rel, before.as_deref())?;
                let target = self.target(rel, true)?;
                let tmp = tmp_next_to(&target);
                std::fs::copy(content, &tmp)?;
                if hash_file(&tmp)? != *after {
                    std::fs::remove_file(&tmp).ok();
                    return Err(std::io::Error::other(format!(
                        "{rel}: the new content changed while applying"
                    )));
                }
                std::fs::File::open(&tmp)?.sync_all()?;
                // Once more right before the swap: copying a large file takes
                // time, and the folder may have changed meanwhile.
                if let Err(e) = self.expect(rel, before.as_deref()) {
                    std::fs::remove_file(&tmp).ok();
                    return Err(e);
                }
                let r = match before {
                    // A new file never replaces one that appeared meanwhile.
                    None => rename_new(&tmp, &target),
                    Some(_) => std::fs::rename(&tmp, &target),
                };
                r.inspect_err(|_| {
                    std::fs::remove_file(&tmp).ok();
                })?;
                sync(&target)
            }
            Op::Remove { rel, before, prune } => {
                self.expect(rel, Some(before))?;
                let target = self.target(rel, false)?;
                std::fs::remove_file(&target)?;
                if let Some(parent) = target.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
                if *prune {
                    self.prune_empty(rel);
                }
                Ok(())
            }
            Op::Move {
                from,
                to,
                hash,
                prune,
            } => {
                self.expect(from, Some(hash))?;
                self.expect(to, None)?;
                let src = self.target(from, false)?;
                let dst = self.target(to, true)?;
                rename_new(&src, &dst)?;
                sync(&dst)?;
                if *prune {
                    self.prune_empty(from);
                }
                Ok(())
            }
        }
    }

    /// Takes one operation back – only while the folder holds what it wrote.
    /// `Ok(false)`: it had not happened (nothing to do).
    fn undo_op(&self, op: &Op, backup: &Path) -> std::io::Result<bool> {
        match op {
            Op::Put {
                rel, before, after, ..
            } => {
                let now = self.current(rel)?;
                if now.as_deref() == before.as_deref() {
                    return Ok(false);
                }
                if now.as_deref() != Some(after.as_str()) {
                    return Err(conflict(format!("{rel} was changed after it was written")));
                }
                let target = self.target(rel, false)?;
                match before {
                    None => {
                        std::fs::remove_file(&target)?;
                        self.prune_empty(rel);
                    }
                    Some(_) => {
                        let tmp = tmp_next_to(&target);
                        std::fs::copy(backup.join(rel), &tmp)?;
                        std::fs::File::open(&tmp)?.sync_all()?;
                        std::fs::rename(&tmp, &target)?;
                        sync(&target)?;
                    }
                }
                Ok(true)
            }
            Op::Remove { rel, before, .. } => {
                match self.current(rel)? {
                    Some(h) if h == *before => return Ok(false),
                    Some(_) => {
                        return Err(conflict(format!("{rel} appeared again with other content")));
                    }
                    None => {}
                }
                let target = self.target(rel, true)?;
                let tmp = tmp_next_to(&target);
                std::fs::copy(backup.join(rel), &tmp)?;
                std::fs::File::open(&tmp)?.sync_all()?;
                rename_new(&tmp, &target).inspect_err(|_| {
                    std::fs::remove_file(&tmp).ok();
                })?;
                sync(&target)?;
                Ok(true)
            }
            Op::Move { from, to, hash, .. } => {
                let (at_from, at_to) = (self.current(from)?, self.current(to)?);
                if at_from.as_deref() == Some(hash.as_str()) && at_to.is_none() {
                    return Ok(false);
                }
                if at_to.as_deref() != Some(hash.as_str()) || at_from.is_some() {
                    return Err(conflict(format!("{to} or {from} changed after the move")));
                }
                let src = self.target(to, false)?;
                let dst = self.target(from, true)?;
                rename_new(&src, &dst)?;
                sync(&dst)?;
                self.prune_empty(to);
                Ok(true)
            }
        }
    }

    /// Removes the folders above `rel` that are empty now (made for a file
    /// that is gone again) – never the folder itself, never one with files.
    fn prune_empty(&self, rel: &str) {
        let mut dir = self.source.join(rel);
        while dir.pop() && dir.starts_with(&self.source) && dir != self.source {
            if std::fs::remove_dir(&dir).is_err() {
                break;
            }
        }
    }

    fn write_journal(&self, j: &Journal) -> Result<()> {
        let tmp = self.dir.join("journal.json.tmp");
        let f = std::fs::File::create(&tmp).map_err(Error::internal)?;
        serde_json::to_writer(&f, j)?;
        f.sync_all().map_err(Error::internal)?;
        std::fs::rename(&tmp, self.journal_file()).map_err(Error::internal)?;
        std::fs::File::open(&self.dir)
            .and_then(|d| d.sync_all())
            .map_err(Error::internal)
    }

    fn write_next(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let p = self.dir.join(format!("{name}.next"));
        std::fs::write(&p, bytes).map_err(Error::internal)?;
        std::fs::File::open(&p)
            .and_then(|f| f.sync_all())
            .map_err(Error::internal)
    }

    /// Moves the committed baseline and history into place; after an undo,
    /// the copy lets go of the paths it took back (the baseline no longer
    /// notes them).
    fn finish_commit(&self, j: &Journal) -> Result<()> {
        for name in ["baseline.json", "applied.json"] {
            let next = self.dir.join(format!("{name}.next"));
            if next.exists() {
                std::fs::rename(&next, self.dir.join(name)).map_err(Error::internal)?;
            }
        }
        // Durable before the journal goes: baseline and history in place.
        let sync_dir = || {
            std::fs::File::open(&self.dir)
                .and_then(|d| d.sync_all())
                .map_err(Error::internal)
        };
        sync_dir()?;
        if j.id.starts_with("un-") {
            let base = self.baseline()?;
            for rel in j.ops.iter().flat_map(op_paths) {
                if !base.contains_key(rel) {
                    self.let_go(rel);
                }
            }
        }
        std::fs::remove_file(self.journal_file()).ok();
        sync_dir()
    }

    /// Runs `ops` as one transaction: the originals they need backed up and
    /// synced first; each operation journaled before it runs and checked
    /// right before; on failure everything done is put back. `commit` gives
    /// the new baseline and history once all operations ran.
    fn transact(
        &self,
        id: &str,
        ops: Vec<Op>,
        backup_from_folder: bool,
        fault: &dyn Fn(usize) -> std::io::Result<()>,
        commit: impl FnOnce() -> Result<(BTreeMap<String, Entry>, Vec<Applied>)>,
    ) -> Result<()> {
        let backup = self.backup(id);
        // 1. Originals that will be replaced or removed – checked and synced.
        if backup_from_folder {
            for op in &ops {
                let (rel, want) = match op {
                    Op::Put {
                        rel,
                        before: Some(b),
                        ..
                    } => (rel, b),
                    Op::Remove { rel, before, .. } => (rel, before),
                    _ => continue,
                };
                let to = backup.join(rel);
                let from = self
                    .target(rel, false)
                    .map_err(|e| Error::Conflict(format!("nothing was changed – {e}")))?;
                copy_file(&from, &to).map_err(Error::internal)?;
                // The backup is durable – the file and every folder entry on its way.
                let mut synced = Some(to.clone());
                while let Some(p) = synced {
                    std::fs::File::open(&p)
                        .and_then(|f| f.sync_all())
                        .map_err(Error::internal)?;
                    synced = p
                        .parent()
                        .filter(|d| d.starts_with(&self.dir))
                        .map(Path::to_path_buf);
                }
                if hash_file(&to).map_err(Error::internal)? != *want {
                    return Err(Error::Conflict(format!(
                        "nothing was changed – {rel} was changed in the folder meanwhile"
                    )));
                }
            }
        }
        let mut journal = Journal {
            id: id.to_string(),
            ops,
            done: 0,
            started: None,
            phase: Phase::Running,
        };
        self.write_journal(&journal)?;
        // 2. Each operation: journaled, checked, done.
        let mut failure = None;
        for i in 0..journal.ops.len() {
            journal.started = Some(i);
            self.write_journal(&journal)?;
            if let Err(e) = fault(i).and_then(|_| self.run_op(&journal.ops[i])) {
                failure = Some(e);
                break;
            }
            journal.done = i + 1;
            journal.started = None;
            self.write_journal(&journal)?;
        }
        if let Some(e) = failure {
            let rolled = self.roll_back(&journal);
            if rolled.is_ok() {
                std::fs::remove_file(self.journal_file()).ok();
            }
            let what = if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("nothing was changed – {e}")
            } else {
                format!("applying failed ({e})")
            };
            return Err(match rolled {
                Ok(()) if e.kind() == std::io::ErrorKind::AlreadyExists => Error::Conflict(what),
                Ok(()) => Error::internal(format!("{what} – the folder is as it was")),
                Err(why) => Error::internal(format!(
                    "{what}; putting things back stopped: {why} – nothing else will be written to this folder until it is resolved"
                )),
            });
        }
        // 3. Commit: baseline and history as `.next`, then the phase, then
        //    into place – a crash at any point is finished or undone.
        let (baseline, applied) = commit()?;
        self.write_next("baseline.json", &serde_json::to_vec(&baseline)?)?;
        self.write_next("applied.json", &serde_json::to_vec(&applied)?)?;
        journal.phase = Phase::Committed;
        self.write_journal(&journal)?;
        self.finish_commit(&journal)
    }

    /// Puts back what the journal says was done (and the one that may have
    /// started), newest first – each only if the folder holds what it wrote.
    fn roll_back(&self, j: &Journal) -> std::result::Result<(), String> {
        let backup = self.backup(&j.id);
        let last = j.started.map_or(j.done, |s| s + 1).min(j.ops.len());
        let mut problems = Vec::new();
        for op in j.ops[..last].iter().rev() {
            if let Err(e) = self.undo_op(op, &backup) {
                tracing::error!(error = %e, ?op, "putting an operation back failed");
                problems.push(e.to_string());
            }
        }
        for name in ["baseline.json.next", "applied.json.next"] {
            std::fs::remove_file(self.dir.join(name)).ok();
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    /// After a crash: an apply or undo in the middle is put back, one that
    /// was committed is finished. Returns whether there was anything to do.
    /// While something cannot be put back, the journal stays (and blocks
    /// further writes to the folder).
    pub fn recover(&self) -> Result<bool> {
        let Ok(text) = std::fs::read_to_string(self.journal_file()) else {
            return Ok(false);
        };
        let j: Journal = serde_json::from_str(&text)?;
        if j.phase == Phase::Committed {
            self.finish_commit(&j)?;
            return Ok(true);
        }
        self.roll_back(&j).map_err(|why| {
            Error::internal(format!(
                "an interrupted apply could not be put back completely: {why} – check these files in the folder"
            ))
        })?;
        std::fs::remove_file(self.journal_file()).ok();
        Ok(true)
    }

    pub fn applied(&self) -> Result<Vec<Applied>> {
        match std::fs::read_to_string(self.applied_file()) {
            Ok(t) => Ok(serde_json::from_str(&t)?),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Applies the changes – all, or those at `paths` (a rename counts by
    /// its new path). `version`: the changes as the user saw them; if they
    /// differ now, nothing is applied.
    pub fn apply(&self, paths: Option<&[String]>, version: Option<&str>) -> Result<Applied> {
        self.apply_with(paths, version, &|_| Ok(()))
    }

    fn apply_with(
        &self,
        paths: Option<&[String]>,
        version: Option<&str>,
        fault: &dyn Fn(usize) -> std::io::Result<()>,
    ) -> Result<Applied> {
        let _writing = Writing::start(&self.source)?;
        self.recover()?;
        let all = self.changes_hashed()?;
        if let Some(v) = version
            && v != version_of(&all)
        {
            return Err(Error::Conflict(
                "the changes are not the ones you saw anymore – look at them again".into(),
            ));
        }
        let plan: Vec<(Change, Option<String>)> = match paths {
            None => all,
            Some(ps) => all
                .into_iter()
                .filter(|(c, _)| ps.contains(&c.path))
                .collect(),
        };
        if plan.is_empty() {
            return Err(Error::invalid("there are no changes to apply"));
        }
        let base = self.baseline()?;
        let work = self.work();
        let id = format!("ap-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let mut ops = Vec::new();
        for (c, hash) in &plan {
            let base_hash = |rel: &str| base.get(rel).map(|e| e.hash.clone());
            ops.push(match c.kind {
                ChangeKind::Added | ChangeKind::Modified => Op::Put {
                    rel: c.path.clone(),
                    content: work.join(&c.path),
                    before: base_hash(&c.path),
                    after: hash.clone().unwrap_or_default(),
                },
                ChangeKind::Deleted => Op::Remove {
                    rel: c.path.clone(),
                    before: base_hash(&c.path).unwrap_or_default(),
                    prune: false,
                },
                ChangeKind::Renamed => {
                    let from = c.from.clone().unwrap_or_default();
                    Op::Move {
                        hash: base_hash(&from).unwrap_or_default(),
                        from,
                        to: c.path.clone(),
                        prune: false,
                    }
                }
            });
        }
        let changes: Vec<Change> = plan.iter().map(|(c, _)| c.clone()).collect();
        // What the folder holds after it: a renamed file the content it had.
        let after = plan
            .iter()
            .filter_map(|(c, hash)| match c.kind {
                ChangeKind::Added | ChangeKind::Modified => {
                    hash.clone().map(|h| (c.path.clone(), h))
                }
                ChangeKind::Renamed => c
                    .from
                    .as_deref()
                    .and_then(|f| base.get(f))
                    .map(|e| (c.path.clone(), e.hash.clone())),
                ChangeKind::Deleted => None,
            })
            .collect();
        let applied = Applied {
            id: id.clone(),
            changes: changes.clone(),
            at: chrono::Utc::now(),
            after,
        };
        let done = applied.clone();
        self.transact(&id, ops, true, fault, || {
            let mut next = base.clone();
            for (c, hash) in &plan {
                if let Some(from) = &c.from {
                    next.remove(from);
                }
                match (c.kind, hash) {
                    (ChangeKind::Deleted, _) => {
                        next.remove(&c.path);
                    }
                    (_, Some(h)) => {
                        next.insert(
                            c.path.clone(),
                            Entry {
                                size: c.size,
                                hash: h.clone(),
                                gone: false,
                            },
                        );
                    }
                    (_, None) => {}
                }
            }
            let mut history = self.applied()?;
            history.push(done);
            Ok((next, history))
        })?;
        Ok(applied)
    }

    /// Takes back the last apply – if the folder still holds what was
    /// applied, and the copy has no open changes (they would be lost).
    pub fn undo(&self) -> Result<Applied> {
        self.undo_with(&|_| Ok(()))
    }

    fn undo_with(&self, fault: &dyn Fn(usize) -> std::io::Result<()>) -> Result<Applied> {
        let _writing = Writing::start(&self.source)?;
        self.recover()?;
        if !self.changes()?.is_empty() {
            return Err(Error::Conflict(
                "keep or drop the open changes first – undoing would lose them".into(),
            ));
        }
        let mut history = self.applied()?;
        let last = history
            .pop()
            .ok_or_else(|| Error::invalid("nothing was applied that could be undone"))?;
        let base = self.baseline()?;
        let backup = self.backup(&last.id);
        let mut ops = Vec::new();
        for c in last.changes.iter().rev() {
            // As applied – older records without it: as noted since.
            let base_hash = |rel: &str| {
                last.after
                    .get(rel)
                    .cloned()
                    .or_else(|| base.get(rel).map(|e| e.hash.clone()))
                    .unwrap_or_default()
            };
            ops.push(match c.kind {
                ChangeKind::Added => Op::Remove {
                    rel: c.path.clone(),
                    before: base_hash(&c.path),
                    prune: true,
                },
                ChangeKind::Modified => Op::Put {
                    rel: c.path.clone(),
                    content: backup.join(&c.path),
                    before: Some(base_hash(&c.path)),
                    after: hash_file(&backup.join(&c.path)).map_err(Error::internal)?,
                },
                ChangeKind::Deleted => Op::Put {
                    rel: c.path.clone(),
                    content: backup.join(&c.path),
                    before: None,
                    after: hash_file(&backup.join(&c.path)).map_err(Error::internal)?,
                },
                ChangeKind::Renamed => Op::Move {
                    from: c.path.clone(),
                    to: c.from.clone().unwrap_or_default(),
                    hash: base_hash(&c.path),
                    prune: true,
                },
            });
        }
        let id = format!("un-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        // The copy lets go of every path the undo touched: the task sees the
        // folder's files again (taken out of the copy as the commit
        // finishes – after a crash too).
        let mut next = base.clone();
        for op in &ops {
            for rel in op_paths(op) {
                next.remove(rel);
            }
        }
        self.transact(&id, ops, true, fault, || Ok((next, history)))?;
        std::fs::remove_dir_all(backup).ok();
        Ok(last)
    }

    /// Makes the copy equal to the folder for `paths` (or everything):
    /// changes not applied are dropped – the task sees the folder's files
    /// again. The folder is never touched. Noted first, files after: a crash
    /// in between leaves files the folder has as "new" (never written over
    /// a file there), never a deletion.
    pub fn discard(&self, paths: Option<&[String]>) -> Result<Vec<Change>> {
        let all = self.changes()?;
        let plan: Vec<Change> = match paths {
            None => all,
            Some(ps) => all.into_iter().filter(|c| ps.contains(&c.path)).collect(),
        };
        let mut base = self.baseline()?;
        let rels: Vec<&String> = plan
            .iter()
            .flat_map(|c| std::iter::once(&c.path).chain(c.from.as_ref()))
            .collect();
        for rel in &rels {
            base.remove(*rel);
        }
        self.save_baseline(&base)?;
        for rel in rels {
            self.let_go(rel);
        }
        Ok(plan)
    }

    /// Takes the copy's file at `rel` out (and folders left empty by it).
    fn let_go(&self, rel: &str) {
        let Ok(p) = self.own_path(rel) else {
            return;
        };
        std::fs::remove_file(&p).ok();
        let work = self.work();
        let mut dir = p;
        while dir.pop() && dir.starts_with(&work) && dir != work {
            if std::fs::remove_dir(&dir).is_err() {
                break;
            }
        }
    }

    /// Material for a free task (a file the user gave it): into the task's
    /// own folder – which the task sees – not a change, not a result.
    /// Returns the name it got.
    pub fn add_input(&self, name: &str, bytes: &[u8]) -> Result<String> {
        let name = unique_in(&[&self.source, &self.work()], name);
        valid(&name).map_err(|e| Error::invalid(e.to_string()))?;
        let to = match plain_under(&self.source, &name) {
            Some(p) if self.root_ok() => p,
            _ => {
                return Err(Error::Conflict(
                    "the task's folder moved or became a link".into(),
                ));
            }
        };
        put_file(&to, |tmp| std::fs::write(tmp, bytes)).map_err(Error::internal)?;
        Ok(name)
    }

    /// A file of the folder with this name and exactly this content, if
    /// there is one (looked for among at most `limit` entries): what the user
    /// gave is already there.
    pub fn find_same(&self, name: &str, bytes: &[u8], limit: usize) -> Option<String> {
        let want = hex::encode(Sha256::digest(bytes));
        let mut seen = 0;
        let mut stack = vec![String::new()];
        while let Some(dir) = stack.pop() {
            for item in self.list(&dir) {
                seen += 1;
                if seen > limit {
                    return None;
                }
                let rel = if dir.is_empty() {
                    item.name.clone()
                } else {
                    format!("{dir}/{}", item.name)
                };
                if item.dir {
                    stack.push(rel);
                } else if item.name == name
                    && item.size == bytes.len() as u64
                    && self
                        .file(&rel)
                        .and_then(|p| hash_file(&p).ok())
                        .is_some_and(|h| h == want)
                {
                    return Some(rel);
                }
            }
        }
        None
    }

    /// The copy and its backups are removed (the folder stays as it is).
    pub fn remove(&self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// `name`, or `name 2`, `name 3` … – the first that is free in all `dirs`.
pub fn unique_in(dirs: &[&Path], name: &str) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut candidate = name.to_string();
    let mut n = 2;
    while dirs
        .iter()
        .any(|d| std::fs::symlink_metadata(d.join(&candidate)).is_ok())
    {
        candidate = format!("{stem} {n}{ext}");
        n += 1;
    }
    candidate
}

/// Copies a result to `dir` under its name (or `name 2` …) – never over an
/// existing file. Returns where it went.
pub fn save_copy(from: &Path, dir: &Path) -> Result<PathBuf> {
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::invalid("a result without a name"))?;
    loop {
        let target = dir.join(unique_in(&[dir], &name));
        let tmp = tmp_next_to(&target);
        std::fs::copy(from, &tmp).map_err(Error::internal)?;
        std::fs::File::open(&tmp)
            .and_then(|f| f.sync_all())
            .map_err(Error::internal)?;
        match rename_new(&tmp, &target) {
            Ok(()) => return Ok(target),
            // Taken in the meantime: the next free name.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&tmp).ok();
            }
            Err(e) => {
                std::fs::remove_file(&tmp).ok();
                return Err(Error::internal(e));
            }
        }
    }
}

/// The paths of the folder an operation touches.
fn op_paths(op: &Op) -> Vec<&String> {
    match op {
        Op::Put { rel, .. } | Op::Remove { rel, .. } => vec![rel],
        Op::Move { from, to, .. } => vec![from, to],
    }
}

fn version_of(changes: &[(Change, Option<String>)]) -> String {
    let mut h = Sha256::new();
    for (c, hash) in changes {
        h.update(format!(
            "{:?}\0{}\0{:?}\0{:?}\n",
            c.kind, c.path, c.from, hash
        ));
    }
    hex::encode(h.finalize())[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf, WorkCopy) {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("Belege");
        std::fs::create_dir_all(src.join("2025")).unwrap();
        std::fs::write(src.join("2025/rechnung-a.pdf"), "A").unwrap();
        std::fs::write(src.join("2025/rechnung-b.pdf"), "B").unwrap();
        std::fs::write(src.join("notiz.txt"), "alt").unwrap();
        std::fs::write(src.join(".DS_Store"), "x").unwrap();
        let wc = WorkCopy::create(&src, &t.path().join("session")).unwrap();
        let src = std::fs::canonicalize(&src).unwrap();
        (t, src, wc)
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    // covers: M10-AC-04
    #[test]
    fn changes_in_the_copy_are_found_by_content_and_applied_with_a_way_back() {
        let (_t, src, wc) = setup();
        assert!(wc.file(".DS_Store").is_none(), "hidden files stay out");
        wc.write("notiz.txt", b"neu").unwrap();
        wc.make_dir("Archiv").unwrap();
        wc.rename("2025/rechnung-a.pdf", "Archiv/2025-a.pdf")
            .unwrap();
        wc.delete("2025/rechnung-b.pdf").unwrap();
        wc.write("tabelle.csv", b"a;b").unwrap();
        let ch = wc.changes().unwrap();
        let kinds: Vec<(ChangeKind, &str, Option<&str>)> = ch
            .iter()
            .map(|c| (c.kind, c.path.as_str(), c.from.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            [
                (ChangeKind::Deleted, "2025/rechnung-b.pdf", None),
                (
                    ChangeKind::Renamed,
                    "Archiv/2025-a.pdf",
                    Some("2025/rechnung-a.pdf")
                ),
                (ChangeKind::Modified, "notiz.txt", None),
                (ChangeKind::Added, "tabelle.csv", None),
            ]
        );
        assert_eq!(read(&src.join("notiz.txt")), "alt");
        let v = wc.version().unwrap();
        let applied = wc.apply(None, Some(&v)).unwrap();
        assert_eq!(read(&src.join("notiz.txt")), "neu");
        assert_eq!(read(&src.join("Archiv/2025-a.pdf")), "A");
        assert!(!src.join("2025/rechnung-a.pdf").exists());
        assert!(!src.join("2025/rechnung-b.pdf").exists());
        assert_eq!(read(&src.join("tabelle.csv")), "a;b");
        assert!(
            wc.changes().unwrap().is_empty(),
            "the copy equals the folder now"
        );
        assert!(!wc.journal_file().exists());
        // Undo puts everything back – in the folder and in the copy.
        assert_eq!(wc.undo().unwrap().id, applied.id);
        assert_eq!(read(&src.join("notiz.txt")), "alt");
        assert_eq!(read(&src.join("2025/rechnung-a.pdf")), "A");
        assert_eq!(read(&src.join("2025/rechnung-b.pdf")), "B");
        assert!(!src.join("tabelle.csv").exists() && !src.join("Archiv").exists());
        assert!(wc.changes().unwrap().is_empty());
        assert!(wc.undo().is_err(), "nothing left to undo");
    }

    #[test]
    fn a_folder_changed_meanwhile_is_a_conflict_and_nothing_is_written() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        wc.write("neu.txt", b"x").unwrap();
        std::fs::write(src.join("notiz.txt"), "vom Nutzer").unwrap();
        let e = wc.apply(None, None).unwrap_err();
        assert!(matches!(e, Error::Conflict(_)), "{e:?}");
        assert!(
            e.message()
                .contains("notiz.txt was changed in the folder meanwhile"),
            "{}",
            e.message()
        );
        assert_eq!(read(&src.join("notiz.txt")), "vom Nutzer");
        assert!(!src.join("neu.txt").exists());
        // A new file never replaces one that appeared meanwhile.
        std::fs::write(src.join("neu.txt"), "auch vom Nutzer").unwrap();
        let e = wc.apply(Some(&["neu.txt".to_string()]), None).unwrap_err();
        assert!(
            e.message().contains("appeared in the folder meanwhile"),
            "{}",
            e.message()
        );
        assert_eq!(read(&src.join("neu.txt")), "auch vom Nutzer");
        std::fs::remove_file(src.join("neu.txt")).unwrap();
        wc.apply(Some(&["neu.txt".to_string()]), None).unwrap();
        assert_eq!(read(&src.join("neu.txt")), "x");
    }

    #[test]
    fn what_is_applied_is_exactly_what_was_shown() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        let shown = wc.version().unwrap();
        wc.write("notiz.txt", b"anders").unwrap();
        let e = wc.apply(None, Some(&shown)).unwrap_err();
        assert!(
            e.message().contains("not the ones you saw"),
            "{}",
            e.message()
        );
        assert_eq!(read(&src.join("notiz.txt")), "alt");
    }

    #[test]
    fn a_failing_step_rolls_back_every_step_before_it() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        wc.delete("2025/rechnung-b.pdf").unwrap();
        wc.write("neu/tief/a.txt", b"x").unwrap();
        for at in 0..3 {
            let e = wc
                .apply_with(None, None, &|i| {
                    if i == at {
                        Err(std::io::Error::other("disk full"))
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert!(
                e.message().contains("the folder is as it was"),
                "{}",
                e.message()
            );
            assert_eq!(read(&src.join("notiz.txt")), "alt", "step {at}");
            assert_eq!(read(&src.join("2025/rechnung-b.pdf")), "B", "step {at}");
            assert!(!src.join("neu").exists(), "step {at}");
            assert_eq!(wc.changes().unwrap().len(), 3, "the copy keeps its changes");
            assert!(!wc.journal_file().exists());
        }
    }

    #[test]
    fn a_crash_in_the_middle_is_put_back_and_a_committed_one_finished() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        let after = hash_file(&wc.work().join("notiz.txt")).unwrap();
        let before = hash_file(&src.join("notiz.txt")).unwrap();
        // As if Ancilo stopped right after writing the note.
        let id = "ap-crash";
        copy_file(&src.join("notiz.txt"), &wc.backup(id).join("notiz.txt")).unwrap();
        std::fs::write(src.join("notiz.txt"), "neu").unwrap();
        let op = Op::Put {
            rel: "notiz.txt".into(),
            content: wc.work().join("notiz.txt"),
            before: Some(before),
            after,
        };
        wc.write_journal(&Journal {
            id: id.into(),
            ops: vec![op.clone()],
            done: 0,
            started: Some(0),
            phase: Phase::Running,
        })
        .unwrap();
        assert!(wc.recover().unwrap());
        assert_eq!(read(&src.join("notiz.txt")), "alt");
        assert!(!wc.recover().unwrap(), "nothing more to do");
        // Changed by the user after the crash: not overwritten, the journal stays.
        std::fs::write(src.join("notiz.txt"), "neu").unwrap();
        wc.write_journal(&Journal {
            id: id.into(),
            ops: vec![op.clone()],
            done: 1,
            started: None,
            phase: Phase::Running,
        })
        .unwrap();
        std::fs::write(src.join("notiz.txt"), "vom Nutzer danach").unwrap();
        assert!(wc.recover().is_err());
        assert_eq!(read(&src.join("notiz.txt")), "vom Nutzer danach");
        assert!(
            wc.apply(None, None).is_err(),
            "no more writes until resolved"
        );
        std::fs::remove_file(wc.journal_file()).unwrap();
        // Committed: the new baseline goes into place.
        std::fs::write(src.join("notiz.txt"), "neu").unwrap();
        wc.write_next("baseline.json", br#"{"notiz.txt":{"size":3,"hash":"x"}}"#)
            .unwrap();
        wc.write_journal(&Journal {
            id: id.into(),
            ops: vec![op],
            done: 1,
            started: None,
            phase: Phase::Committed,
        })
        .unwrap();
        assert!(wc.recover().unwrap());
        assert_eq!(
            read(&src.join("notiz.txt")),
            "neu",
            "a committed apply stays"
        );
        assert!(read(&wc.baseline_file()).contains("\"x\""));
    }

    #[test]
    fn undo_refuses_when_the_folder_changed_since_or_changes_are_open() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        wc.rename("2025/rechnung-a.pdf", "a.pdf").unwrap();
        wc.apply(None, None).unwrap();
        // Open changes in the copy would be lost.
        wc.write("offen.txt", b"x").unwrap();
        let e = wc.undo().unwrap_err();
        assert!(
            e.message().contains("keep or drop the open changes first"),
            "{}",
            e.message()
        );
        wc.delete("offen.txt").unwrap();
        // A new file where a moved one was: never overwritten.
        std::fs::write(src.join("2025/rechnung-a.pdf"), "neu vom Nutzer").unwrap();
        let e = wc.undo().unwrap_err();
        assert!(
            e.message().contains("nothing was changed"),
            "{}",
            e.message()
        );
        assert_eq!(read(&src.join("2025/rechnung-a.pdf")), "neu vom Nutzer");
        assert_eq!(
            read(&src.join("notiz.txt")),
            "neu",
            "and nothing else was undone"
        );
        std::fs::remove_file(src.join("2025/rechnung-a.pdf")).unwrap();
        std::fs::write(src.join("notiz.txt"), "später geändert").unwrap();
        let e = wc.undo().unwrap_err();
        assert!(e.message().contains("notiz.txt"), "{}", e.message());
        assert_eq!(read(&src.join("notiz.txt")), "später geändert");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_folder_never_leads_writing_outside() {
        let (t, src, wc) = setup();
        wc.write("2025/neu.txt", b"x").unwrap();
        // Meanwhile the subfolder became a link to somewhere else.
        let outside = t.path().join("woanders");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::rename(src.join("2025"), t.path().join("2025-weg")).unwrap();
        std::os::unix::fs::symlink(&outside, src.join("2025")).unwrap();
        let e = wc.apply(None, None).unwrap_err();
        assert!(e.message().contains("link"), "{}", e.message());
        assert!(!outside.join("neu.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn discarding_never_takes_a_link_into_the_copy() {
        let (t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        std::fs::write(t.path().join("geheim.txt"), "s3cret").unwrap();
        std::fs::remove_file(src.join("notiz.txt")).unwrap();
        std::os::unix::fs::symlink(t.path().join("geheim.txt"), src.join("notiz.txt")).unwrap();
        wc.discard(None).unwrap();
        // The task sees the folder again – but never through the link.
        assert!(wc.file("notiz.txt").is_none());
        assert!(!wc.list("").iter().any(|i| i.name == "notiz.txt"));
        assert!(!wc.work().join("notiz.txt").exists());
    }

    #[test]
    fn discarding_brings_the_copy_back_to_the_folder() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        wc.rename("2025/rechnung-a.pdf", "a.pdf").unwrap();
        wc.write("x.txt", b"x").unwrap();
        wc.discard(None).unwrap();
        assert!(wc.changes().unwrap().is_empty());
        assert_eq!(wc.file("notiz.txt"), Some(src.join("notiz.txt")));
        assert_eq!(
            wc.file("2025/rechnung-a.pdf"),
            Some(src.join("2025/rechnung-a.pdf"))
        );
        assert!(wc.file("x.txt").is_none() && wc.file("a.pdf").is_none());
        assert!(
            files(&wc.work()).unwrap().is_empty(),
            "the copy is empty again"
        );
    }

    // covers: M10-AC-04
    #[test]
    fn a_large_folder_starts_at_once_and_the_task_sees_it_through_the_copy() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("viel");
        for i in 0..=20_000 {
            if i % 1000 == 0 {
                std::fs::create_dir_all(src.join(format!("d{}", i / 1000))).unwrap();
            }
            std::fs::write(src.join(format!("d{}/{i}.txt", i / 1000)), "").unwrap();
        }
        std::fs::write(src.join("brief.txt"), "Hallo").unwrap();
        let wc = WorkCopy::create(&src, &t.path().join("s")).unwrap();
        // Nothing was copied or hashed up front.
        assert!(files(&wc.work()).unwrap().is_empty());
        assert!(wc.baseline().unwrap().is_empty());
        let src = std::fs::canonicalize(&src).unwrap();
        // Untouched files are read from the folder …
        assert_eq!(wc.file("brief.txt"), Some(src.join("brief.txt")));
        assert_eq!(wc.list("d3").len(), 1000);
        // … a changed one from the copy; the folder stays as it is.
        wc.write("brief.txt", b"Hallo Welt").unwrap();
        assert_eq!(wc.file("brief.txt"), Some(wc.work().join("brief.txt")));
        assert_eq!(read(&src.join("brief.txt")), "Hallo");
        let top: Vec<(String, bool, u64)> = wc
            .list("")
            .into_iter()
            .filter(|i| !i.dir)
            .map(|i| (i.name, i.dir, i.size))
            .collect();
        assert_eq!(top, [("brief.txt".to_string(), false, 10)]);
        // A file deleted in the copy is gone for the task – not in the folder.
        wc.delete("d0/0.txt").unwrap();
        assert!(wc.file("d0/0.txt").is_none());
        assert_eq!(wc.list("d0").len(), 999);
        assert!(src.join("d0/0.txt").exists());
        let kinds: Vec<ChangeKind> = wc.changes().unwrap().iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ChangeKind::Modified, ChangeKind::Deleted]);
    }

    #[test]
    fn a_folder_moves_with_everything_in_it() {
        let (_t, src, wc) = setup();
        wc.rename("2025", "Archiv/2025").unwrap();
        assert!(wc.file("2025/rechnung-a.pdf").is_none());
        assert!(wc.file("Archiv/2025/rechnung-b.pdf").is_some());
        assert!(
            wc.rename("Archiv", "Archiv/innen").is_err(),
            "never into itself"
        );
        assert!(
            wc.rename("notiz.txt", "Archiv/2025/rechnung-a.pdf")
                .is_err(),
            "never over a file"
        );
        wc.apply(None, None).unwrap();
        assert_eq!(read(&src.join("Archiv/2025/rechnung-a.pdf")), "A");
        assert!(!src.join("2025/rechnung-a.pdf").exists());
    }

    #[test]
    fn the_same_file_is_found_in_the_folder() {
        let (_t, _src, wc) = setup();
        assert_eq!(
            wc.find_same("rechnung-b.pdf", b"B", 1000).as_deref(),
            Some("2025/rechnung-b.pdf")
        );
        assert_eq!(wc.find_same("rechnung-b.pdf", b"anders", 1000), None);
        assert_eq!(wc.find_same("rechnung-b.pdf", b"B", 1), None, "only so far");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_replaced_by_a_link_is_neither_read_nor_written() {
        let (t, src, wc) = setup();
        let outside = t.path().join("draussen");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("notiz.txt"), "s3cret").unwrap();
        std::fs::rename(&src, t.path().join("Belege-weg")).unwrap();
        std::os::unix::fs::symlink(&outside, &src).unwrap();
        assert!(wc.file("notiz.txt").is_none());
        assert!(wc.list("").is_empty());
        assert!(!wc.is_dir("2025"));
        assert!(wc.write("neu.txt", b"x").is_err());
        assert!(!outside.join("neu.txt").exists());
    }

    // covers: M10-AC-04
    #[test]
    fn a_crash_in_the_middle_of_a_step_never_becomes_a_deletion() {
        let (_t, src, wc) = setup();
        // Noted, but the file never reached the copy (crash): untouched.
        wc.take_over("notiz.txt", false).unwrap();
        assert!(wc.changes().unwrap().is_empty());
        assert_eq!(wc.file("notiz.txt"), Some(src.join("notiz.txt")));
        // Moved in the copy, but not yet marked (crash): the folder's file
        // stays, the moved one is new – nothing is lost when kept.
        wc.take_over("2025/rechnung-a.pdf", true).unwrap();
        std::fs::rename(
            wc.work().join("2025/rechnung-a.pdf"),
            wc.work().join("a.pdf"),
        )
        .unwrap();
        let kinds: Vec<(ChangeKind, String)> = wc
            .changes()
            .unwrap()
            .into_iter()
            .map(|c| (c.kind, c.path))
            .collect();
        assert_eq!(kinds, [(ChangeKind::Added, "a.pdf".to_string())]);
        assert!(wc.file("2025/rechnung-a.pdf").is_some());
        // Dropped, but the copy's file not yet taken out (crash): it shows as
        // new where the folder has one – keeping it never writes over that.
        wc.discard(None).unwrap();
        wc.write("notiz.txt", b"neu").unwrap();
        let mut base = wc.baseline().unwrap();
        base.remove("notiz.txt");
        wc.save_baseline(&base).unwrap();
        let e = wc.apply(None, None).unwrap_err();
        assert!(
            e.message().contains("appeared in the folder meanwhile"),
            "{}",
            e.message()
        );
        assert_eq!(read(&src.join("notiz.txt")), "alt");
    }

    #[test]
    fn the_copys_own_file_is_never_taken_from_the_folder() {
        let (_t, src, wc) = setup();
        wc.write("neu.txt", b"vom Agenten").unwrap();
        // The user makes a file of the same name meanwhile.
        std::fs::write(src.join("neu.txt"), "vom Nutzer").unwrap();
        wc.rename("neu.txt", "b.txt").unwrap();
        assert_eq!(read(&wc.work().join("b.txt")), "vom Agenten");
        let kinds: Vec<(ChangeKind, String)> = wc
            .changes()
            .unwrap()
            .into_iter()
            .map(|c| (c.kind, c.path))
            .collect();
        assert_eq!(kinds, [(ChangeKind::Added, "b.txt".to_string())]);
        // Deleting the copy's own file never deletes the user's.
        wc.write("c.txt", b"vom Agenten").unwrap();
        std::fs::write(src.join("c.txt"), "auch vom Nutzer").unwrap();
        wc.delete("c.txt").unwrap();
        assert!(
            !wc.changes().unwrap().iter().any(|c| c.path == "c.txt"),
            "no deletion of the user's file"
        );
        wc.apply(None, None).unwrap();
        assert_eq!(read(&src.join("neu.txt")), "vom Nutzer");
        assert_eq!(read(&src.join("c.txt")), "auch vom Nutzer");
    }

    #[test]
    fn each_path_has_one_spelling() {
        let (_t, _src, wc) = setup();
        for alias in [
            "2025/./neu.txt",
            "2025//neu.txt",
            "./neu.txt",
            "2025/../neu.txt",
            "neu.txt/",
        ] {
            assert!(wc.write(alias, b"x").is_err(), "{alias}");
            assert!(wc.file(alias).is_none(), "{alias}");
        }
        assert_eq!(clean("./2025/neu.txt/").as_deref(), Some("2025/neu.txt"));
        assert_eq!(clean("2025/./neu.txt"), None);
        assert!(wc.changes().unwrap().is_empty());
    }

    #[test]
    fn an_undo_lets_go_of_the_copy_even_after_a_crash() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"neu").unwrap();
        wc.delete("2025/rechnung-b.pdf").unwrap();
        wc.apply(None, None).unwrap();
        wc.undo().unwrap();
        assert!(wc.changes().unwrap().is_empty());
        assert_eq!(wc.file("notiz.txt"), Some(src.join("notiz.txt")));
        assert_eq!(
            wc.file("2025/rechnung-b.pdf"),
            Some(src.join("2025/rechnung-b.pdf"))
        );
        assert!(files(&wc.work()).unwrap().is_empty());
        // Committed, then a crash before the copy let go: finished at the next start.
        wc.write("notiz.txt", b"neu").unwrap();
        wc.apply(None, None).unwrap();
        let mut base = wc.baseline().unwrap();
        base.remove("notiz.txt");
        wc.write_next("baseline.json", &serde_json::to_vec(&base).unwrap())
            .unwrap();
        wc.write_journal(&Journal {
            id: "un-crash".into(),
            ops: vec![Op::Remove {
                rel: "notiz.txt".into(),
                before: "x".into(),
                prune: false,
            }],
            done: 1,
            started: None,
            phase: Phase::Committed,
        })
        .unwrap();
        assert!(wc.recover().unwrap());
        assert!(!wc.work().join("notiz.txt").exists());
        assert!(wc.changes().unwrap().is_empty());
    }

    #[test]
    fn undo_follows_what_was_applied_not_what_the_copy_did_since() {
        let (_t, src, wc) = setup();
        wc.write("notiz.txt", b"eins").unwrap();
        wc.apply(None, None).unwrap();
        wc.write("notiz.txt", b"zwei").unwrap();
        wc.apply(None, None).unwrap();
        // Changed again and dropped: the copy forgets the file.
        wc.write("notiz.txt", b"drei").unwrap();
        wc.discard(None).unwrap();
        wc.undo().unwrap();
        assert_eq!(read(&src.join("notiz.txt")), "eins");
        wc.undo().unwrap();
        assert_eq!(read(&src.join("notiz.txt")), "alt");
    }

    #[test]
    fn a_name_spelled_otherwise_is_the_same_file() {
        let (_t, src, wc) = setup();
        // Only where the disk ignores case (APFS by default).
        if !src.join("NOTIZ.TXT").exists() {
            return;
        }
        wc.write("Neu.txt", b"x").unwrap();
        wc.delete("neu.txt").unwrap();
        assert!(wc.changes().unwrap().is_empty());
        wc.delete("NOTIZ.txt").unwrap();
        let ch = wc.changes().unwrap();
        assert_eq!(
            (ch[0].kind, ch[0].path.as_str()),
            (ChangeKind::Deleted, "notiz.txt")
        );
        assert!(wc.file("Notiz.TXT").is_none(), "deleted by any spelling");
    }

    #[cfg(unix)]
    #[test]
    fn material_never_goes_through_a_replaced_folder() {
        let t = tempfile::tempdir().unwrap();
        let own = t.path().join("eigen");
        std::fs::create_dir_all(&own).unwrap();
        let wc = WorkCopy::create(&own, &t.path().join("s")).unwrap();
        let outside = t.path().join("draussen");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir(&own).unwrap();
        std::os::unix::fs::symlink(&outside, &own).unwrap();
        assert!(wc.add_input("a.txt", b"x").is_err());
        assert!(!outside.join("a.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_spelling_is_never_looked_up_through_a_link() {
        let (t, src, wc) = setup();
        let outside = t.path().join("draussen");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("Datei.txt"), "s3cret").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("link")).unwrap();
        assert_eq!(wc.spelled("link/datei.txt"), "link/datei.txt");
        assert!(wc.file("link/datei.txt").is_none());
    }
}
