//! Delegated tasks: Claude Code, Codex (and the CLI) hand work to Ancilo.
//!
//! - **synchronous** (default): the agent works directly in the project; the
//!   caller waits and gets a compact result (summary, changed files, diff stat)
//! - **background**: the agent works in its own git worktree on the branch
//!   `ancilo/<id>` and commits there; the caller merges when it wants to –
//!   nothing interferes with the work going on in the project meanwhile
//!
//! Tasks are persisted; after a daemon restart queued and background tasks
//! continue.

pub mod ops;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ancilo_agent::{Access, AgentOutcome, AgentSpec, FileChange, ShellSettings, Status, Workspace};
use ancilo_core::{Error, EventBus, Paths, Result};
use ancilo_gateway::Gateway;
use ancilo_gateway::scheduler::Priority;
use ancilo_models::routing::{AbOutcome, AbSource, AbTicket, RouteRequest, Via};
use ancilo_storage::Db;
use ancilo_storage::rusqlite::{OptionalExtension, params};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Role for delegated work (falls back to the default model).
pub const ROLE_DELEGATION: &str = "delegation";

const MAX_ACCESS_SETTING: &str = "tasks.max_access";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateInput {
    /// What to do: concrete and self-contained (the worker sees nothing else).
    pub task: String,
    /// Absolute path of the project directory.
    pub cwd: PathBuf,
    /// Files to look at first (relative to cwd).
    #[serde(default)]
    pub files: Vec<String>,
    /// `read`, `edit` (default) or `shell`.
    #[serde(default)]
    pub allow: Option<Access>,
    /// Work in a separate git worktree and return immediately with a task id.
    #[serde(default)]
    pub background: bool,
    /// Kind of task: tests, refactor, fix, docs, summary, other.
    #[serde(default)]
    pub kind: Option<String>,
    /// Model id or role. Default: role `delegation`, else the default model.
    #[serde(default)]
    pub model: Option<String>,
    /// Upper limit of agent steps (default 30).
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Offer the worker the project search (default true).
    #[serde(default)]
    pub search: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Done,
    Partial,
    Failed,
    Cancelled,
}

impl TaskStatus {
    fn from_agent(s: Status) -> Self {
        match s {
            Status::Done => Self::Done,
            Status::Partial => Self::Partial,
            Status::Failed => Self::Failed,
            Status::Cancelled => Self::Cancelled,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_final(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Where a background task works.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// The commit the worktree started from.
    pub base: String,
}

/// Compact result for the caller – deliberately short to save its context.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskView {
    pub task_id: String,
    pub status: TaskStatus,
    pub model: String,
    /// Why this model: explicit, kind, role, default or ab_test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// Task kind (given or estimated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub summary: Option<String>,
    pub changed_files: Vec<FileChange>,
    /// e.g. `+148 −3 in 2 files`
    pub diff_stat: Option<String>,
    /// Background tasks: branch with the committed result.
    pub branch: Option<String>,
    pub commit: Option<String>,
    /// How to take over a background result.
    pub next_step: Option<String>,
    pub steps: Option<u32>,
    /// Tokens the worker used (prompt + completion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    /// Only with `detail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    input: DelegateInput,
    model: String,
    worktree: Option<Worktree>,
    /// How the model was chosen and the task kind used for it.
    #[serde(default)]
    via: Option<Via>,
    #[serde(default)]
    kind: Option<String>,
    /// Set when the task is part of an A/B test.
    #[serde(default)]
    ab: Option<AbTicket>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredResult {
    outcome: Option<AgentOutcome>,
    commit: Option<String>,
    error: Option<String>,
    duration_ms: Option<u64>,
}

pub struct Options {
    pub max_concurrent: usize,
    pub timeout: Duration,
    pub default_max_steps: u32,
    pub shell: ShellSettings,
    /// Offered to the worker as the tool `search` (the project index).
    pub search: Option<std::sync::Arc<dyn ancilo_agent::CodeSearch>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            timeout: Duration::from_secs(20 * 60),
            default_max_steps: 30,
            shell: ShellSettings::default(),
            search: None,
        }
    }
}

struct Inner {
    db: Db,
    bus: EventBus,
    gateway: Gateway,
    paths: Paths,
    options: Options,
    running: Mutex<HashMap<String, CancellationToken>>,
    slots: tokio::sync::Semaphore,
    /// Set during daemon shutdown: interrupted tasks keep their open status
    /// so that they resume after the restart.
    stopping: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub struct TaskRunner {
    inner: Arc<Inner>,
}

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ancilo")
        .env("GIT_AUTHOR_EMAIL", "ancilo@localhost")
        .env("GIT_COMMITTER_NAME", "Ancilo")
        .env("GIT_COMMITTER_EMAIL", "ancilo@localhost")
        .output()
        .await
        .map_err(|e| Error::unavailable(format!("git is not available: {e}")))?;
    if !out.status.success() {
        return Err(Error::Conflict(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn diff_stat(files: &[FileChange]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let (a, r): (usize, usize) = files
        .iter()
        .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    Some(format!(
        "+{a} −{r} in {} file{}",
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    ))
}

impl TaskRunner {
    pub fn new(db: Db, bus: EventBus, gateway: Gateway, paths: Paths, options: Options) -> Self {
        let slots = tokio::sync::Semaphore::new(options.max_concurrent.max(1));
        Self {
            inner: Arc::new(Inner {
                db,
                bus,
                gateway,
                paths,
                options,
                running: Mutex::new(HashMap::new()),
                slots,
                stopping: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    fn save(
        &self,
        id: &str,
        status: TaskStatus,
        stored: &Stored,
        result: Option<&StoredResult>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let request = serde_json::to_string(stored)?;
        let result = result.map(serde_json::to_string).transpose()?;
        let mode = if stored.input.background {
            "background"
        } else {
            "sync"
        };
        self.inner.db.with(|c| {
            c.execute(
                "INSERT INTO tasks(id, created_at, updated_at, status, mode, request, result) VALUES(?1, ?2, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at, status = excluded.status,
                   request = excluded.request, result = COALESCE(excluded.result, tasks.result)",
                params![id, now, status.as_str(), mode, request, result],
            )
            .map(|_| ())
        })
    }

    fn load(&self, id: &str) -> Result<(TaskStatus, Stored, StoredResult)> {
        let row: Option<(String, String, Option<String>)> = self.inner.db.with(|c| {
            c.query_row(
                "SELECT status, request, result FROM tasks WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })?;
        let (status, request, result) =
            row.ok_or_else(|| Error::not_found(format!("no task '{id}'")))?;
        let status: TaskStatus = serde_json::from_value(json!(status))?;
        let stored: Stored = serde_json::from_str(&request)?;
        let result: StoredResult = result
            .map(|r| serde_json::from_str(&r))
            .transpose()?
            .unwrap_or_default();
        Ok((status, stored, result))
    }

    fn view(&self, id: &str, detail: bool) -> Result<TaskView> {
        let (status, stored, result) = self.load(id)?;
        let outcome = result.outcome.as_ref();
        let changed = outcome.map(|o| o.changed_files.clone()).unwrap_or_default();
        let branch = stored.worktree.as_ref().map(|w| w.branch.clone());
        let next_step = match (&branch, &result.commit, status) {
            (Some(b), Some(_), TaskStatus::Done | TaskStatus::Partial) => Some(format!(
                "review with `git diff HEAD...{b}`, take over with `git merge {b}`"
            )),
            _ => None,
        };
        Ok(TaskView {
            task_id: id.to_string(),
            status,
            model: stored.model,
            via: stored.via,
            kind: stored.kind,
            summary: outcome
                .map(|o| o.summary.chars().take(2000).collect())
                .or(result.error.clone()),
            diff_stat: diff_stat(&changed),
            changed_files: changed,
            branch,
            commit: result.commit.clone(),
            next_step,
            steps: outcome.map(|o| o.steps),
            tokens: outcome.map(|o| o.prompt_tokens + o.completion_tokens),
            duration_ms: result.duration_ms,
            diff: if detail {
                outcome.map(|o| o.diff.clone())
            } else {
                None
            },
        })
    }

    /// Hands a task to Ancilo. Synchronous tasks return when done; background
    /// tasks return immediately (status `queued`).
    pub async fn delegate(&self, input: DelegateInput) -> Result<TaskView> {
        if input.task.trim().is_empty() {
            return Err(Error::invalid("task is empty"));
        }
        if !input.cwd.is_absolute() || !input.cwd.is_dir() {
            return Err(Error::invalid(format!(
                "cwd must be an existing absolute directory: {}",
                input.cwd.display()
            )));
        }
        let id = format!("t-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let routed = self.inner.gateway.manager().route(&RouteRequest {
            model: input.model.as_deref(),
            kind: input.kind.as_deref(),
            role: ROLE_DELEGATION,
            text: Some(&input.task),
            ab: Some(AbSource::Delegation),
            subject: Some(&id),
        })?;
        if self.inner.gateway.manager().is_cloud(&routed.model) {
            return Err(Error::PermissionDenied(format!(
                "'{}' is a cloud model – code from your projects is never sent to the cloud; use a local model",
                routed.model
            )));
        }
        if let Some(ticket) = &routed.ab
            && let Some(shadow) = &ticket.shadow_model
        {
            self.start_shadow(&input, shadow, ticket).await;
        }
        self.submit(
            id,
            input,
            routed.model,
            Some(routed.via),
            routed.kind,
            routed.ab,
        )
        .await
    }

    /// Shadow mode of an A/B test: B does the same task in its own worktree,
    /// without effect on the caller's result. Needs a git repository.
    async fn start_shadow(&self, input: &DelegateInput, model: &str, ticket: &AbTicket) {
        let id = format!("t-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let manager = self.inner.gateway.manager();
        let Ok(b_ticket) = manager.ab_shadow_assigned(ticket, Some(&id)) else {
            return;
        };
        let shadow = DelegateInput {
            background: true,
            model: Some(model.to_string()),
            ..input.clone()
        };
        if let Err(e) = Box::pin(self.submit(
            id,
            shadow,
            model.to_string(),
            Some(Via::AbTest),
            None,
            Some(b_ticket.clone()),
        ))
        .await
        {
            // No worktree possible (no git): record as not comparable.
            tracing::info!(error = %e.message(), "shadow run skipped");
            manager
                .ab_record(
                    &b_ticket,
                    &AbOutcome {
                        error: true,
                        ..Default::default()
                    },
                )
                .ok();
        }
    }

    async fn submit(
        &self,
        id: String,
        input: DelegateInput,
        model: String,
        via: Option<Via>,
        kind: Option<String>,
        ab: Option<AbTicket>,
    ) -> Result<TaskView> {
        let mut input = input;
        let access = input.allow.unwrap_or(Access::Edit).min(self.max_access());
        input.allow = Some(access);
        let worktree = if input.background {
            let top = git(&input.cwd, &["rev-parse", "--show-toplevel"]).await;
            match top {
                Ok(top) => {
                    let base = git(Path::new(&top), &["rev-parse", "HEAD"]).await?;
                    let branch = format!("ancilo/{}", &id[2..]);
                    let path = self.inner.paths.worktrees_dir().join(&id);
                    git(
                        Path::new(&top),
                        &[
                            "worktree",
                            "add",
                            "-q",
                            "-b",
                            &branch,
                            &path.display().to_string(),
                            &base,
                        ],
                    )
                    .await?;
                    Some(Worktree { path, branch, base })
                }
                Err(_) if access == Access::Read => None,
                Err(_) => {
                    return Err(Error::invalid(
                        "background tasks that change files need a git repository (they work on their own branch); use background=false or allow=read",
                    ));
                }
            }
        } else {
            None
        };
        let stored = Stored {
            input,
            model,
            worktree,
            via,
            kind,
            ab,
        };
        self.save(&id, TaskStatus::Queued, &stored, None)?;
        self.inner.bus.emit(
            "task.queued",
            Some(&id),
            json!({"model": stored.model, "background": stored.input.background}),
        );
        let cancel = CancellationToken::new();
        self.inner
            .running
            .lock()
            .unwrap()
            .insert(id.clone(), cancel.clone());
        if stored.input.background {
            let me = self.clone();
            let id2 = id.clone();
            tokio::spawn(async move { me.execute(&id2, stored, cancel).await });
            return self.view(&id, false);
        }
        self.execute(&id, stored, cancel).await;
        self.view(&id, false)
    }

    async fn execute(&self, id: &str, stored: Stored, cancel: CancellationToken) {
        let _slot = tokio::select! {
            p = self.inner.slots.acquire() => p,
            _ = cancel.cancelled() => {
                if self.inner.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    self.inner.running.lock().unwrap().remove(id);
                    return;
                }
                let _ = self.save(id, TaskStatus::Cancelled, &stored, Some(&StoredResult { error: Some("cancelled while queued".into()), ..Default::default() }));
                self.inner.bus.emit("task.cancelled", Some(id), json!({}));
                self.inner.running.lock().unwrap().remove(id);
                return;
            }
        };
        let started = Instant::now();
        let _ = self.save(id, TaskStatus::Running, &stored, None);
        self.inner
            .bus
            .emit("task.started", Some(id), json!({"model": stored.model}));
        let result = self.run_agent(id, &stored, cancel).await;
        if self
            .inner
            .stopping
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            // Interrupted by a shutdown – not a result. Stays "running".
            self.inner.running.lock().unwrap().remove(id);
            return;
        }
        let (status, mut result) = match result {
            Ok((status, r)) => (status, r),
            Err(e) => (
                TaskStatus::Failed,
                StoredResult {
                    error: Some(e.message()),
                    ..Default::default()
                },
            ),
        };
        result.duration_ms = Some(started.elapsed().as_millis() as u64);
        let _ = self.save(id, status, &stored, Some(&result));
        if let Some(ticket) = &stored.ab
            && status != TaskStatus::Cancelled
        {
            let outcome = AbOutcome {
                success: Some(status == TaskStatus::Done),
                latency_ms: result.duration_ms.unwrap_or(0),
                error: status == TaskStatus::Failed,
                interventions: result.outcome.as_ref().map_or(0, |o| o.interventions),
            };
            if let Err(e) = self.inner.gateway.manager().ab_record(ticket, &outcome) {
                tracing::warn!(error = %e.message(), "recording the A/B outcome failed");
            }
        }
        self.inner.running.lock().unwrap().remove(id);
        let kind = match status {
            TaskStatus::Failed => "task.failed",
            TaskStatus::Cancelled => "task.cancelled",
            _ => "task.finished",
        };
        self.inner.bus.emit(
            kind,
            Some(id),
            json!({"status": status.as_str(), "duration_ms": result.duration_ms}),
        );
    }

    async fn run_agent(
        &self,
        id: &str,
        stored: &Stored,
        cancel: CancellationToken,
    ) -> Result<(TaskStatus, StoredResult)> {
        let root = stored
            .worktree
            .as_ref()
            .map(|w| w.path.clone())
            .unwrap_or_else(|| stored.input.cwd.clone());
        let access = stored.input.allow.unwrap_or(Access::Edit);
        let mut ws = Workspace::new(&root, access)
            .map_err(|e| Error::invalid(format!("cannot open {}: {e}", root.display())))?
            .with_events(self.inner.bus.clone(), id)
            .with_shell(self.inner.options.shell.clone());
        if let Some(s) = &self.inner.options.search
            && stored.input.search != Some(false)
        {
            ws = ws.with_search(s.clone());
        }
        let mut task = stored.input.task.clone();
        if !stored.input.files.is_empty() {
            task.push_str(&format!(
                "\n\nStart with these files: {}",
                stored.input.files.join(", ")
            ));
        }
        let spec = AgentSpec {
            model: stored.model.clone(),
            system: ancilo_agent::WORKER_PROMPT.into(),
            task,
            max_steps: stored
                .input
                .max_steps
                .unwrap_or(self.inner.options.default_max_steps),
            priority: if stored.input.background {
                Priority::Background
            } else {
                Priority::Sync
            },
            reliability: None,
            temperature: None,
            seed: None,
            history: Vec::new(),
            local_only: true,
            think: true,
        };
        let limit = cancel.child_token();
        let timer = {
            let limit = limit.clone();
            let t = self.inner.options.timeout;
            tokio::spawn(async move {
                tokio::time::sleep(t).await;
                limit.cancel();
            })
        };
        let outcome = ancilo_agent::run(
            &self.inner.gateway,
            &ws,
            spec,
            limit.clone(),
            Some((self.inner.bus.clone(), id.to_string())),
        )
        .await;
        timer.abort();
        let timed_out = limit.is_cancelled() && !cancel.is_cancelled();
        // Task results stay compact: the conversation is not kept.
        let mut outcome = outcome;
        outcome.messages.clear();
        let mut status = TaskStatus::from_agent(outcome.status);
        if timed_out {
            status = TaskStatus::Partial;
            outcome.summary = format!(
                "time limit of {} min reached; the changes so far are kept",
                self.inner.options.timeout.as_secs() / 60
            );
        }
        let mut commit = None;
        if let Some(wt) = &stored.worktree
            && !outcome.changed_files.is_empty()
        {
            git(&wt.path, &["add", "-A"]).await?;
            let subject: String = stored
                .input
                .task
                .lines()
                .next()
                .unwrap_or("task")
                .chars()
                .take(60)
                .collect();
            let message = format!("ancilo: {subject}\n\n{}", outcome.summary);
            git(&wt.path, &["commit", "-q", "-m", &message]).await?;
            commit = Some(git(&wt.path, &["rev-parse", "HEAD"]).await?);
        }
        Ok((
            status,
            StoredResult {
                outcome: Some(outcome),
                commit,
                error: None,
                duration_ms: None,
            },
        ))
    }

    /// The most a delegated task may do (the user's setting; default: shell,
    /// which is sandboxed). Requests for more are limited to it.
    pub fn max_access(&self) -> Access {
        self.inner
            .db
            .get_setting(MAX_ACCESS_SETTING)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_value(json!(s)).ok())
            .unwrap_or(Access::Shell)
    }

    pub fn set_max_access(&self, access: Access) -> Result<Access> {
        let s = serde_json::to_value(access)?;
        self.inner
            .db
            .set_setting(MAX_ACCESS_SETTING, s.as_str().unwrap_or("shell"))?;
        self.inner
            .bus
            .emit("settings.changed", None, json!({"max_access": s}));
        Ok(access)
    }

    pub fn status(&self, id: &str) -> Result<TaskView> {
        self.view(id, false)
    }

    /// Waits until the task is final (or `timeout`), then returns it.
    pub async fn result(&self, id: &str, detail: bool, wait: Option<Duration>) -> Result<TaskView> {
        let deadline = wait.map(|w| Instant::now() + w);
        loop {
            let v = self.view(id, detail)?;
            if v.status.is_final() || deadline.is_none_or(|d| Instant::now() >= d) {
                return Ok(v);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub fn cancel(&self, id: &str) -> Result<TaskView> {
        let token = self.inner.running.lock().unwrap().get(id).cloned();
        match token {
            Some(t) => t.cancel(),
            None => {
                let v = self.view(id, false)?;
                if v.status.is_final() {
                    return Ok(v);
                }
            }
        }
        self.view(id, false)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<TaskView>> {
        let ids: Vec<String> = self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT id FROM tasks ORDER BY created_at DESC LIMIT ?1")?;
            let rows = s.query_map(params![limit as i64], |r| r.get(0))?;
            rows.collect()
        })?;
        ids.iter().map(|id| self.view(id, false)).collect()
    }

    /// Daemon shutdown: interrupts running tasks without recording a result
    /// (they resume after the restart) and waits until they have stopped.
    pub async fn shutdown(&self) {
        self.inner
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let tokens: Vec<CancellationToken> = self
            .inner
            .running
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for t in tokens {
            t.cancel();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.inner.running.lock().unwrap().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// After a daemon restart: queued and background tasks continue;
    /// interrupted synchronous tasks are reported as failed (their caller is gone).
    pub async fn restore(&self) -> Result<()> {
        let open: Vec<(String, String)> = self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT id, status FROM tasks WHERE status IN ('queued', 'running') ORDER BY created_at")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        for (id, status) in open {
            let (_, stored, _) = self.load(&id)?;
            if !stored.input.background && status == "running" {
                self.save(
                    &id,
                    TaskStatus::Failed,
                    &stored,
                    Some(&StoredResult {
                        error: Some("interrupted by a restart of Ancilo".into()),
                        ..Default::default()
                    }),
                )?;
                self.inner
                    .bus
                    .emit("task.failed", Some(&id), json!({"reason": "restart"}));
                continue;
            }
            if let Some(wt) = &stored.worktree {
                // Start over from the base commit.
                git(&wt.path, &["reset", "-q", "--hard", &wt.base])
                    .await
                    .ok();
                git(&wt.path, &["clean", "-q", "-fd"]).await.ok();
            }
            let cancel = CancellationToken::new();
            self.inner
                .running
                .lock()
                .unwrap()
                .insert(id.clone(), cancel.clone());
            self.save(&id, TaskStatus::Queued, &stored, None)?;
            let me = self.clone();
            tokio::spawn(async move { me.execute(&id, stored, cancel).await });
        }
        Ok(())
    }
}
