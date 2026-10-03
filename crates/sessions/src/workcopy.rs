//! A copy of a folder of documents to work in (decision
//! `2026-10-03-drei-bereiche`, Codex's review): the agent changes only the
//! copy; the user keeps the changes – or not.
//!
//! - **Copy**: every file (hidden ones, links and iCloud-only placeholders
//!   left out) is copied – on APFS as a clone that costs no space – after
//!   checking limits and free space. Its SHA-256 is the baseline.
//! - **Changes**: added, modified, deleted, renamed (same content, new
//!   place), compared by content, not by size or time.
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

/// Files a folder may hold to be copied.
pub const MAX_FILES: usize = 20_000;
/// Bytes a folder may hold to be copied.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024 * 1024;
/// Free space kept on the disk besides the copy.
const SPARE_BYTES: u64 = 1024 * 1024 * 1024;

/// One file of the baseline (the folder as it was copied, or last applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    size: u64,
    hash: String,
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

/// Free bytes on the disk that holds `path`, and whether it is APFS (where
/// a copy is a clone that costs no space). Asks only that disk – never all
/// of them (network disks could take seconds to answer).
fn disk_of(path: &Path) -> Option<(u64, bool)> {
    let st = rustix::fs::statfs(path).ok()?;
    let free = (st.f_bavail as u64).saturating_mul(st.f_bsize as u64);
    #[cfg(target_os = "macos")]
    let apfs = {
        let name: Vec<u8> = st
            .f_fstypename
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        name == b"apfs"
    };
    #[cfg(not(target_os = "macos"))]
    let apfs = false;
    Some((free, apfs))
}

/// Whether two paths are on the same disk (a clone is only possible there).
fn same_disk(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
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

    /// Copies `source` into `dir/work` after checking limits and space.
    pub fn create(source: &Path, dir: &Path) -> Result<Self> {
        let source = std::fs::canonicalize(source)
            .map_err(|e| Error::invalid(format!("cannot open {}: {e}", source.display())))?;
        if !source.is_dir() {
            return Err(Error::invalid(format!(
                "not a folder: {}",
                source.display()
            )));
        }
        let list = files(&source)?;
        let total: u64 = list.values().sum();
        if list.len() > MAX_FILES {
            return Err(Error::invalid(format!(
                "this folder holds {} files – Ancilo works on folders with up to {MAX_FILES}; choose a smaller one",
                list.len()
            )));
        }
        if total > MAX_BYTES {
            return Err(Error::invalid(format!(
                "this folder holds {:.1} GB – Ancilo works on folders up to {} GB; choose a smaller one",
                total as f64 / 1e9,
                MAX_BYTES / 1024 / 1024 / 1024
            )));
        }
        std::fs::create_dir_all(dir).map_err(Error::internal)?;
        let dir = std::fs::canonicalize(dir).map_err(Error::internal)?;
        // On the same APFS disk the copy is a clone; elsewhere it needs room.
        if let Some((free, apfs)) = disk_of(&dir) {
            let needs = if apfs && same_disk(&dir, &source) {
                0
            } else {
                total
            };
            if needs + SPARE_BYTES > free {
                return Err(Error::InsufficientResources(format!(
                    "the copy needs {:.1} GB, but only {:.1} GB are free",
                    (needs + SPARE_BYTES) as f64 / 1e9,
                    free as f64 / 1e9
                )));
            }
        }
        let wc = Self {
            source: source.clone(),
            dir: dir.clone(),
        };
        let work = wc.work();
        std::fs::create_dir_all(&work).map_err(Error::internal)?;
        let mut baseline = BTreeMap::new();
        for (rel, size) in &list {
            let from = source.join(rel);
            copy_file(&from, &work.join(rel))
                .map_err(|e| Error::internal(format!("copying {rel}: {e}")))?;
            // The copy's hash is the original's: the file was just copied.
            let hash = hash_file(&work.join(rel)).map_err(Error::internal)?;
            baseline.insert(rel.clone(), Entry { size: *size, hash });
        }
        wc.save_baseline(&baseline)?;
        Ok(wc)
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
            .filter(|(rel, _)| !now.contains_key(*rel))
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

    /// Moves the committed baseline and history into place.
    fn finish_commit(&self) -> Result<()> {
        for name in ["baseline.json", "applied.json"] {
            let next = self.dir.join(format!("{name}.next"));
            if next.exists() {
                std::fs::rename(&next, self.dir.join(name)).map_err(Error::internal)?;
            }
        }
        std::fs::remove_file(self.journal_file()).ok();
        Ok(())
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
        self.finish_commit()
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
            self.finish_commit()?;
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
        let applied = Applied {
            id: id.clone(),
            changes: changes.clone(),
            at: chrono::Utc::now(),
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
            let base_hash = |rel: &str| base.get(rel).map(|e| e.hash.clone()).unwrap_or_default();
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
        let mirror = ops.clone();
        self.transact(&id, ops, true, fault, || {
            let mut next = base.clone();
            for op in &mirror {
                match op {
                    Op::Put { rel, after, .. } => {
                        let size = std::fs::metadata(self.source.join(rel))
                            .map(|m| m.len())
                            .unwrap_or(0);
                        next.insert(
                            rel.clone(),
                            Entry {
                                size,
                                hash: after.clone(),
                            },
                        );
                    }
                    Op::Remove { rel, .. } => {
                        next.remove(rel);
                    }
                    Op::Move { from, to, .. } => {
                        if let Some(e) = next.remove(from) {
                            next.insert(to.clone(), e);
                        }
                    }
                }
            }
            Ok((next, history))
        })?;
        // The copy follows the folder (it had no open changes).
        let work = self.work();
        for op in &mirror {
            match op {
                Op::Put { rel, content, .. } => {
                    copy_file(content, &work.join(rel)).map_err(Error::internal)?
                }
                Op::Remove { rel, .. } => {
                    std::fs::remove_file(work.join(rel)).ok();
                }
                Op::Move { from, to, .. } => {
                    if let Some(parent) = work.join(to).parent() {
                        std::fs::create_dir_all(parent).map_err(Error::internal)?;
                    }
                    std::fs::rename(work.join(from), work.join(to)).map_err(Error::internal)?;
                }
            }
        }
        std::fs::remove_dir_all(backup).ok();
        Ok(last)
    }

    /// Makes the copy equal to the folder for `paths` (or everything):
    /// changes not applied are dropped. The folder is never touched.
    pub fn discard(&self, paths: Option<&[String]>) -> Result<Vec<Change>> {
        let all = self.changes()?;
        let plan: Vec<Change> = match paths {
            None => all,
            Some(ps) => all.into_iter().filter(|c| ps.contains(&c.path)).collect(),
        };
        let base = self.baseline()?;
        let work = self.work();
        for c in &plan {
            let restore = |rel: &str| -> Result<()> {
                if !base.contains_key(rel) {
                    return Ok(());
                }
                // Only the folder's own file – never through a link, never
                // a placeholder that is only in iCloud.
                let from = self
                    .target(rel, false)
                    .map_err(|e| Error::Conflict(format!("cannot restore {rel}: {e}")))?;
                let meta = std::fs::symlink_metadata(&from)
                    .map_err(|e| Error::Conflict(format!("cannot restore {rel}: {e}")))?;
                if !meta.is_file() || only_in_cloud(&meta) {
                    return Err(Error::Conflict(format!(
                        "cannot restore {rel}: it is not a plain file in the folder anymore"
                    )));
                }
                copy_file(&from, &work.join(rel))
                    .map_err(|e| Error::internal(format!("restoring {rel}: {e}")))
            };
            match c.kind {
                ChangeKind::Added => {
                    std::fs::remove_file(work.join(&c.path)).ok();
                }
                ChangeKind::Modified | ChangeKind::Deleted => restore(&c.path)?,
                ChangeKind::Renamed => {
                    std::fs::remove_file(work.join(&c.path)).ok();
                    if let Some(from) = &c.from {
                        restore(from)?;
                    }
                }
            }
        }
        Ok(plan)
    }

    /// The copy and its backups are removed (the folder stays as it is).
    pub fn remove(&self) {
        std::fs::remove_dir_all(&self.dir).ok();
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
        let w = wc.work();
        assert!(!w.join(".DS_Store").exists(), "hidden files stay out");
        std::fs::write(w.join("notiz.txt"), "neu").unwrap();
        std::fs::create_dir_all(w.join("Archiv")).unwrap();
        std::fs::rename(w.join("2025/rechnung-a.pdf"), w.join("Archiv/2025-a.pdf")).unwrap();
        std::fs::remove_file(w.join("2025/rechnung-b.pdf")).unwrap();
        std::fs::write(w.join("tabelle.csv"), "a;b").unwrap();
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
        let w = wc.work();
        std::fs::write(w.join("notiz.txt"), "neu").unwrap();
        std::fs::write(w.join("neu.txt"), "x").unwrap();
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
        std::fs::write(wc.work().join("notiz.txt"), "neu").unwrap();
        let shown = wc.version().unwrap();
        std::fs::write(wc.work().join("notiz.txt"), "anders").unwrap();
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
        let w = wc.work();
        std::fs::write(w.join("notiz.txt"), "neu").unwrap();
        std::fs::remove_file(w.join("2025/rechnung-b.pdf")).unwrap();
        std::fs::create_dir_all(w.join("neu/tief")).unwrap();
        std::fs::write(w.join("neu/tief/a.txt"), "x").unwrap();
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
        std::fs::write(wc.work().join("notiz.txt"), "neu").unwrap();
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
        let w = wc.work();
        std::fs::write(w.join("notiz.txt"), "neu").unwrap();
        std::fs::rename(w.join("2025/rechnung-a.pdf"), w.join("a.pdf")).unwrap();
        wc.apply(None, None).unwrap();
        // Open changes in the copy would be lost.
        std::fs::write(w.join("offen.txt"), "x").unwrap();
        let e = wc.undo().unwrap_err();
        assert!(
            e.message().contains("keep or drop the open changes first"),
            "{}",
            e.message()
        );
        std::fs::remove_file(w.join("offen.txt")).unwrap();
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
        std::fs::write(wc.work().join("2025/neu.txt"), "x").unwrap();
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
        std::fs::write(wc.work().join("notiz.txt"), "neu").unwrap();
        std::fs::write(t.path().join("geheim.txt"), "s3cret").unwrap();
        std::fs::remove_file(src.join("notiz.txt")).unwrap();
        std::os::unix::fs::symlink(t.path().join("geheim.txt"), src.join("notiz.txt")).unwrap();
        assert!(wc.discard(None).is_err());
        assert_eq!(read(&wc.work().join("notiz.txt")), "neu");
    }

    #[test]
    fn discarding_brings_the_copy_back_to_the_folder() {
        let (_t, _src, wc) = setup();
        let w = wc.work();
        std::fs::write(w.join("notiz.txt"), "neu").unwrap();
        std::fs::rename(w.join("2025/rechnung-a.pdf"), w.join("a.pdf")).unwrap();
        std::fs::write(w.join("x.txt"), "x").unwrap();
        wc.discard(None).unwrap();
        assert!(wc.changes().unwrap().is_empty());
        assert_eq!(read(&w.join("notiz.txt")), "alt");
    }

    #[test]
    fn too_large_folders_are_refused_before_anything_is_copied() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("viel");
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..=MAX_FILES {
            if i % 1000 == 0 {
                std::fs::create_dir_all(src.join(format!("d{}", i / 1000))).unwrap();
            }
            std::fs::write(src.join(format!("d{}/{i}.txt", i / 1000)), "").unwrap();
        }
        let e = WorkCopy::create(&src, &t.path().join("s")).unwrap_err();
        assert!(
            e.message().contains("choose a smaller one"),
            "{}",
            e.message()
        );
        assert!(!t.path().join("s/work").exists());
    }
}
