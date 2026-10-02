//! Eval for the coding agent in the app (M8): reference tasks through the
//! same operations the app uses – `create_session`, `send_message` (one or
//! more turns), `apply_changes` – judged on the project afterwards.
//!
//! Beyond the result, every run checks the promise of the coding view: the
//! project stays untouched until the changes are applied.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use ancilo_core::{Error, Result};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::delegation::{Check, Report, Run, TaskResult, check, git, tempfile_dir};

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
    /// What the agent may do without asking: read | edit | shell (default).
    #[serde(default)]
    pub permission: Option<String>,
    pub files: BTreeMap<String, String>,
    /// The user's messages, one per turn; later turns build on earlier ones.
    pub turns: Vec<String>,
    /// Judged on the project after the changes were applied; the summary is
    /// the agent's last answer.
    pub checks: Vec<Check>,
}

pub fn builtin(name: &str) -> Option<Suite> {
    match name {
        "coding" => serde_yaml::from_str(include_str!("../../../evals/coding.yaml")).ok(),
        _ => None,
    }
}

/// Limit for one turn.
const TURN_LIMIT: Duration = Duration::from_secs(600);

struct Api<'a> {
    http: reqwest::Client,
    url: &'a str,
    token: &'a str,
}

impl Api<'_> {
    async fn op(&self, name: &str, input: Value) -> std::result::Result<Value, String> {
        let r = self
            .http
            .post(format!("{}/api/v1/ops/{name}", self.url))
            .bearer_auth(self.token)
            .header("x-ancilo-confirm", "true")
            .json(&input)
            .send()
            .await
            .map_err(|e| format!("{name}: {e}"))?;
        let ok = r.status().is_success();
        let v: Value = r.json().await.map_err(|e| format!("{name}: {e}"))?;
        if ok {
            Ok(v)
        } else {
            Err(format!(
                "{name}: {}",
                v["error"]["message"].as_str().unwrap_or("failed")
            ))
        }
    }
}

fn git_status(dir: &Path) -> String {
    std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(dir)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// One run of a task; returns the run and the session's turns count.
async fn run_task(
    api: &Api<'_>,
    dir: &Path,
    task: &Task,
    model: &str,
) -> std::result::Result<u64, String> {
    let s = api
        .op(
            "create_session",
            json!({"cwd": dir, "model": model, "permission": task.permission.as_deref().unwrap_or("shell"), "title": task.id}),
        )
        .await?;
    let id = s["id"].as_str().ok_or("no session id")?.to_string();
    let result = async {
        let mut last = Value::Null;
        for text in &task.turns {
            api.op("send_message", json!({"session": id, "text": text}))
                .await?;
            let begin = Instant::now();
            loop {
                last = api.op("get_session", json!({"session": id})).await?;
                if last["status"] != "running" {
                    break;
                }
                if let Some(a) = last["approvals"].as_array().and_then(|a| a.first()) {
                    return Err(format!(
                        "asked for approval: {} {}",
                        a["tool"], a["arguments"]
                    ));
                }
                if begin.elapsed() > TURN_LIMIT {
                    let _ = api.op("cancel_turn", json!({"session": id})).await;
                    return Err("turn time limit reached".into());
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        // The project is untouched until the changes are applied.
        let status = git_status(dir);
        if !status.is_empty() {
            return Err(format!("project changed before applying: {status}"));
        }
        // Always through the operation: it reports what it could not apply.
        api.op("apply_changes", json!({"session": id})).await?;
        let summary = last["messages"]
            .as_array()
            .and_then(|m| {
                m.iter()
                    .rev()
                    .find(|m| m["role"] == "assistant" && m["text"] != "")
            })
            .and_then(|m| m["text"].as_str())
            .unwrap_or_default()
            .to_string();
        check(dir, &task.files, &task.checks, &summary)?;
        Ok(last["turns"].as_u64().unwrap_or(0))
    }
    .await;
    let _ = api.op("delete_session", json!({"session": id})).await;
    result
}

/// Runs the suite through the session operations of a running Ancilo.
pub async fn run(
    suite: &Suite,
    base_url: &str,
    token: &str,
    model: &str,
    repeats: u32,
) -> Result<Report> {
    let api = Api {
        http: reqwest::Client::new(),
        url: base_url,
        token,
    };
    let started_at = Utc::now();
    let mut tasks = Vec::new();
    let (mut passed_all, mut total) = (0u32, 0u32);
    for task in &suite.tasks {
        let mut runs = Vec::new();
        for _ in 0..repeats.max(1) {
            let dir = tempfile_dir()?;
            let dir = {
                for (path, content) in &task.files {
                    let p = dir.join(path);
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(p, content)?;
                }
                git(&dir, &["init", "-q", "-b", "main"])?;
                git(&dir, &["add", "-A"])?;
                git(&dir, &["commit", "-q", "-m", "fixture"])?;
                std::fs::canonicalize(&dir).map_err(|e| Error::internal(e.to_string()))?
            };
            let begin = Instant::now();
            let verdict = run_task(&api, &dir, task, model).await;
            runs.push(Run {
                passed: verdict.is_ok(),
                status: if verdict.is_ok() {
                    "done".into()
                } else {
                    "failed".into()
                },
                steps: verdict.as_ref().ok().copied(),
                tokens: None,
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
        search: true,
        success_rate: if total == 0 {
            0.0
        } else {
            f64::from(passed_all) / f64::from(total)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_suite_parses_and_every_task_is_checked() {
        let s = builtin("coding").unwrap();
        assert!(s.tasks.len() >= 8);
        assert!(
            s.tasks
                .iter()
                .all(|t| !t.checks.is_empty() && !t.turns.is_empty())
        );
        assert!(
            s.tasks.iter().any(|t| t.turns.len() > 1),
            "multi-turn tasks"
        );
    }
}
