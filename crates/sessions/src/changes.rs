//! Where a session's agent works, and how its changes reach the project.
//!
//! Every session works in a **work area of its own**; the project changes
//! only when changes are applied – checked first, then written, all or
//! nothing.
//!
//! - **Git project:** a git worktree based on a snapshot of the project –
//!   committed, changed and new (not ignored) files; nothing in the project
//!   changes to take it.
//! - **No git:** a shadow repository in Ancilo's data directory holds the
//!   snapshot; the work area is a checkout of it (large generated folders
//!   such as `node_modules` are linked, not copied).
//!
//! Reading changes (diffs) never touches an index anyone else uses, paths are
//! taken literally (no git wildcards), renames are shown as delete + add.
//!
//! See decision `2026-09-30-m8-umsetzung`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileDiff {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    /// Unified diff (possibly cut).
    pub patch: String,
}

const MAX_PATCH: usize = 200_000;

/// Large, generated folders a shadow snapshot leaves out; the work area links
/// them to the project's so builds and tests still find them.
const HEAVY: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "dist",
    "build",
    "__pycache__",
    ".DS_Store",
];

/// A git repository as seen from one directory.
#[derive(Debug, Clone, Copy)]
struct Repo<'a> {
    cwd: &'a Path,
    /// Shadow repositories: the git directory and the work tree.
    shadow: Option<(&'a Path, &'a Path)>,
    /// A separate index (shadow work areas, private copies).
    index: Option<&'a Path>,
}

impl<'a> Repo<'a> {
    fn at(cwd: &'a Path) -> Self {
        Self {
            cwd,
            shadow: None,
            index: None,
        }
    }

    fn with_index(self, index: &'a Path) -> Self {
        Self {
            index: Some(index),
            ..self
        }
    }

    fn git(&self, args: &[&str], input: Option<&str>) -> Result<String> {
        let mut cmd = Command::new("git");
        if let Some(ix) = self.index {
            cmd.env("GIT_INDEX_FILE", ix);
        }
        if let Some((g, w)) = self.shadow {
            cmd.env("GIT_DIR", g).env("GIT_WORK_TREE", w);
        }
        cmd.current_dir(self.cwd)
            .args(["-c", "core.quotepath=off", "-c", "core.autocrlf=false"])
            .args(args)
            // Paths are file names, never patterns.
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_AUTHOR_NAME", "Ancilo")
            .env("GIT_AUTHOR_EMAIL", "ancilo@localhost")
            .env("GIT_COMMITTER_NAME", "Ancilo")
            .env("GIT_COMMITTER_EMAIL", "ancilo@localhost")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::unavailable(format!("git is not available: {e}")))?;
        if let Some(text) = input
            && let Some(mut stdin) = child.stdin.take()
        {
            stdin.write_all(text.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(Error::Conflict(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// The index this repository uses by default.
    fn index_path(&self) -> Result<PathBuf> {
        if let Some(ix) = self.index {
            return Ok(ix.to_path_buf());
        }
        let p = self.git(
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            None,
        )?;
        Ok(PathBuf::from(p.trim()))
    }

    /// Runs `f` against a private copy of the index with the whole work tree
    /// staged – nobody else's index is touched, so views, diffs and a running
    /// agent may look at the same work area at the same time.
    fn staged<T>(&self, f: impl FnOnce(Repo<'_>) -> Result<T>) -> Result<T> {
        let real = self.index_path()?;
        let tmp = tempfile::tempdir()?;
        let ix = tmp.path().join("index");
        if real.is_file() {
            // A copy keeps git's stat cache (only changed files are hashed
            // again) – and must keep the original's modification time: git
            // trusts cached stat data of entries older than the index file
            // ("racy git"), so a fresh time would hide a same-size edit.
            std::fs::copy(&real, &ix)?;
            let modified = std::fs::metadata(&real)?.modified()?;
            std::fs::File::options()
                .write(true)
                .open(&ix)?
                .set_modified(modified)?;
        }
        let repo = self.with_index(&ix);
        repo.git(&["add", "-A"], None)?;
        f(repo)
    }

    /// The work tree as a commit on top of `parent` (nothing changes).
    fn snapshot(&self, parent: Option<&str>, message: &str) -> Result<String> {
        self.staged(|r| {
            let tree = r.git(&["write-tree"], None)?;
            let mut args = vec!["commit-tree", tree.trim(), "-m", message];
            if let Some(p) = parent {
                args.extend(["-p", p]);
            }
            Ok(r.git(&args, None)?.trim().to_string())
        })
    }
}

/// `git diff --numstat -z` → files.
fn numstat(text: &str) -> Vec<FileDiff> {
    text.split('\0')
        .filter_map(|rec| {
            let mut parts = rec.splitn(3, '\t');
            let added = parts.next()?.trim().parse().unwrap_or(0);
            let removed = parts.next()?.parse().unwrap_or(0);
            let path = parts.next()?.to_string();
            (!path.is_empty()).then_some(FileDiff {
                path,
                added,
                removed,
            })
        })
        .collect()
}

fn cut(mut patch: String) -> String {
    if patch.len() > MAX_PATCH {
        let mut end = MAX_PATCH;
        while !patch.is_char_boundary(end) {
            end -= 1;
        }
        patch.truncate(end);
        patch.push_str("\n… (diff shortened)\n");
    }
    patch
}

/// Whether `dir` is inside a git work tree; returns its top.
pub fn git_top(dir: &Path) -> Option<PathBuf> {
    Repo::at(dir)
        .git(&["rev-parse", "--show-toplevel"], None)
        .ok()
        .map(|s| PathBuf::from(s.trim()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Changes {
    /// A git worktree of the project.
    Worktree {
        /// The project (top of its repository).
        project: PathBuf,
        dir: PathBuf,
        /// Project snapshot the changes count from.
        base: String,
    },
    /// A project without git: shadow repository + work area.
    Shadow {
        project: PathBuf,
        git_dir: PathBuf,
        dir: PathBuf,
        base: String,
    },
}

impl Changes {
    /// A work area for `project` in `dir`, at the project's current state.
    /// `shadow_git`: where a project without git keeps its snapshots.
    pub fn create(project: &Path, dir: &Path, shadow_git: &Path) -> Result<Self> {
        match git_top(project) {
            Some(top) => {
                let base = Self::project_snapshot(&top)?;
                Self::git_worktree(&top, dir, &base, &base)
            }
            None => {
                std::fs::create_dir_all(shadow_git)?;
                let init = Repo {
                    cwd: project,
                    shadow: Some((shadow_git, project)),
                    index: None,
                };
                init.git(&["init", "-q"], None)?;
                std::fs::create_dir_all(shadow_git.join("info"))?;
                std::fs::write(shadow_git.join("info/exclude"), HEAVY.join("\n") + "\n")?;
                let changes = Self::Shadow {
                    project: project.to_path_buf(),
                    git_dir: shadow_git.to_path_buf(),
                    dir: dir.to_path_buf(),
                    base: String::new(),
                };
                let base = changes.project_repo().snapshot(None, "project")?;
                changes.at(dir, &base, &base)
            }
        }
    }

    /// The project's current state (committed, changed and new files) as a
    /// commit of a git project.
    fn project_snapshot(project: &Path) -> Result<String> {
        let repo = Repo::at(project);
        let head = repo
            .git(&["rev-parse", "--verify", "-q", "HEAD"], None)
            .ok();
        repo.snapshot(head.as_deref().map(str::trim), "ancilo: project state")
    }

    fn git_worktree(project: &Path, dir: &Path, base: &str, start: &str) -> Result<Self> {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Repo::at(project).git(
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &dir.display().to_string(),
                start,
            ],
            None,
        )?;
        Ok(Self::Worktree {
            project: project.to_path_buf(),
            dir: dir.to_path_buf(),
            base: base.to_string(),
        })
    }

    /// Another work area of the same kind in `dir`, at `start`, whose
    /// changes count from `base` ("retry with another model").
    fn at(&self, dir: &Path, base: &str, start: &str) -> Result<Self> {
        match self {
            Self::Worktree { project, .. } => Self::git_worktree(project, dir, base, start),
            Self::Shadow {
                project, git_dir, ..
            } => {
                std::fs::create_dir_all(dir)?;
                let c = Self::Shadow {
                    project: project.clone(),
                    git_dir: git_dir.clone(),
                    dir: dir.to_path_buf(),
                    base: base.to_string(),
                };
                c.reset_to(start)?;
                // Generated folders stay in the project; link them.
                for name in HEAVY {
                    let (from, to) = (project.join(name), dir.join(name));
                    if from.is_dir() && !to.exists() {
                        #[cfg(unix)]
                        std::os::unix::fs::symlink(&from, &to).ok();
                    }
                }
                Ok(c)
            }
        }
    }

    /// A work area for a variant: the same base, starting at `start`.
    pub fn variant(&self, dir: &Path, start: &str) -> Result<Self> {
        self.at(dir, self.base(), start)
    }

    /// Where the agent works.
    pub fn work_dir(&self) -> &Path {
        match self {
            Self::Worktree { dir, .. } | Self::Shadow { dir, .. } => dir,
        }
    }

    pub fn project(&self) -> &Path {
        match self {
            Self::Worktree { project, .. } | Self::Shadow { project, .. } => project,
        }
    }

    pub fn is_git(&self) -> bool {
        matches!(self, Self::Worktree { .. })
    }

    fn base(&self) -> &str {
        match self {
            Self::Worktree { base, .. } | Self::Shadow { base, .. } => base,
        }
    }

    fn work_index(&self) -> Option<PathBuf> {
        match self {
            Self::Shadow { dir, .. } => Some(dir.with_extension("index")),
            Self::Worktree { .. } => None,
        }
    }

    /// Runs `f` with the work area's repository.
    fn with_work<T>(&self, f: impl FnOnce(Repo<'_>) -> Result<T>) -> Result<T> {
        let ix = self.work_index();
        let repo = match self {
            Self::Worktree { dir, .. } => Repo::at(dir),
            Self::Shadow { git_dir, dir, .. } => Repo {
                cwd: dir,
                shadow: Some((git_dir, dir)),
                index: ix.as_deref(),
            },
        };
        f(repo)
    }

    /// The project's repository (shadow: the shadow repository over the project).
    fn project_repo(&self) -> Repo<'_> {
        match self {
            Self::Worktree { project, .. } => Repo::at(project),
            Self::Shadow {
                project, git_dir, ..
            } => Repo {
                cwd: project,
                shadow: Some((git_dir, project)),
                index: None,
            },
        }
    }

    fn snapshot_project(&self) -> Result<String> {
        match self {
            Self::Worktree { project, .. } => Self::project_snapshot(project),
            Self::Shadow { base, .. } => self.project_repo().snapshot(Some(base), "project"),
        }
    }

    /// Makes the work area exactly `commit` (removes everything else that is
    /// not ignored).
    fn reset_to(&self, commit: &str) -> Result<()> {
        self.with_work(|r| {
            if self.is_git() {
                r.git(&["checkout", "-q", "--detach", "--force", commit], None)?;
                r.git(&["reset", "-q", "--hard", commit], None)?;
            } else {
                r.git(&["read-tree", "--reset", "-u", commit], None)?;
                r.git(&["checkout-index", "-a", "-f"], None)?;
            }
            r.git(&["clean", "-q", "-fd"], None)?;
            Ok(())
        })
    }

    /// The work so far as a commit on top of the base (for "retry": another
    /// model starts from exactly this state).
    pub fn checkpoint(&self) -> Result<String> {
        self.with_work(|r| r.snapshot(Some(self.base()), "ancilo checkpoint"))
    }

    /// Changes against the base, optionally only some paths.
    pub fn diff(&self, paths: Option<&[String]>) -> Result<Diff> {
        let base = self.base().to_string();
        let mut args = vec!["diff", "--cached", "--no-renames", "--numstat", "-z", &base];
        let mut pargs = vec!["diff", "--cached", "--no-renames", "--binary", &base];
        if let Some(p) = paths {
            args.push("--");
            pargs.push("--");
            args.extend(p.iter().map(String::as_str));
            pargs.extend(p.iter().map(String::as_str));
        }
        self.with_work(|r| {
            r.staged(|s| {
                Ok(Diff {
                    files: numstat(&s.git(&args, None)?),
                    patch: cut(s.git(&pargs, None)?),
                })
            })
        })
    }

    /// The patch of some of the changes (complete, never cut).
    fn patch_of(&self, paths: &[String]) -> Result<String> {
        if paths.is_empty() {
            return Ok(String::new());
        }
        let base = self.base().to_string();
        let mut args = vec!["diff", "--cached", "--no-renames", "--binary", &base, "--"];
        args.extend(paths.iter().map(String::as_str));
        self.with_work(|r| r.staged(|s| s.git(&args, None)))
    }

    fn select(&self, paths: Option<&[String]>) -> Result<(Vec<String>, Vec<String>)> {
        let all: Vec<String> = self.diff(None)?.files.into_iter().map(|f| f.path).collect();
        let selected: Vec<String> = match paths {
            Some(p) => all.iter().filter(|a| p.contains(a)).cloned().collect(),
            None => all.clone(),
        };
        let rest = all.into_iter().filter(|a| !selected.contains(a)).collect();
        Ok((selected, rest))
    }

    /// Puts changes (all, or `paths`) into the project. Returns the applied
    /// paths. All or nothing: the chosen changes and those that stay pending
    /// are checked against the project's current state first – on a conflict
    /// nothing changes anywhere.
    pub fn apply(&mut self, paths: Option<&[String]>) -> Result<Vec<String>> {
        let (selected, rest) = self.select(paths)?;
        if selected.is_empty() {
            return Ok(selected);
        }
        let sel_patch = self.patch_of(&selected)?;
        let rest_patch = self.patch_of(&rest)?;
        let now = self.snapshot_project()?;
        // The check runs in a private index – no file is touched.
        let tmp = tempfile::tempdir()?;
        let ix = tmp.path().join("index");
        let check = self.project_repo().with_index(&ix);
        check.git(&["read-tree", &now], None)?;
        check
            .git(
                &["apply", "--cached", "--check", "--binary", "-"],
                Some(&sel_patch),
            )
            .map_err(|e| {
                Error::Conflict(format!(
                    "the project changed in the meantime – the changes do not apply cleanly: {}",
                    e.message()
                ))
            })?;
        if !rest_patch.is_empty() {
            check.git(&["apply", "--cached", "--binary", "-"], Some(&sel_patch))?;
            check
                .git(&["apply", "--cached", "--check", "--binary", "-"], Some(&rest_patch))
                .map_err(|e| {
                    Error::Conflict(format!(
                        "the changes you keep would no longer fit the project – apply them together, or discard some first: {}",
                        e.message()
                    ))
                })?;
        }
        // Now for real: the project, then the work area on the new state.
        let project = self.project().to_path_buf();
        Repo::at(&project).git(&["apply", "--binary", "-"], Some(&sel_patch))?;
        let new_base = self.snapshot_project()?;
        self.set_base(&new_base);
        self.reset_to(&new_base)?;
        if !rest_patch.is_empty() {
            self.reapply(&rest_patch)?;
        }
        Ok(selected)
    }

    /// Re-applies pending changes to the work area. Checked beforehand; if
    /// the project changed in between, the patch is kept as a file.
    fn reapply(&self, patch: &str) -> Result<()> {
        let plain = self.with_work(|r| r.git(&["apply", "--binary", "-"], Some(patch)));
        if plain.is_ok() {
            return Ok(());
        }
        let three_way =
            self.with_work(|r| r.git(&["apply", "--3way", "--binary", "-"], Some(patch)));
        if three_way.is_ok() {
            return Ok(());
        }
        let file = self.work_dir().with_extension(format!(
            "pending-{}.patch",
            chrono::Utc::now().format("%Y%m%dT%H%M%S")
        ));
        std::fs::write(&file, patch)?;
        Err(Error::Conflict(format!(
            "the project changed while applying; the changes still pending are saved in {}",
            file.display()
        )))
    }

    fn set_base(&mut self, new: &str) {
        match self {
            Self::Worktree { base, .. } | Self::Shadow { base, .. } => *base = new.to_string(),
        }
    }

    /// Drops changes (all, or `paths`) from the work area without traces –
    /// the project is never touched. Returns the dropped paths.
    pub fn discard(&mut self, paths: Option<&[String]>) -> Result<Vec<String>> {
        let (selected, rest) = self.select(paths)?;
        let rest_patch = self.patch_of(&rest)?;
        let base = self.base().to_string();
        self.reset_to(&base)?;
        if !rest_patch.is_empty() {
            self.reapply(&rest_patch)?;
        }
        Ok(selected)
    }

    /// Takes over another work area's state and base (a variant the user
    /// chose) – after its changes were applied or checked.
    pub fn adopt(&mut self, other: &Changes) -> Result<()> {
        let state = other.checkpoint()?;
        self.set_base(other.base());
        self.reset_to(&state)
    }

    /// Removes the work area (the shadow repository stays with the session data).
    pub fn remove(&self) {
        match self {
            Self::Worktree { project, dir, .. } => {
                Repo::at(project)
                    .git(
                        &["worktree", "remove", "--force", &dir.display().to_string()],
                        None,
                    )
                    .ok();
            }
            Self::Shadow { .. } => {
                if let Some(ix) = self.work_index() {
                    std::fs::remove_file(ix).ok();
                }
            }
        }
        std::fs::remove_dir_all(self.work_dir()).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        ancilo_testkit::home::git_repo(d.path(), &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        d
    }

    fn plain() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "two\n").unwrap();
        d
    }

    fn area(project: &Path, data: &Path, name: &str) -> Changes {
        Changes::create(project, &data.join(name), &data.join("shadow.git")).unwrap()
    }

    fn paths(c: &Changes) -> Vec<String> {
        c.diff(None)
            .unwrap()
            .files
            .into_iter()
            .map(|f| f.path)
            .collect()
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap_or_default()
    }

    /// A same-size edit right after the work area was created (the same
    /// second as its index) must still show up – git's "racy" check depends
    /// on the index copy keeping the original's modification time.
    #[test]
    fn same_size_edits_in_the_same_second_are_seen() {
        for project in [repo(), plain()] {
            for i in 0..5 {
                let data = tempfile::tempdir().unwrap();
                let c = area(project.path(), data.path(), &format!("s{i}"));
                assert!(paths(&c).is_empty());
                std::fs::write(c.work_dir().join("a.txt"), "ONE\n").unwrap();
                assert_eq!(paths(&c), ["a.txt"], "round {i}");
                c.remove();
            }
        }
    }

    #[test]
    fn concurrent_readers_see_consistent_changes() {
        let project = repo();
        let data = tempfile::tempdir().unwrap();
        let c = area(project.path(), data.path(), "s");
        std::fs::write(c.work_dir().join("a.txt"), "one\nmore\n").unwrap();
        std::fs::write(c.work_dir().join("new.txt"), "fresh\n").unwrap();
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| (c.diff(None), c.checkpoint())))
                .collect();
            for h in handles {
                let (d, cp) = h.join().unwrap();
                let p: Vec<String> = d.unwrap().files.into_iter().map(|f| f.path).collect();
                assert_eq!(p, ["a.txt", "new.txt"]);
                cp.unwrap();
            }
        });
        // Reading changed nothing in the work area's own index.
        let status = Repo::at(c.work_dir())
            .git(&["status", "--porcelain"], None)
            .unwrap();
        assert!(
            status.contains(" M a.txt") && status.contains("?? new.txt"),
            "{status}"
        );
    }

    #[test]
    fn changes_reach_the_project_only_when_applied() {
        for project in [repo(), plain()] {
            let p = project.path();
            // Uncommitted and new files of the user are part of the base.
            std::fs::write(p.join("a.txt"), "one\nuser edit\n").unwrap();
            std::fs::write(p.join("notes.txt"), "user's new file\n").unwrap();
            let data = tempfile::tempdir().unwrap();
            let mut c = area(p, data.path(), "s");
            let work = c.work_dir().to_path_buf();
            assert_eq!(read(&work.join("a.txt")), "one\nuser edit\n");
            assert_eq!(read(&work.join("notes.txt")), "user's new file\n");
            std::fs::write(work.join("a.txt"), "one\nuser edit\nagent\n").unwrap();
            std::fs::write(work.join("new.txt"), "fresh\n").unwrap();
            std::fs::write(work.join("b.txt"), "changed\n").unwrap();
            assert_eq!(paths(&c), vec!["a.txt", "b.txt", "new.txt"]);
            assert!(!p.join("new.txt").exists(), "the project is untouched");
            // Apply one file, discard another, keep the third pending.
            c.apply(Some(&["new.txt".into()])).unwrap();
            assert_eq!(read(&p.join("new.txt")), "fresh\n");
            // An applied new file stays in the work area.
            assert_eq!(read(&work.join("new.txt")), "fresh\n");
            c.discard(Some(&["b.txt".into()])).unwrap();
            assert_eq!(paths(&c), vec!["a.txt"]);
            assert_eq!(read(&p.join("b.txt")), "two\n");
            c.apply(None).unwrap();
            assert_eq!(read(&p.join("a.txt")), "one\nuser edit\nagent\n");
            assert!(paths(&c).is_empty());
            // No repository appears in a project without one.
            assert_eq!(p.join(".git").exists(), c.is_git());
            c.remove();
        }
    }

    #[test]
    fn discarding_never_touches_the_project() {
        for project in [repo(), plain()] {
            let p = project.path();
            let data = tempfile::tempdir().unwrap();
            let mut c = area(p, data.path(), "s");
            std::fs::write(c.work_dir().join("a.txt"), "agent\n").unwrap();
            // The user works on the project meanwhile.
            std::fs::write(p.join("a.txt"), "user\n").unwrap();
            std::fs::write(p.join("mine.txt"), "mine\n").unwrap();
            c.discard(None).unwrap();
            assert_eq!(read(&p.join("a.txt")), "user\n");
            assert_eq!(read(&p.join("mine.txt")), "mine\n");
            assert!(paths(&c).is_empty());
        }
    }

    #[test]
    fn conflicts_change_nothing_anywhere() {
        for project in [repo(), plain()] {
            let p = project.path();
            let data = tempfile::tempdir().unwrap();
            let mut c = area(p, data.path(), "s");
            std::fs::write(c.work_dir().join("a.txt"), "agent version\n").unwrap();
            std::fs::write(c.work_dir().join("b.txt"), "agent b\n").unwrap();
            // The user changed b in the project: applying only a would leave
            // the pending b unable to fit – refused before anything happens.
            std::fs::write(p.join("b.txt"), "user b\n").unwrap();
            let err = c.apply(Some(&["a.txt".into()])).unwrap_err();
            assert!(err.message().contains("no longer fit"), "{}", err.message());
            assert_eq!(read(&p.join("a.txt")), "one\n");
            assert_eq!(paths(&c), ["a.txt", "b.txt"]);
            // Applying the conflicting file itself is refused as well.
            let err = c.apply(None).unwrap_err();
            assert!(
                err.message().contains("do not apply cleanly"),
                "{}",
                err.message()
            );
            assert_eq!(read(&p.join("a.txt")), "one\n");
            assert_eq!(read(&p.join("b.txt")), "user b\n");
        }
    }

    #[test]
    fn paths_are_names_not_patterns_and_renames_are_explicit() {
        let project = repo();
        let p = project.path();
        std::fs::write(p.join("*.txt"), "star\n").unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut c = area(p, data.path(), "s");
        let w = c.work_dir().to_path_buf();
        std::fs::write(w.join("*.txt"), "star changed\n").unwrap();
        std::fs::write(w.join("a.txt"), "a changed\n").unwrap();
        std::fs::rename(w.join("b.txt"), w.join("c.txt")).unwrap();
        assert_eq!(paths(&c), ["*.txt", "a.txt", "b.txt", "c.txt"]);
        // Discarding the file literally named `*.txt` keeps every other change.
        c.discard(Some(&["*.txt".into()])).unwrap();
        assert_eq!(paths(&c), ["a.txt", "b.txt", "c.txt"]);
        assert_eq!(read(&w.join("*.txt")), "star\n");
        // Discarding the rename's deletion only brings b back; c stays.
        c.discard(Some(&["b.txt".into()])).unwrap();
        assert_eq!(paths(&c), ["a.txt", "c.txt"]);
        assert_eq!(read(&w.join("c.txt")), "two\n");
    }

    #[test]
    fn variants_start_from_the_same_state_and_can_be_adopted() {
        for project in [repo(), plain()] {
            let data = tempfile::tempdir().unwrap();
            let main = area(project.path(), data.path(), "main");
            std::fs::write(main.work_dir().join("a.txt"), "turn 1\n").unwrap();
            let checkpoint = main.checkpoint().unwrap();
            let v = main.variant(&data.path().join("v"), &checkpoint).unwrap();
            assert_eq!(read(&v.work_dir().join("a.txt")), "turn 1\n");
            std::fs::write(v.work_dir().join("b.txt"), "variant\n").unwrap();
            let mut main = main;
            main.adopt(&v).unwrap();
            assert_eq!(paths(&main), vec!["a.txt", "b.txt"]);
            v.remove();
            main.remove();
        }
    }

    #[test]
    fn generated_folders_are_linked_not_copied() {
        let project = plain();
        std::fs::create_dir_all(project.path().join("node_modules/pkg")).unwrap();
        std::fs::write(project.path().join("node_modules/pkg/index.js"), "x").unwrap();
        let data = tempfile::tempdir().unwrap();
        let c = area(project.path(), data.path(), "s");
        let link = c.work_dir().join("node_modules");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(read(&link.join("pkg/index.js")), "x");
        assert!(paths(&c).is_empty(), "not a change");
    }
}
