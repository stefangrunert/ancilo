//! Eval-Set II: delegated tasks against fixture repositories.
//!
//! Each task gets a fresh git repository with its files, is delegated through
//! Ancilo's `delegate` operation (the real path Claude/Codex use), and is
//! judged by checks on the result: file contents, commands that must succeed,
//! files that must stay untouched.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use ancilo_core::{Error, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub name: String,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub kind: Option<String>,
    /// read | edit (default) | shell
    #[serde(default)]
    pub allow: Option<String>,
    pub files: BTreeMap<String, String>,
    /// Unrelated modules around the task's files – a repository where the
    /// right place has to be found first.
    #[serde(default)]
    pub filler: Option<Filler>,
    pub task: String,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Filler {
    /// Number of generated Python modules.
    pub modules: u32,
    #[serde(default)]
    pub seed: u64,
}

/// Areas and names for generated modules – plausible, deterministic, and
/// free of the words the tasks describe.
const AREAS: &[&str] = &[
    "billing", "display", "accounts", "catalog", "net", "sales", "storage", "reports", "admin",
    "tools",
];
const NOUNS: &[&str] = &[
    "ledger", "record", "column", "bucket", "vector", "matrix", "segment", "snapshot", "manifest",
    "journal", "cursor", "buffer", "digest", "profile", "tag", "batch", "window", "sample",
    "series", "layer", "index", "token", "entry", "slot",
];
const VERBS: &[&str] = &[
    "merge",
    "flatten",
    "normalize",
    "checksum",
    "compact",
    "split",
    "rotate",
    "tally",
    "scale",
    "trim",
    "group",
    "rank",
    "sort",
    "hash",
    "encode",
    "decode",
    "align",
    "clip",
    "score",
    "fold",
];

/// Writes `modules` generated modules into `dir` (Python packages).
fn write_filler(dir: &Path, f: &Filler) -> Result<()> {
    let mut state = f
        .seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut next = |n: usize| {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) as usize) % n
    };
    std::fs::create_dir_all(dir.join("lib"))?;
    std::fs::write(dir.join("lib/__init__.py"), "")?;
    for area in AREAS {
        std::fs::create_dir_all(dir.join("lib").join(area))?;
        std::fs::write(dir.join("lib").join(area).join("__init__.py"), "")?;
    }
    for i in 0..f.modules {
        let area = AREAS[next(AREAS.len())];
        let noun = NOUNS[next(NOUNS.len())];
        let mut body = format!("\"\"\"Helpers for {noun} data.\"\"\"\n\n");
        for _ in 0..3 {
            let (verb, other) = (VERBS[next(VERBS.len())], NOUNS[next(NOUNS.len())]);
            let k = 2 + next(7);
            body.push_str(&format!(
                "def {verb}_{other}(items, factor={k}):\n    \"\"\"{verb} each {other} of a {noun}.\"\"\"\n    out = []\n    for x in items:\n        out.append(x * factor if isinstance(x, (int, float)) else x)\n    return out\n\n\n"
            ));
        }
        let path = dir.join("lib").join(area).join(format!("{noun}_{i}.py"));
        std::fs::write(path, body.trim_end().to_string() + "\n")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Check {
    pub file_contains: Option<FileText>,
    pub file_not_contains: Option<FileText>,
    /// Shell command run in the repository; must exit with 0.
    pub command: Option<String>,
    /// The task summary must contain this text (case-insensitive).
    pub summary_contains: Option<String>,
    /// These files must be byte-identical afterwards.
    pub unchanged: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileText {
    pub path: String,
    pub text: String,
}

pub fn builtin(name: &str) -> Option<Suite> {
    match name {
        "delegation" => serde_yaml::from_str(include_str!("../../../evals/delegation.yaml")).ok(),
        "delegation-search" => {
            serde_yaml::from_str(include_str!("../../../evals/delegation-search.yaml")).ok()
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub passed: bool,
    pub status: String,
    pub steps: Option<u64>,
    /// Prompt + completion tokens of the worker.
    #[serde(default)]
    pub tokens: Option<u64>,
    pub duration_ms: u64,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskResult {
    pub id: String,
    pub kind: Option<String>,
    pub runs: Vec<Run>,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub suite: String,
    pub model: String,
    pub started_at: DateTime<Utc>,
    pub repeats: u32,
    pub tasks: Vec<TaskResult>,
    pub success_rate: f64,
    /// Whether the worker could use the project search.
    #[serde(default)]
    pub search: bool,
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "eval")
        .env("GIT_AUTHOR_EMAIL", "eval@localhost")
        .env("GIT_COMMITTER_NAME", "eval")
        .env("GIT_COMMITTER_EMAIL", "eval@localhost")
        .output()
        .map_err(|e| Error::unavailable(format!("git: {e}")))?
        .status
        .success();
    if ok {
        Ok(())
    } else {
        Err(Error::internal(format!("git {args:?} failed")))
    }
}

/// Writes the task's files into `dir` and commits them in a fresh repository.
pub fn prepare(dir: &Path, task: &Task) -> Result<()> {
    if let Some(f) = &task.filler {
        write_filler(dir, f)?;
    }
    for (path, content) in &task.files {
        let p = dir.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, content)?;
    }
    git(dir, &["init", "-q", "-b", "main"])?;
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-q", "-m", "fixture"])
}

/// Applies the checks; `Err` explains the first failed one.
pub fn judge(dir: &Path, task: &Task, result: &Value) -> std::result::Result<(), String> {
    if !matches!(result["status"].as_str(), Some("done")) {
        return Err(format!(
            "status {}: {}",
            result["status"],
            result["summary"].as_str().unwrap_or("")
        ));
    }
    check(
        dir,
        &task.files,
        &task.checks,
        result["summary"].as_str().unwrap_or_default(),
    )
}

/// Checks a result in `dir` (`files`: the fixture, for `unchanged`).
pub fn check(
    dir: &Path,
    files: &BTreeMap<String, String>,
    checks: &[Check],
    summary: &str,
) -> std::result::Result<(), String> {
    for c in checks {
        if let Some(ft) = &c.file_contains {
            let text = std::fs::read_to_string(dir.join(&ft.path))
                .map_err(|_| format!("{} missing", ft.path))?;
            if !text.contains(&ft.text) {
                return Err(format!("{} lacks {:?}", ft.path, ft.text));
            }
        }
        if let Some(ft) = &c.file_not_contains {
            let text = std::fs::read_to_string(dir.join(&ft.path)).unwrap_or_default();
            if text.contains(&ft.text) {
                return Err(format!("{} still contains {:?}", ft.path, ft.text));
            }
        }
        if let Some(cmd) = &c.command {
            let out = Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(dir)
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                let err: String = String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(200)
                    .collect();
                return Err(format!("`{cmd}` failed: {err}"));
            }
        }
        if let Some(s) = &c.summary_contains
            && !summary.to_lowercase().contains(&s.to_lowercase())
        {
            return Err(format!("summary lacks {s:?}"));
        }
        for path in c.unchanged.iter().flatten() {
            let now = std::fs::read_to_string(dir.join(path)).unwrap_or_default();
            if Some(&now) != files.get(path) {
                return Err(format!("{path} was changed"));
            }
        }
    }
    Ok(())
}

/// Runs the suite through the `delegate` operation of a running Ancilo.
pub async fn run(
    suite: &Suite,
    base_url: &str,
    token: &str,
    model: &str,
    repeats: u32,
    search: bool,
) -> Result<Report> {
    let http = reqwest::Client::new();
    let started_at = Utc::now();
    let mut tasks = Vec::new();
    let (mut passed_all, mut total) = (0u32, 0u32);
    for task in &suite.tasks {
        let mut runs = Vec::new();
        for _ in 0..repeats.max(1) {
            let dir = tempfile_dir()?;
            prepare(&dir, task)?;
            let begin = Instant::now();
            let resp = http
                .post(format!("{base_url}/api/v1/ops/delegate"))
                .bearer_auth(token)
                .header("x-ancilo-confirm", "true")
                .json(&json!({
                    "task": task.task, "cwd": std::fs::canonicalize(&dir)?, "model": model,
                    "kind": task.kind, "allow": task.allow.clone().unwrap_or_else(|| "edit".into()),
                    "search": search,
                }))
                .send()
                .await
                .map_err(|e| Error::unavailable(e.to_string()))?;
            let result: Value = resp
                .json()
                .await
                .map_err(|e| Error::unavailable(e.to_string()))?;
            let verdict = judge(&dir, task, &result);
            runs.push(Run {
                passed: verdict.is_ok(),
                status: result["status"].as_str().unwrap_or("error").to_string(),
                steps: result["steps"].as_u64(),
                tokens: result["tokens"].as_u64(),
                duration_ms: begin.elapsed().as_millis() as u64,
                failure: verdict.err(),
            });
            std::fs::remove_dir_all(&dir).ok();
        }
        let ok = runs.iter().filter(|r| r.passed).count();
        passed_all += ok as u32;
        total += runs.len() as u32;
        tasks.push(TaskResult {
            id: task.id.clone(),
            kind: task.kind.clone(),
            success_rate: ok as f64 / runs.len() as f64,
            runs,
        });
    }
    Ok(Report {
        suite: suite.name.clone(),
        model: model.to_string(),
        started_at,
        repeats: repeats.max(1),
        tasks,
        search,
        success_rate: if total == 0 {
            0.0
        } else {
            f64::from(passed_all) / f64::from(total)
        },
    })
}

pub(crate) fn tempfile_dir() -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("ancilo-eval-{}", uuid_like()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_suite_hides_each_bug_among_unrelated_modules() {
        let s = builtin("delegation-search").unwrap();
        assert_eq!(s.tasks.len(), 8);
        for task in &s.tasks {
            let dir = tempfile_dir().unwrap();
            prepare(&dir, task).unwrap();
            let files = std::process::Command::new("git")
                .args(["ls-files"])
                .current_dir(&dir)
                .output()
                .unwrap();
            let n = String::from_utf8_lossy(&files.stdout).lines().count();
            assert!(n > 80, "{}: {n} files", task.id);
            // The task's words do not simply lead to the file.
            let target = task.files.keys().next().unwrap();
            let words: Vec<&str> = task
                .task
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 5)
                .collect();
            let text = std::fs::read_to_string(dir.join(target))
                .unwrap()
                .to_lowercase();
            let hits = words
                .iter()
                .filter(|w| text.contains(&w.to_lowercase()))
                .count();
            assert!(
                hits <= 1,
                "{}: {hits} of the task's words appear in {target}",
                task.id
            );
            // The bug is real: the check fails before the fix.
            let verdict = check(&dir, &task.files, &task.checks, "");
            assert!(verdict.is_err(), "{} passes unfixed", task.id);
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn suite_parses_with_fifteen_tasks() {
        let s = builtin("delegation").unwrap();
        assert_eq!(s.tasks.len(), 15);
        assert!(
            s.tasks
                .iter()
                .all(|t| !t.checks.is_empty() && !t.files.is_empty())
        );
    }

    #[test]
    fn checks_catch_wrong_results_and_accept_right_ones() {
        let s = builtin("delegation").unwrap();
        let task = s.tasks.iter().find(|t| t.id == "bump-version").unwrap();
        let dir = tempfile_dir().unwrap();
        prepare(&dir, task).unwrap();
        let done = json!({"status": "done", "summary": "bumped"});
        assert!(judge(&dir, task, &done).unwrap_err().contains("lacks"));
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        assert!(judge(&dir, task, &done).is_ok());
        assert!(judge(&dir, task, &json!({"status": "failed", "summary": "no"})).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
