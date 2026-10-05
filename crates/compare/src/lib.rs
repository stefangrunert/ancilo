//! Comparing models on the user's own tasks and machine (M4).
//!
//! - **comparison:** the same task on N models, each run in its own git
//!   worktree from the same commit, with identical prompt, tools, pipeline,
//!   temperature and seeds (all logged); checked objectively by a command
//! - **suite run:** a saved task collection (eval format) on N models – a
//!   personal benchmark
//! - **leaderboard** per model × task kind, **recommendations** from results
//!   with enough data (applied only with a confirmation)
//!
//! Runs are sequential, model by model: parallel runs would share GPU
//! bandwidth and distort durations and tokens/s (see decision
//! `2026-09-30-m4-umsetzung`). Load time is measured separately.

pub mod ops;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ancilo_agent::{Access, AgentOutcome, AgentSpec, ShellSettings, Status, Workspace};
use ancilo_core::stats::{self, Difference, Rate};
use ancilo_core::{Error, EventBus, Paths, Result};
use ancilo_eval::delegation::{Suite, Task as SuiteTask};
use ancilo_gateway::scheduler::Priority;
use ancilo_gateway::{CallOpts, ChatReply, Gateway};
use ancilo_models::routing::{KINDS, RouteRequest, estimate_kind};
use ancilo_storage::Db;
use ancilo_storage::rusqlite::{OptionalExtension, params};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// Results per model below this count give no recommendation.
pub const MIN_SAMPLES_RECOMMENDATION: u64 = 10;
const RUN_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const LOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_REPEAT: u32 = 20;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareInput {
    /// The task, as for `delegate`.
    pub task: String,
    /// Absolute path inside a git repository; runs start from its HEAD commit.
    pub cwd: PathBuf,
    /// Models to compare (ids, names or roles).
    pub models: Vec<String>,
    /// Command that decides success, e.g. `cargo test` (runs in the sandbox).
    #[serde(default)]
    pub check: Option<String>,
    /// Allow network access for the check command.
    #[serde(default)]
    pub check_network: bool,
    /// Runs per model (default 1, at most 20).
    #[serde(default)]
    pub repeat: Option<u32>,
    /// `read`, `edit` (default) or `shell`.
    #[serde(default)]
    pub allow: Option<Access>,
    /// Hide which model is which until a rating is given.
    #[serde(default)]
    pub blind: bool,
    /// Task kind (tests, refactor, fix, docs, summary, other); estimated if missing.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Optional judge model: rates each result 1–10 (marked as model-based).
    #[serde(default)]
    pub judge: Option<String>,
    /// Base seed; run i uses seed + i.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Files to look at first.
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuiteRunInput {
    /// Built-in (`delegation`), saved suite name, or path to a YAML file.
    pub suite: String,
    pub models: Vec<String>,
    /// Runs per task and model (default 1).
    #[serde(default)]
    pub repeat: Option<u32>,
    #[serde(default)]
    pub max_steps: Option<u32>,
    #[serde(default)]
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Compare(CompareInput),
    Suite(SuiteRunInput),
}

/// Everything that makes runs comparable – logged with every comparison.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunConfig {
    pub worker_prompt_sha256: String,
    pub temperature: f64,
    pub seed: u64,
    pub max_steps: u32,
    pub reliability: Value,
    pub tools: Vec<String>,
    pub allow: Option<Access>,
    pub check: Option<String>,
    pub check_network: bool,
    pub base_commit: Option<String>,
    pub order: String,
    pub ancilo_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckResult {
    pub passed: bool,
    pub exit_code: Option<i32>,
    /// Last part of the output.
    pub output: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    pub label: String,
    /// Hidden while a blind comparison is not rated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Suite runs: the task id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suite_task: Option<String>,
    pub kind: String,
    pub index: u32,
    pub success: bool,
    /// Agent status: done, partial, failed, cancelled.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckResult>,
    /// Why it failed (check output, suite check, error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub duration_ms: u64,
    pub load_ms: u64,
    pub generation_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub tokens_per_s: Option<f64>,
    pub steps: u32,
    pub tool_calls: u32,
    pub interventions: u32,
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Arm {
    pub label: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Rating {
    /// Best label, or `tie`.
    pub best: String,
    pub note: Option<String>,
    pub rated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JudgeScore {
    pub label: String,
    /// 1–10; `None` if the judge gave no usable answer.
    pub score: Option<f64>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Judgement {
    pub judge_model: String,
    /// Always "model-based – not an objective measurement".
    pub note: String,
    pub scores: Vec<JudgeScore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct State {
    /// Top directory of the compared repository (branches live there).
    #[serde(default)]
    repo: Option<PathBuf>,
    kind: String,
    kind_estimated: bool,
    config: RunConfig,
    arms: Vec<Arm>,
    blind: bool,
    runs: Vec<RunResult>,
    load_ms: HashMap<String, u64>,
    rating: Option<Rating>,
    judgement: Option<Judgement>,
    error: Option<String>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl ComparisonStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_final(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArmSummary {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub runs: u64,
    pub success: Rate,
    pub duration_p50_ms: Option<u64>,
    pub duration_p95_ms: Option<u64>,
    /// Measured once before the runs; not part of the durations.
    pub load_ms: Option<u64>,
    pub tokens_per_s: Option<f64>,
    pub tokens_mean: Option<f64>,
    pub steps_mean: Option<f64>,
    pub interventions: u64,
    /// e.g. `+12 −3 in 2 files` (median run by size)
    pub typical_diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ComparisonReport {
    pub id: String,
    /// compare | suite
    pub mode: String,
    pub status: ComparisonStatus,
    pub title: String,
    pub kind: String,
    pub kind_estimated: bool,
    pub blind: bool,
    /// Blind comparisons: whether the mapping is revealed (after a rating).
    pub revealed: bool,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub config: RunConfig,
    pub progress: String,
    /// Sorted: success, then lower confidence bound, then duration.
    pub ranking: Vec<ArmSummary>,
    /// Pairwise: is the leader significantly better than each other arm?
    pub verdict: String,
    pub runs: Vec<RunResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<Rating>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judgement: Option<Judgement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LeaderboardEntry {
    pub model: String,
    pub kind: String,
    pub success: Rate,
    pub duration_p50_ms: Option<u64>,
    pub tokens_per_s: Option<f64>,
}

/// How often the user took a model's result over another one's
/// ("retry with another model") – subjective, kept apart from measurements.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChoiceEntry {
    pub model: String,
    /// Taken over the other model's result.
    pub chosen: u64,
    /// The other model's result was taken instead.
    pub passed_over: u64,
}

/// What the results of one task kind allow to say.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Conclusion {
    pub kind: String,
    /// Set only when the best model is ahead with statistical significance.
    pub best: Option<String>,
    /// One sentence for people and the assistant.
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Leaderboard {
    pub entries: Vec<LeaderboardEntry>,
    /// Per task kind: is there a reliable best model?
    #[serde(default)]
    pub conclusions: Vec<Conclusion>,
    /// The user's choices in coding sessions.
    #[serde(default)]
    pub choices: Vec<ChoiceEntry>,
    /// The same as Markdown – for the knowledge base and the assistant.
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    SetRoute { kind: String, model: String },
    AssignRole { role: String, model: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Recommendation {
    pub id: String,
    pub created_at: DateTime<Utc>,
    /// open | applied | dismissed
    pub status: String,
    pub action: Action,
    pub rationale: String,
}

struct Inner {
    db: Db,
    bus: EventBus,
    gateway: Gateway,
    paths: Paths,
    shell: ShellSettings,
    search: Option<Arc<dyn ancilo_agent::CodeSearch>>,
    running: Mutex<HashMap<String, CancellationToken>>,
    /// One comparison at a time – measurements must not disturb each other.
    slot: tokio::sync::Mutex<()>,
    /// Recommendations are checked, then created: one refresh at a time, or
    /// two concurrent ones create the same recommendation twice.
    refreshing: Mutex<()>,
}

#[derive(Clone)]
pub struct Comparer {
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

fn labels(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let c = (b'A' + (i % 26) as u8) as char;
            if i < 26 {
                c.to_string()
            } else {
                format!("{c}{}", i / 26)
            }
        })
        .collect()
}

/// Fisher–Yates with a SplitMix64 stream – the blind labels.
pub fn shuffle<T>(items: &mut [T], mut seed: u64) {
    for i in (1..items.len()).rev() {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        items.swap(i, (z % (i as u64 + 1)) as usize);
    }
}

fn random_u64() -> u64 {
    u64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap())
}

fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_string()
    } else {
        format!("…{}", s.chars().skip(count - n).collect::<String>())
    }
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn median_f(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    Some(s[(s.len() - 1) / 2])
}

fn diff_stat(files: usize, added: usize, removed: usize) -> String {
    format!(
        "+{added} −{removed} in {files} file{}",
        if files == 1 { "" } else { "s" }
    )
}

fn summarize(
    label: &str,
    model: Option<String>,
    runs: &[&RunResult],
    load_ms: Option<u64>,
) -> ArmSummary {
    let durations: Vec<u64> = runs.iter().map(|r| r.duration_ms).collect();
    let tps: Vec<f64> = runs.iter().filter_map(|r| r.tokens_per_s).collect();
    let tokens: Vec<f64> = runs
        .iter()
        .map(|r| (r.prompt_tokens + r.completion_tokens) as f64)
        .collect();
    let steps: Vec<f64> = runs.iter().map(|r| f64::from(r.steps)).collect();
    let mut by_size: Vec<&&RunResult> = runs.iter().collect();
    by_size.sort_by_key(|r| r.lines_added + r.lines_removed);
    ArmSummary {
        label: label.to_string(),
        model,
        runs: runs.len() as u64,
        success: Rate::new(
            runs.iter().filter(|r| r.success).count() as u64,
            runs.len() as u64,
        ),
        duration_p50_ms: stats::percentile(&durations, 50.0),
        duration_p95_ms: stats::percentile(&durations, 95.0),
        load_ms,
        tokens_per_s: median_f(&tps),
        tokens_mean: mean(&tokens),
        steps_mean: mean(&steps),
        interventions: runs.iter().map(|r| u64::from(r.interventions)).sum(),
        typical_diff: by_size
            .get(by_size.len().saturating_sub(1) / 2)
            .map(|r| diff_stat(r.files_changed, r.lines_added, r.lines_removed)),
    }
}

/// Ranks arms and states whether the leader is significantly better.
fn rank(mut arms: Vec<ArmSummary>) -> (Vec<ArmSummary>, String) {
    arms.sort_by(|a, b| {
        b.success
            .rate
            .total_cmp(&a.success.rate)
            .then(b.success.low.total_cmp(&a.success.low))
            .then(
                a.duration_p50_ms
                    .unwrap_or(u64::MAX)
                    .cmp(&b.duration_p50_ms.unwrap_or(u64::MAX)),
            )
    });
    let verdict = match arms.first() {
        None => "no runs yet".to_string(),
        Some(_) if arms.len() == 1 => "only one model".to_string(),
        Some(lead) => {
            let parts: Vec<String> = arms[1..]
                .iter()
                .map(|other| {
                    let (d, p) = stats::compare_rates(&other.success, &lead.success, 5, 0.05);
                    match d {
                        Difference::InsufficientData => format!(
                            "{} vs. {}: too few runs for a statement (at least 5 each)",
                            lead.label, other.label
                        ),
                        Difference::SecondBetter => format!(
                            "{} is significantly more successful than {} (p = {:.3})",
                            lead.label,
                            other.label,
                            p.unwrap_or(0.0)
                        ),
                        _ => format!(
                            "{} vs. {}: no significant difference in success",
                            lead.label, other.label
                        ),
                    }
                })
                .collect();
            parts.join("; ")
        }
    };
    (arms, verdict)
}

/// Extracts `{"score": n, "reason": "…"}` from a judge answer, tolerating
/// text around it; falls back to the first number.
pub fn parse_judge(text: &str) -> (Option<f64>, String) {
    if let (Some(s), Some(e)) = (text.find('{'), text.rfind('}'))
        && s < e
        && let Ok(v) = serde_json::from_str::<Value>(&text[s..=e])
        && let Some(score) = v["score"].as_f64()
    {
        return (
            Some(score.clamp(1.0, 10.0)),
            v["reason"].as_str().unwrap_or_default().to_string(),
        );
    }
    let number: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    (
        number.parse::<f64>().ok().map(|s| s.clamp(1.0, 10.0)),
        tail(text.trim(), 300),
    )
}

impl Comparer {
    pub fn new(
        db: Db,
        bus: EventBus,
        gateway: Gateway,
        paths: Paths,
        shell: ShellSettings,
        search: Option<Arc<dyn ancilo_agent::CodeSearch>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                bus,
                gateway,
                paths,
                shell,
                search,
                running: Mutex::new(HashMap::new()),
                slot: tokio::sync::Mutex::new(()),
                refreshing: Mutex::new(()),
            }),
        }
    }

    fn suites_dir(&self) -> PathBuf {
        self.inner.paths.home().join("suites")
    }

    // ---- persistence -------------------------------------------------------

    fn save(
        &self,
        id: &str,
        status: ComparisonStatus,
        request: &Request,
        state: &State,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let request = serde_json::to_string(request)?;
        let state = serde_json::to_string(state)?;
        self.inner.db.with(|c| {
            c.execute(
                "INSERT INTO comparisons(id, created_at, updated_at, status, request, state) VALUES(?1, ?2, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at, status = excluded.status, state = excluded.state",
                params![id, now, status.as_str(), request, state],
            )
            .map(|_| ())
        })
    }

    fn load(&self, id: &str) -> Result<(ComparisonStatus, DateTime<Utc>, Request, State)> {
        let row: Option<(String, String, String, String)> = self.inner.db.with(|c| {
            c.query_row(
                "SELECT status, created_at, request, state FROM comparisons WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
        })?;
        let (status, created, request, state) =
            row.ok_or_else(|| Error::not_found(format!("no comparison '{id}'")))?;
        Ok((
            serde_json::from_value(json!(status))?,
            DateTime::parse_from_rfc3339(&created)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            serde_json::from_str(&request)?,
            serde_json::from_str(&state)?,
        ))
    }

    fn store_runs(&self, id: &str, state: &State) -> Result<()> {
        let mapping: HashMap<&str, &str> = state
            .arms
            .iter()
            .map(|a| (a.label.as_str(), a.model.as_str()))
            .collect();
        self.inner.db.with(|c| {
            c.execute(
                "DELETE FROM compare_runs WHERE comparison_id = ?1",
                params![id],
            )?;
            for r in &state.runs {
                let model = mapping.get(r.label.as_str()).copied().unwrap_or_default();
                c.execute(
                    "INSERT INTO compare_runs(comparison_id, ts, model_id, label, kind, suite_task, idx, success, data)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        Utc::now().to_rfc3339(),
                        model,
                        r.label,
                        r.kind,
                        r.suite_task,
                        r.index,
                        r.success,
                        serde_json::to_string(r).unwrap_or_default()
                    ],
                )?;
            }
            Ok(())
        })
    }

    // ---- starting ----------------------------------------------------------

    fn resolve_models(&self, models: &[String]) -> Result<Vec<String>> {
        if models.is_empty() {
            return Err(Error::invalid("name at least one model"));
        }
        let manager = self.inner.gateway.manager();
        let mut ids = Vec::new();
        for m in models {
            let id = manager.resolve_strict(m)?;
            if manager.is_cloud(&id) {
                return Err(Error::PermissionDenied(format!(
                    "'{id}' is a cloud model – comparisons run on your code, which never goes to the cloud"
                )));
            }
            if ids.contains(&id) {
                return Err(Error::invalid(format!("model '{id}' is listed twice")));
            }
            ids.push(id);
        }
        Ok(ids)
    }

    fn config(
        &self,
        seed: u64,
        max_steps: u32,
        allow: Option<Access>,
        check: Option<String>,
        check_network: bool,
        base: Option<String>,
    ) -> RunConfig {
        let tools = Workspace::new(Path::new("/"), allow.unwrap_or(Access::Edit))
            .map(|w| {
                w.definitions()
                    .iter()
                    .filter_map(|d| d["function"]["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        RunConfig {
            worker_prompt_sha256: hex::encode(Sha256::digest(
                ancilo_agent::WORKER_PROMPT.as_bytes(),
            )),
            temperature: ancilo_agent::DEFAULT_TEMPERATURE,
            seed,
            max_steps,
            reliability: serde_json::to_value(self.inner.gateway.reliability())
                .unwrap_or(Value::Null),
            tools,
            allow,
            check,
            check_network,
            base_commit: base,
            order: "sequential, model by model; load time measured separately".into(),
            ancilo_version: ancilo_core::VERSION.into(),
        }
    }

    fn new_state(
        &self,
        models: Vec<String>,
        blind: bool,
        kind: String,
        kind_estimated: bool,
        config: RunConfig,
    ) -> State {
        let mut models = models;
        if blind {
            shuffle(&mut models, random_u64());
        }
        State {
            repo: None,
            kind,
            kind_estimated,
            config,
            arms: labels(models.len())
                .into_iter()
                .zip(models)
                .map(|(label, model)| Arm { label, model })
                .collect(),
            blind,
            runs: Vec::new(),
            load_ms: HashMap::new(),
            rating: None,
            judgement: None,
            error: None,
            started_at: None,
            finished_at: None,
        }
    }

    /// Starts a comparison in the background; returns its id at once.
    pub async fn compare(&self, input: CompareInput) -> Result<ComparisonReport> {
        if input.task.trim().is_empty() {
            return Err(Error::invalid("task is empty"));
        }
        if !input.cwd.is_absolute() || !input.cwd.is_dir() {
            return Err(Error::invalid(format!(
                "cwd must be an existing absolute directory: {}",
                input.cwd.display()
            )));
        }
        let repeat = input.repeat.unwrap_or(1);
        if repeat == 0 || repeat > MAX_REPEAT {
            return Err(Error::invalid(format!("repeat must be 1–{MAX_REPEAT}")));
        }
        let models = self.resolve_models(&input.models)?;
        if let Some(j) = &input.judge {
            self.inner.gateway.manager().resolve_strict(j)?;
        }
        let top = git(&input.cwd, &["rev-parse", "--show-toplevel"])
            .await
            .map_err(|_| Error::invalid("comparisons need a git repository: every run starts from the same commit in its own worktree"))?;
        let base = git(Path::new(&top), &["rev-parse", "HEAD"])
            .await
            .map_err(|_| {
                Error::invalid("the repository has no commit yet – comparisons start from HEAD")
            })?;
        let (kind, estimated) = match input.kind.as_deref().map(str::to_lowercase) {
            Some(k) if KINDS.contains(&k.as_str()) => (k, false),
            Some(k) => {
                return Err(Error::invalid(format!(
                    "unknown kind '{k}' – use one of: {}",
                    KINDS.join(", ")
                )));
            }
            None => (estimate_kind(&input.task).to_string(), true),
        };
        let config = self.config(
            input.seed.unwrap_or(42),
            input.max_steps.unwrap_or(30),
            input.allow,
            input.check.clone(),
            input.check_network,
            Some(base),
        );
        let state = self.new_state(models, input.blind, kind, estimated, config);
        self.launch(Request::Compare(input), state).await
    }

    /// Runs a suite on several models in the background.
    pub async fn run_suite(&self, input: SuiteRunInput) -> Result<ComparisonReport> {
        let suite = self.suite(&input.suite)?;
        let repeat = input.repeat.unwrap_or(1);
        if repeat == 0 || repeat > MAX_REPEAT {
            return Err(Error::invalid(format!("repeat must be 1–{MAX_REPEAT}")));
        }
        let models = self.resolve_models(&input.models)?;
        let config = self.config(
            input.seed.unwrap_or(42),
            input.max_steps.unwrap_or(30),
            None,
            None,
            false,
            None,
        );
        let state = self.new_state(
            models,
            false,
            format!("suite:{}", suite.name),
            false,
            config,
        );
        self.launch(Request::Suite(input), state).await
    }

    async fn launch(&self, request: Request, state: State) -> Result<ComparisonReport> {
        let id = format!("c-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        self.save(&id, ComparisonStatus::Queued, &request, &state)?;
        let visible: Vec<Value> = state
            .arms
            .iter()
            .map(|a| {
                if state.blind {
                    json!({"label": a.label})
                } else {
                    json!({"label": a.label, "model": a.model})
                }
            })
            .collect();
        self.inner.bus.emit(
            "compare.started",
            Some(&id),
            json!({"arms": visible, "blind": state.blind}),
        );
        let cancel = CancellationToken::new();
        self.inner
            .running
            .lock()
            .unwrap()
            .insert(id.clone(), cancel.clone());
        let me = self.clone();
        let id2 = id.clone();
        tokio::spawn(async move { me.execute(&id2, request, state, cancel).await });
        self.report(&id)
    }

    // ---- running -----------------------------------------------------------

    async fn execute(
        &self,
        id: &str,
        request: Request,
        mut state: State,
        cancel: CancellationToken,
    ) {
        let _slot = tokio::select! {
            s = self.inner.slot.lock() => s,
            _ = cancel.cancelled() => {
                let _ = self.save(id, ComparisonStatus::Cancelled, &request, &state);
                self.inner.running.lock().unwrap().remove(id);
                return;
            }
        };
        state.started_at = Some(Utc::now());
        let _ = self.save(id, ComparisonStatus::Running, &request, &state);
        let result = match &request {
            Request::Compare(input) => {
                self.run_compare(id, input, &request, &mut state, &cancel)
                    .await
            }
            Request::Suite(input) => {
                self.run_suite_inner(id, input, &request, &mut state, &cancel)
                    .await
            }
        };
        state.finished_at = Some(Utc::now());
        let status = match result {
            _ if cancel.is_cancelled() => ComparisonStatus::Cancelled,
            Ok(()) => ComparisonStatus::Done,
            Err(e) => {
                state.error = Some(e.message());
                ComparisonStatus::Failed
            }
        };
        if status == ComparisonStatus::Done
            && let Request::Compare(input) = &request
            && let Some(judge) = &input.judge
        {
            state.judgement = self.judge(judge, &input.task, &state).await;
        }
        let _ = self.save(id, status, &request, &state);
        if !state.blind {
            let _ = self.store_runs(id, &state);
            let _ = self.refresh_recommendations();
        }
        self.inner.running.lock().unwrap().remove(id);
        self.inner.bus.emit(
            "compare.finished",
            Some(id),
            json!({"status": status.as_str(), "runs": state.runs.len()}),
        );
    }

    async fn load_model(&self, model: &str) -> Result<u64> {
        let started = Instant::now();
        self.inner
            .gateway
            .manager()
            .ensure_running(model, LOAD_TIMEOUT)
            .await?;
        Ok(started.elapsed().as_millis() as u64)
    }

    async fn run_compare(
        &self,
        id: &str,
        input: &CompareInput,
        request: &Request,
        state: &mut State,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let top = PathBuf::from(git(&input.cwd, &["rev-parse", "--show-toplevel"]).await?);
        let top = std::fs::canonicalize(&top).unwrap_or(top);
        let cwd = std::fs::canonicalize(&input.cwd).unwrap_or(input.cwd.clone());
        let subdir = cwd
            .strip_prefix(&top)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        state.repo = Some(top.clone());
        let base = state.config.base_commit.clone().unwrap_or_default();
        let repeat = input.repeat.unwrap_or(1);
        let short = &id[2..];
        let mut task = input.task.clone();
        if !input.files.is_empty() {
            task.push_str(&format!(
                "\n\nStart with these files: {}",
                input.files.join(", ")
            ));
        }
        for arm in state.arms.clone() {
            let load_ms = match self.load_model(&arm.model).await {
                Ok(ms) => ms,
                Err(e) => {
                    for i in 0..repeat {
                        state.runs.push(failed_run(
                            &arm.label,
                            &state.kind,
                            None,
                            i,
                            &format!("model could not be loaded: {}", e.message()),
                        ));
                    }
                    let _ = self.save(id, ComparisonStatus::Running, request, state);
                    continue;
                }
            };
            state.load_ms.insert(arm.label.clone(), load_ms);
            for i in 0..repeat {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                let label = arm.label.to_lowercase();
                let branch = format!("ancilo/cmp-{short}/{label}-{i}");
                let wt = self
                    .inner
                    .paths
                    .worktrees_dir()
                    .join(format!("cmp-{short}-{label}-{i}"));
                git(
                    &top,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        &branch,
                        &wt.display().to_string(),
                        &base,
                    ],
                )
                .await?;
                let root = wt.join(&subdir);
                let seed = state.config.seed + u64::from(i);
                let started = Instant::now();
                let outcome = self
                    .agent(
                        id,
                        &root,
                        &arm.model,
                        &task,
                        input.allow.unwrap_or(Access::Edit),
                        state.config.max_steps,
                        seed,
                        state.blind,
                        cancel,
                    )
                    .await;
                let duration_ms = started.elapsed().as_millis() as u64;
                let check = match &input.check {
                    Some(c) if outcome.status != Status::Cancelled => {
                        Some(self.check(&wt, &root, c, input.check_network).await)
                    }
                    _ => None,
                };
                let mut commit = None;
                if !outcome.changed_files.is_empty() {
                    git(&wt, &["add", "-A"]).await.ok();
                    if git(
                        &wt,
                        &[
                            "commit",
                            "-q",
                            "-m",
                            &format!("ancilo compare {id} {label}-{i}"),
                        ],
                    )
                    .await
                    .is_ok()
                    {
                        commit = git(&wt, &["rev-parse", "HEAD"]).await.ok();
                    }
                }
                git(
                    &top,
                    &["worktree", "remove", "--force", &wt.display().to_string()],
                )
                .await
                .ok();
                std::fs::remove_dir_all(ancilo_agent::sandbox::temp_dir(&wt)).ok();
                let success =
                    outcome.status == Status::Done && check.as_ref().is_none_or(|c| c.passed);
                let failure = if success {
                    None
                } else if outcome.status != Status::Done {
                    Some(format!(
                        "{:?}: {}",
                        outcome.status,
                        tail(&outcome.summary, 300)
                    ))
                } else {
                    check
                        .as_ref()
                        .map(|c| format!("check failed: {}", tail(&c.output, 300)))
                };
                let run = run_result(
                    &arm.label,
                    &state.kind,
                    None,
                    i,
                    success,
                    &outcome,
                    duration_ms,
                    check,
                    failure,
                    Some(branch),
                    commit,
                );
                self.inner.bus.emit(
                    "compare.run_finished",
                    Some(id),
                    json!({"label": arm.label, "index": i, "success": success, "duration_ms": duration_ms}),
                );
                state.runs.push(run);
                let _ = self.save(id, ComparisonStatus::Running, request, state);
            }
        }
        Ok(())
    }

    async fn run_suite_inner(
        &self,
        id: &str,
        input: &SuiteRunInput,
        request: &Request,
        state: &mut State,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let suite = self.suite(&input.suite)?;
        let repeat = input.repeat.unwrap_or(1);
        let short = &id[2..];
        for arm in state.arms.clone() {
            let load_ms = match self.load_model(&arm.model).await {
                Ok(ms) => ms,
                Err(e) => {
                    for t in &suite.tasks {
                        for i in 0..repeat {
                            state.runs.push(failed_run(
                                &arm.label,
                                &task_kind(t),
                                Some(t.id.clone()),
                                i,
                                &format!("model could not be loaded: {}", e.message()),
                            ));
                        }
                    }
                    continue;
                }
            };
            state.load_ms.insert(arm.label.clone(), load_ms);
            for t in &suite.tasks {
                for i in 0..repeat {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    let dir = self.inner.paths.worktrees_dir().join(format!(
                        "suite-{short}-{}-{}-{i}",
                        arm.label.to_lowercase(),
                        t.id
                    ));
                    std::fs::remove_dir_all(&dir).ok();
                    std::fs::create_dir_all(&dir)?;
                    ancilo_eval::delegation::prepare(&dir, t)?;
                    let root = std::fs::canonicalize(&dir)?;
                    let access = match t.allow.as_deref() {
                        Some("read") => Access::Read,
                        Some("shell") => Access::Shell,
                        _ => Access::Edit,
                    };
                    let started = Instant::now();
                    let outcome = self
                        .agent(
                            id,
                            &root,
                            &arm.model,
                            &t.task,
                            access,
                            state.config.max_steps,
                            state.config.seed + u64::from(i),
                            false,
                            cancel,
                        )
                        .await;
                    let duration_ms = started.elapsed().as_millis() as u64;
                    let status = match outcome.status {
                        Status::Done => "done",
                        Status::Partial => "partial",
                        Status::Failed => "failed",
                        Status::Cancelled => "cancelled",
                    };
                    let verdict = ancilo_eval::delegation::judge(
                        &root,
                        t,
                        &json!({"status": status, "summary": outcome.summary}),
                    );
                    std::fs::remove_dir_all(&dir).ok();
                    let success = verdict.is_ok();
                    let run = run_result(
                        &arm.label,
                        &task_kind(t),
                        Some(t.id.clone()),
                        i,
                        success,
                        &outcome,
                        duration_ms,
                        None,
                        verdict.err(),
                        None,
                        None,
                    );
                    self.inner.bus.emit(
                        "compare.run_finished",
                        Some(id),
                        json!({"label": arm.label, "task": t.id, "index": i, "success": success}),
                    );
                    state.runs.push(run);
                    let _ = self.save(id, ComparisonStatus::Running, request, state);
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn agent(
        &self,
        id: &str,
        root: &Path,
        model: &str,
        task: &str,
        access: Access,
        max_steps: u32,
        seed: u64,
        blind: bool,
        cancel: &CancellationToken,
    ) -> AgentOutcome {
        let mut ws = match Workspace::new(root, access) {
            Ok(w) => w.with_shell(self.inner.shell.clone()),
            Err(e) => return error_outcome(&format!("cannot open {}: {e}", root.display())),
        };
        // Every arm gets the same tools, including search.
        if let Some(s) = &self.inner.search {
            ws = ws.with_search(s.clone());
        }
        let spec = AgentSpec {
            model: model.to_string(),
            system: ancilo_agent::WORKER_PROMPT.into(),
            task: task.to_string(),
            max_steps,
            priority: Priority::Compare,
            reliability: None,
            temperature: Some(ancilo_agent::DEFAULT_TEMPERATURE),
            seed: Some(seed),
            history: Vec::new(),
            local_only: true,
            think: true,
        };
        let limit = cancel.child_token();
        let timer = {
            let limit = limit.clone();
            tokio::spawn(async move {
                tokio::time::sleep(RUN_TIMEOUT).await;
                limit.cancel();
            })
        };
        // Blind comparisons emit no agent events: they name the model.
        let events = (!blind).then(|| (self.inner.bus.clone(), id.to_string()));
        let mut outcome =
            ancilo_agent::run(&self.inner.gateway, &ws, spec, limit.clone(), events).await;
        timer.abort();
        if limit.is_cancelled() && !cancel.is_cancelled() {
            outcome.status = Status::Partial;
            outcome.summary = "time limit reached".into();
        }
        outcome
    }

    async fn check(
        &self,
        sandbox_root: &Path,
        dir: &Path,
        script: &str,
        network: bool,
    ) -> CheckResult {
        let started = Instant::now();
        let cmd = if self.inner.shell.sandbox {
            ancilo_agent::sandbox::command(
                &ancilo_agent::sandbox::Bounds {
                    root: sandbox_root,
                    hidden: &self.inner.shell.hidden,
                    network,
                },
                script,
            )
        } else {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(script);
            Ok(c)
        };
        let mut cmd = match cmd {
            Ok(c) => c,
            Err(e) => {
                return CheckResult {
                    passed: false,
                    exit_code: None,
                    output: e,
                    duration_ms: 0,
                };
            }
        };
        cmd.current_dir(dir)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());
        let out = tokio::time::timeout(CHECK_TIMEOUT, cmd.output()).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match out {
            Ok(Ok(o)) => CheckResult {
                passed: o.status.success(),
                exit_code: o.status.code(),
                output: tail(
                    &format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                    2000,
                ),
                duration_ms,
            },
            Ok(Err(e)) => CheckResult {
                passed: false,
                exit_code: None,
                output: format!("could not run the check: {e}"),
                duration_ms,
            },
            Err(_) => CheckResult {
                passed: false,
                exit_code: None,
                output: format!("check timed out after {} s", CHECK_TIMEOUT.as_secs()),
                duration_ms,
            },
        }
    }

    /// Asks the judge model to rate each arm's first run (labels only).
    async fn judge(&self, judge: &str, task: &str, state: &State) -> Option<Judgement> {
        let model = self.inner.gateway.manager().resolve_strict(judge).ok()?;
        let mut scores = Vec::new();
        for arm in &state.arms {
            let Some(run) = state.runs.iter().find(|r| r.label == arm.label) else {
                continue;
            };
            let diff = match (&run.commit, &state.config.base_commit, &state.repo) {
                (Some(c), Some(b), Some(repo)) => {
                    git(repo, &["diff", b, c]).await.unwrap_or_default()
                }
                _ => String::new(),
            };
            let diff: String = diff.chars().take(24_000).collect();
            let prompt = format!(
                "Task given to a coding assistant:\n{task}\n\nIts change ({}):\n```diff\n{}\n```\n\nFinal message: {}\n\nRate how well the change completes the task, from 1 (useless) to 10 (perfect). Reply only with JSON: {{\"score\": <1-10>, \"reason\": \"<one sentence>\"}}",
                arm.label,
                if diff.is_empty() {
                    "(no changes)"
                } else {
                    &diff
                },
                tail(&run.summary, 500)
            );
            let req = json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": "You are a strict, fair code reviewer."},
                    {"role": "user", "content": prompt}
                ],
                "temperature": 0,
                "response_format": {"type": "json_object"},
            });
            let opts = CallOpts {
                priority: Priority::Compare,
                reliability: None,
                api: "internal",
                // The judge reads the results' code.
                local_only: true,
                think: true,
            };
            let (score, reason) = match self.inner.gateway.chat(req, opts).await {
                Ok((ChatReply::Complete(v), _)) => parse_judge(
                    v["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or_default(),
                ),
                Ok(_) => (None, "unexpected stream".into()),
                Err(e) => (None, e.message()),
            };
            scores.push(JudgeScore {
                label: arm.label.clone(),
                score,
                reason,
            });
        }
        Some(Judgement {
            judge_model: model,
            note: "model-based – not an objective measurement".into(),
            scores,
        })
    }

    // ---- reading -----------------------------------------------------------

    pub fn report(&self, id: &str) -> Result<ComparisonReport> {
        let (status, created_at, request, state) = self.load(id)?;
        let revealed = !state.blind || state.rating.is_some();
        let (mode, title, repeat, tasks) = match &request {
            Request::Compare(i) => ("compare", i.task.clone(), i.repeat.unwrap_or(1), 1usize),
            Request::Suite(i) => (
                "suite",
                format!("suite {}", i.suite),
                i.repeat.unwrap_or(1),
                self.suite(&i.suite).map(|s| s.tasks.len()).unwrap_or(0),
            ),
        };
        let total = state.arms.len() * tasks * repeat as usize;
        let arms: Vec<ArmSummary> = state
            .arms
            .iter()
            .map(|a| {
                let runs: Vec<&RunResult> =
                    state.runs.iter().filter(|r| r.label == a.label).collect();
                summarize(
                    &a.label,
                    revealed.then(|| a.model.clone()),
                    &runs,
                    state.load_ms.get(&a.label).copied(),
                )
            })
            .collect();
        let (ranking, verdict) = rank(arms);
        let mapping: HashMap<&str, &str> = state
            .arms
            .iter()
            .map(|a| (a.label.as_str(), a.model.as_str()))
            .collect();
        let runs = state
            .runs
            .iter()
            .map(|r| RunResult {
                model: revealed
                    .then(|| mapping.get(r.label.as_str()).map(|m| m.to_string()))
                    .flatten(),
                ..r.clone()
            })
            .collect();
        Ok(ComparisonReport {
            id: id.to_string(),
            mode: mode.into(),
            status,
            title: tail(&title, 200),
            kind: state.kind,
            kind_estimated: state.kind_estimated,
            blind: state.blind,
            revealed,
            created_at,
            finished_at: state.finished_at,
            config: state.config,
            progress: format!("{} of {total} runs", state.runs.len()),
            ranking,
            verdict,
            runs,
            rating: state.rating,
            judgement: state.judgement,
            error: state.error,
        })
    }

    /// Waits until the comparison is final (or `wait`), then reports.
    pub async fn wait_report(&self, id: &str, wait: Option<Duration>) -> Result<ComparisonReport> {
        let deadline = wait.map(|w| Instant::now() + w);
        loop {
            let r = self.report(id)?;
            if r.status.is_final() || deadline.is_none_or(|d| Instant::now() >= d) {
                return Ok(r);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub fn list(&self, limit: usize) -> Result<Vec<ComparisonReport>> {
        let ids: Vec<String> = self.inner.db.with(|c| {
            let mut s =
                c.prepare("SELECT id FROM comparisons ORDER BY created_at DESC LIMIT ?1")?;
            let rows = s.query_map(params![limit as i64], |r| r.get(0))?;
            rows.collect()
        })?;
        ids.iter()
            .map(|id| {
                self.report(id).map(|mut r| {
                    r.runs.clear();
                    r
                })
            })
            .collect()
    }

    pub fn cancel(&self, id: &str) -> Result<ComparisonReport> {
        if let Some(t) = self.inner.running.lock().unwrap().get(id) {
            t.cancel();
        }
        self.report(id)
    }

    /// Stores the user's rating; blind comparisons are revealed afterwards.
    pub fn rate(&self, id: &str, best: &str, note: Option<String>) -> Result<ComparisonReport> {
        let (status, _, request, mut state) = self.load(id)?;
        if status != ComparisonStatus::Done {
            return Err(Error::Conflict(format!(
                "comparison is {} – rate it when it is done",
                status.as_str()
            )));
        }
        if state.rating.is_some() {
            return Err(Error::Conflict("this comparison is already rated".into()));
        }
        let best = best.trim().to_uppercase();
        if best != "TIE" && !state.arms.iter().any(|a| a.label == best) {
            return Err(Error::invalid(format!(
                "best must be one of {} or `tie`",
                state
                    .arms
                    .iter()
                    .map(|a| a.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        state.rating = Some(Rating {
            best: if best == "TIE" { "tie".into() } else { best },
            note,
            rated_at: Utc::now(),
        });
        self.save(id, status, &request, &state)?;
        if state.blind {
            self.store_runs(id, &state)?;
            self.refresh_recommendations()?;
        }
        self.inner.bus.emit(
            "compare.rated",
            Some(id),
            json!({"best": state.rating.as_ref().map(|r| r.best.clone())}),
        );
        self.report(id)
    }

    // ---- suites ------------------------------------------------------------

    /// A built-in suite, a saved suite by name, or a YAML file.
    pub fn suite(&self, name: &str) -> Result<Suite> {
        if let Some(s) = ancilo_eval::delegation::builtin(name) {
            return Ok(s);
        }
        let saved = self.suites_dir().join(format!("{name}.yaml"));
        let path = if saved.exists() {
            saved
        } else {
            PathBuf::from(name)
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|_| Error::not_found(format!("no suite '{name}'")))?;
        parse_suite(&text)
    }

    pub fn save_suite(&self, name: &str, content: &str) -> Result<SuiteInfo> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::invalid("suite names use letters, digits, - and _"));
        }
        if ancilo_eval::delegation::builtin(name).is_some() {
            return Err(Error::Conflict(format!("'{name}' is a built-in suite")));
        }
        let suite = parse_suite(content)?;
        std::fs::create_dir_all(self.suites_dir())?;
        std::fs::write(self.suites_dir().join(format!("{name}.yaml")), content)?;
        self.inner.bus.emit(
            "suite.saved",
            Some(name),
            json!({"tasks": suite.tasks.len()}),
        );
        Ok(SuiteInfo {
            name: name.to_string(),
            tasks: suite.tasks.len(),
            builtin: false,
        })
    }

    pub fn suites(&self) -> Result<Vec<SuiteInfo>> {
        let mut out = vec![SuiteInfo {
            name: "delegation".into(),
            tasks: ancilo_eval::delegation::builtin("delegation").map_or(0, |s| s.tasks.len()),
            builtin: true,
        }];
        if let Ok(rd) = std::fs::read_dir(self.suites_dir()) {
            let mut saved: Vec<SuiteInfo> = rd
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let name = p.file_stem()?.to_string_lossy().into_owned();
                    let tasks = parse_suite(&std::fs::read_to_string(&p).ok()?)
                        .ok()?
                        .tasks
                        .len();
                    (p.extension()? == "yaml").then_some(SuiteInfo {
                        name,
                        tasks,
                        builtin: false,
                    })
                })
                .collect();
            saved.sort_by(|a, b| a.name.cmp(&b.name));
            out.extend(saved);
        }
        Ok(out)
    }

    // ---- leaderboard & recommendations ---------------------------------------

    pub fn leaderboard(&self, kind: Option<&str>) -> Result<Leaderboard> {
        type Row = (String, String, bool, String);
        let rows: Vec<Row> = self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT model_id, kind, success, data FROM compare_runs")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            rows.collect()
        })?;
        let mut groups: HashMap<(String, String), Vec<(bool, RunResult)>> = HashMap::new();
        for (model, k, success, data) in rows {
            if kind.is_some_and(|want| want != k) {
                continue;
            }
            if let Ok(run) = serde_json::from_str::<RunResult>(&data) {
                groups.entry((model, k)).or_default().push((success, run));
            }
        }
        let mut entries: Vec<LeaderboardEntry> = groups
            .into_iter()
            .map(|((model, kind), runs)| {
                let durations: Vec<u64> = runs.iter().map(|(_, r)| r.duration_ms).collect();
                let tps: Vec<f64> = runs.iter().filter_map(|(_, r)| r.tokens_per_s).collect();
                LeaderboardEntry {
                    model,
                    kind,
                    success: Rate::new(
                        runs.iter().filter(|(s, _)| *s).count() as u64,
                        runs.len() as u64,
                    ),
                    duration_p50_ms: stats::percentile(&durations, 50.0),
                    tokens_per_s: median_f(&tps),
                }
            })
            .collect();
        entries.sort_by(|a, b| {
            a.kind
                .cmp(&b.kind)
                .then(b.success.low.total_cmp(&a.success.low))
                .then(b.success.rate.total_cmp(&a.success.rate))
        });
        let conclusions = conclude(&entries);
        let mut md = String::from(
            "# Model leaderboard (this machine)\n\n| Kind | Model | Success | 95 % interval | Runs | Duration p50 | Tokens/s |\n|---|---|---:|---|---:|---:|---:|\n",
        );
        for e in &entries {
            md.push_str(&format!(
                "| {} | {} | {:.0} % | {:.0}–{:.0} % | {} | {} | {} |\n",
                e.kind,
                e.model,
                e.success.rate * 100.0,
                e.success.low * 100.0,
                e.success.high * 100.0,
                e.success.n,
                e.duration_p50_ms
                    .map_or("–".into(), |d| format!("{:.1} s", d as f64 / 1000.0)),
                e.tokens_per_s.map_or("–".into(), |t| format!("{t:.0}")),
            ));
        }
        let picks: Vec<(String, String)> = self.inner.db.with(|c| {
            let mut s = c.prepare("SELECT chosen, over FROM variant_choices")?;
            let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        let mut tally: HashMap<String, (u64, u64)> = HashMap::new();
        for (chosen, over) in picks {
            tally.entry(chosen).or_default().0 += 1;
            tally.entry(over).or_default().1 += 1;
        }
        let mut choices: Vec<ChoiceEntry> = tally
            .into_iter()
            .map(|(model, (chosen, passed_over))| ChoiceEntry {
                model,
                chosen,
                passed_over,
            })
            .collect();
        choices.sort_by(|a, b| {
            b.chosen
                .cmp(&a.chosen)
                .then(a.passed_over.cmp(&b.passed_over))
                .then(a.model.cmp(&b.model))
        });
        if !choices.is_empty() {
            md.push_str("\n## Your choices in coding sessions (subjective)\n\n| Model | Taken | Passed over |\n|---|---:|---:|\n");
            for c in &choices {
                md.push_str(&format!(
                    "| {} | {} | {} |\n",
                    c.model, c.chosen, c.passed_over
                ));
            }
        }
        if !conclusions.is_empty() {
            md.push('\n');
            for c in &conclusions {
                md.push_str(&format!("- **{}:** {}\n", c.kind, c.note));
            }
        }
        Ok(Leaderboard {
            entries,
            conclusions,
            choices,
            markdown: md,
        })
    }

    /// Derives recommendations from results with enough data. Never applies them.
    pub fn refresh_recommendations(&self) -> Result<Vec<Recommendation>> {
        let _one = self.inner.refreshing.lock().unwrap();
        let manager = self.inner.gateway.manager();
        let board = self.leaderboard(None)?;
        let mut candidates: Vec<(Action, String)> = Vec::new();
        for kind in KINDS {
            let here: Vec<&LeaderboardEntry> =
                board.entries.iter().filter(|e| e.kind == *kind).collect();
            let Ok(current) = manager.route(&RouteRequest {
                kind: Some(kind),
                role: "delegation",
                ..Default::default()
            }) else {
                continue;
            };
            let Some(cur) = here.iter().find(|e| e.model == current.model) else {
                continue;
            };
            let best = here
                .iter()
                .filter(|e| e.model != current.model)
                .filter(|e| {
                    stats::compare_rates(&cur.success, &e.success, MIN_SAMPLES_RECOMMENDATION, 0.05)
                        .0
                        == Difference::SecondBetter
                })
                .max_by(|a, b| a.success.low.total_cmp(&b.success.low));
            if let Some(b) = best {
                candidates.push((
                    Action::SetRoute {
                        kind: kind.to_string(),
                        model: b.model.clone(),
                    },
                    format!(
                        "For `{kind}` tasks, {} succeeded in {:.0} % ({:.0}–{:.0} %, n = {}) vs. {:.0} % ({:.0}–{:.0} %, n = {}) for the current {}.",
                        b.model,
                        b.success.rate * 100.0, b.success.low * 100.0, b.success.high * 100.0, b.success.n,
                        cur.success.rate * 100.0, cur.success.low * 100.0, cur.success.high * 100.0, cur.success.n,
                        cur.model
                    ),
                ));
            }
        }
        for test in manager.ab_list()? {
            let Ok(report) = manager.ab_report(&test.id) else {
                continue;
            };
            let holder = manager.route(&RouteRequest {
                model: Some(&test.role),
                role: &test.role,
                ..Default::default()
            });
            if report.verdict == Difference::SecondBetter
                && holder.is_ok_and(|h| h.model != test.model_b)
            {
                candidates.push((
                    Action::AssignRole {
                        role: test.role.clone(),
                        model: test.model_b.clone(),
                    },
                    format!("A/B test {}: {}", test.id, report.summary),
                ));
            }
        }
        let mut created = Vec::new();
        for (action, rationale) in candidates {
            let key = serde_json::to_string(&action)?;
            let exists: bool = self.inner.db.with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM recommendations WHERE json_extract(data, '$.action') = json(?1) AND status IN ('open', 'dismissed')",
                    params![key],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
            })?;
            if exists {
                continue;
            }
            let rec = Recommendation {
                id: format!("r-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
                created_at: Utc::now(),
                status: "open".into(),
                action,
                rationale,
            };
            self.save_recommendation(&rec)?;
            self.inner
                .bus
                .emit("recommendation.created", Some(&rec.id), json!(rec));
            created.push(rec);
        }
        Ok(created)
    }

    fn save_recommendation(&self, r: &Recommendation) -> Result<()> {
        let data = serde_json::to_string(r)?;
        self.inner.db.with(|c| {
            c.execute(
                "INSERT INTO recommendations(id, created_at, status, data) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET status = excluded.status, data = excluded.data",
                params![r.id, r.created_at.to_rfc3339(), r.status, data],
            )
            .map(|_| ())
        })
    }

    pub fn recommendations(&self, all: bool) -> Result<Vec<Recommendation>> {
        self.refresh_recommendations()?;
        let rows: Vec<String> = self.inner.db.with(|c| {
            let mut s = c.prepare(if all {
                "SELECT data FROM recommendations ORDER BY created_at DESC"
            } else {
                "SELECT data FROM recommendations WHERE status = 'open' ORDER BY created_at DESC"
            })?;
            let rows = s.query_map([], |r| r.get(0))?;
            rows.collect()
        })?;
        Ok(rows
            .iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect())
    }

    fn recommendation(&self, id: &str) -> Result<Recommendation> {
        let data: Option<String> = self.inner.db.with(|c| {
            c.query_row(
                "SELECT data FROM recommendations WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
        })?;
        data.and_then(|d| serde_json::from_str(&d).ok())
            .ok_or_else(|| Error::not_found(format!("no recommendation '{id}'")))
    }

    /// Carries out exactly the recommended change.
    pub fn apply_recommendation(&self, id: &str) -> Result<Recommendation> {
        let mut r = self.recommendation(id)?;
        if r.status != "open" {
            return Err(Error::Conflict(format!("recommendation is {}", r.status)));
        }
        let manager = self.inner.gateway.manager();
        match &r.action {
            Action::SetRoute { kind, model } => {
                manager.set_route(kind, model)?;
            }
            Action::AssignRole { role, model } => {
                manager.assign_role(role, model)?;
            }
        }
        r.status = "applied".into();
        self.save_recommendation(&r)?;
        self.inner
            .bus
            .emit("recommendation.applied", Some(id), json!(r.action));
        Ok(r)
    }

    pub fn dismiss_recommendation(&self, id: &str) -> Result<Recommendation> {
        let mut r = self.recommendation(id)?;
        r.status = "dismissed".into();
        self.save_recommendation(&r)?;
        Ok(r)
    }

    // ---- lifecycle -----------------------------------------------------------

    /// Daemon shutdown: stops running comparisons.
    pub async fn shutdown(&self) {
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

    /// After a restart: comparisons that were interrupted are marked failed –
    /// a measurement that was interrupted is not comparable anymore.
    pub fn restore(&self) -> Result<()> {
        self.inner.db.with(|c| {
            c.execute(
                "UPDATE comparisons SET status = 'failed', state = json_set(state, '$.error', 'interrupted by a restart of Ancilo') WHERE status IN ('queued', 'running')",
                [],
            )
            .map(|_| ())
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SuiteInfo {
    pub name: String,
    pub tasks: usize,
    pub builtin: bool,
}

fn parse_suite(text: &str) -> Result<Suite> {
    let v = ancilo_eval::yaml_value(text)?;
    let suite: Suite =
        serde_json::from_value(v).map_err(|e| Error::invalid(format!("suite: {e}")))?;
    if suite.tasks.is_empty() {
        return Err(Error::invalid("the suite has no tasks"));
    }
    Ok(suite)
}

fn task_kind(t: &SuiteTask) -> String {
    t.kind
        .clone()
        .filter(|k| KINDS.contains(&k.as_str()))
        .unwrap_or_else(|| estimate_kind(&t.task).to_string())
}

fn error_outcome(msg: &str) -> AgentOutcome {
    AgentOutcome {
        status: Status::Failed,
        summary: msg.to_string(),
        steps: 0,
        tool_calls: 0,
        changed_files: Vec::new(),
        diff: String::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        interventions: 0,
        generation_ms: 0,
        load_ms: 0,
        messages: Vec::new(),
    }
}

fn failed_run(
    label: &str,
    kind: &str,
    suite_task: Option<String>,
    index: u32,
    why: &str,
) -> RunResult {
    run_result(
        label,
        kind,
        suite_task,
        index,
        false,
        &error_outcome(why),
        0,
        None,
        Some(why.to_string()),
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_result(
    label: &str,
    kind: &str,
    suite_task: Option<String>,
    index: u32,
    success: bool,
    o: &AgentOutcome,
    duration_ms: u64,
    check: Option<CheckResult>,
    failure: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
) -> RunResult {
    RunResult {
        label: label.to_string(),
        model: None,
        suite_task,
        kind: kind.to_string(),
        index,
        success,
        status: format!("{:?}", o.status).to_lowercase(),
        check,
        failure,
        duration_ms,
        load_ms: o.load_ms,
        generation_ms: o.generation_ms,
        prompt_tokens: o.prompt_tokens,
        completion_tokens: o.completion_tokens,
        tokens_per_s: (o.generation_ms > 0)
            .then(|| o.completion_tokens as f64 / (o.generation_ms as f64 / 1000.0)),
        steps: o.steps,
        tool_calls: o.tool_calls,
        interventions: o.interventions,
        files_changed: o.changed_files.len(),
        lines_added: o.changed_files.iter().map(|f| f.added).sum(),
        lines_removed: o.changed_files.iter().map(|f| f.removed).sum(),
        branch,
        commit,
        summary: tail(&o.summary, 1000),
    }
}

/// Per task kind: the best model if it is significantly ahead of the
/// runner-up, otherwise an honest "not yet" – with the numbers.
fn conclude(entries: &[LeaderboardEntry]) -> Vec<Conclusion> {
    let mut kinds: Vec<&str> = entries.iter().map(|e| e.kind.as_str()).collect();
    kinds.dedup();
    let pct = |r: &stats::Rate| format!("{:.0} % of {}", r.rate * 100.0, r.n);
    kinds
        .into_iter()
        .map(|kind| {
            // Entries are sorted by the lower bound of the success interval.
            let here: Vec<&LeaderboardEntry> = entries.iter().filter(|e| e.kind == kind).collect();
            let (best, note) = match here.as_slice() {
                [only] => (
                    None,
                    format!("only {} was measured ({}) – nothing to compare yet", only.model, pct(&only.success)),
                ),
                [a, b, ..] => match stats::compare_rates(&b.success, &a.success, MIN_SAMPLES_RECOMMENDATION, 0.05).0 {
                    Difference::SecondBetter => (
                        Some(a.model.clone()),
                        format!("{} is reliably best ({} vs. {} for {})", a.model, pct(&a.success), pct(&b.success), b.model),
                    ),
                    Difference::InsufficientData => (
                        None,
                        format!(
                            "not enough data for a reliable ranking yet ({}: {}, {}: {}; at least {MIN_SAMPLES_RECOMMENDATION} runs each needed)",
                            a.model, pct(&a.success), b.model, pct(&b.success)
                        ),
                    ),
                    _ => (
                        None,
                        format!(
                            "no reliable difference yet between {} ({}) and {} ({})",
                            a.model, pct(&a.success), b.model, pct(&b.success)
                        ),
                    ),
                },
                [] => (None, String::new()),
            };
            Conclusion { kind: kind.to_string(), best, note }
        })
        .filter(|c| !c.note.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(model: &str, kind: &str, ok: u64, n: u64) -> LeaderboardEntry {
        LeaderboardEntry {
            model: model.into(),
            kind: kind.into(),
            success: stats::Rate::new(ok, n),
            duration_p50_ms: None,
            tokens_per_s: None,
        }
    }

    #[test]
    fn the_leaderboard_names_a_best_model_only_when_it_is_reliably_ahead() {
        let c = conclude(&[entry("small", "tests", 8, 8), entry("big", "tests", 7, 8)]);
        assert_eq!(c[0].best, None);
        assert!(c[0].note.starts_with("not enough data"), "{}", c[0].note);
        let c = conclude(&[entry("a", "fix", 38, 40), entry("b", "fix", 20, 40)]);
        assert_eq!(c[0].best.as_deref(), Some("a"));
        let c = conclude(&[entry("a", "docs", 30, 40), entry("b", "docs", 28, 40)]);
        assert_eq!(c[0].best, None);
        assert!(
            c[0].note.starts_with("no reliable difference"),
            "{}",
            c[0].note
        );
        let c = conclude(&[entry("a", "refactor", 3, 4)]);
        assert!(c[0].note.contains("nothing to compare"));
    }

    // covers: M4-AC-10
    #[test]
    fn blind_labels_are_shuffled() {
        let orders: std::collections::HashSet<Vec<u32>> = (0..40u64)
            .map(|seed| {
                let mut v = vec![0, 1, 2];
                shuffle(&mut v, seed.wrapping_mul(0x1234_5678_9abc));
                v
            })
            .collect();
        // All 6 permutations of 3 items show up across seeds.
        assert_eq!(orders.len(), 6, "{orders:?}");
        assert_eq!(labels(3), ["A", "B", "C"]);
    }

    #[test]
    fn judge_answers_are_parsed_tolerantly() {
        assert_eq!(
            parse_judge(r#"{"score": 7, "reason": "ok"}"#),
            (Some(7.0), "ok".into())
        );
        assert_eq!(parse_judge("Sure! {\"score\": 12}").0, Some(10.0));
        assert_eq!(parse_judge("I'd give it 4/10.").0, Some(4.0));
        assert_eq!(parse_judge("no idea").0, None);
    }

    #[test]
    fn ranking_prefers_success_then_confidence_then_speed() {
        let run = |label: &str, success: bool, ms: u64| RunResult {
            label: label.into(),
            success,
            duration_ms: ms,
            ..run_result(
                label,
                "tests",
                None,
                0,
                success,
                &error_outcome(""),
                ms,
                None,
                None,
                None,
                None,
            )
        };
        let a: Vec<RunResult> = (0..6).map(|_| run("A", true, 900)).collect();
        let b: Vec<RunResult> = (0..6).map(|_| run("B", false, 100)).collect();
        let arms = vec![
            summarize("B", None, &b.iter().collect::<Vec<_>>(), None),
            summarize("A", None, &a.iter().collect::<Vec<_>>(), None),
        ];
        let (ranked, verdict) = rank(arms);
        assert_eq!(ranked[0].label, "A");
        assert!(verdict.contains("significantly"), "{verdict}");
    }
}
