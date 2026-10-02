//! Eval-Set III (M6-AC-03/07/10): requests to the assistant with a checkable
//! target state – checked through operations, like any client would.

use std::sync::Arc;

use ancilo_core::{Error, OpCtx, Registry, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AskInput, Assistant};

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
    /// `{model}` and `{embed}` are replaced by the ids of the eval models.
    pub prompt: String,
    #[serde(default)]
    pub setup: Vec<Call>,
    /// Confirm every proposed action (as a user who agrees would).
    #[serde(default)]
    pub confirm: bool,
    pub checks: Vec<Check>,
    #[serde(default)]
    pub cleanup: Vec<Call>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub op: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Check {
    /// Call this operation …
    pub op: Option<String>,
    pub input: Value,
    /// … and its result must contain these values (partial match).
    pub expect: Value,
    /// The answer mentions one of these (case-insensitive).
    pub answer_contains: Vec<String>,
    /// These operations were proposed (not executed) before confirming.
    pub proposed: Vec<String>,
    /// These operations were executed right away.
    pub executed: Vec<String>,
    /// At least one of these operations was executed.
    pub executed_any: Vec<String>,
}

pub fn builtin(name: &str) -> Option<Suite> {
    match name {
        "assistant" => serde_yaml::from_str(include_str!("../../../evals/assistant.yaml")).ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskReport {
    pub id: String,
    pub passed: bool,
    pub failure: Option<String>,
    pub answer: String,
    pub operations: Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub suite: String,
    pub model: String,
    pub started_at: DateTime<Utc>,
    pub success_rate: f64,
    pub tasks: Vec<TaskReport>,
}

/// Partial match: every key/value of `expected` is in `actual`; strings match
/// case-insensitively as substrings; arrays match if some element matches.
pub fn matches(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Null, _) => true,
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|av| matches(v, av))),
        (e, Value::Array(a)) if !e.is_array() => a.iter().any(|x| matches(e, x)),
        (Value::Array(e), Value::Array(a)) => e.iter().all(|x| a.iter().any(|y| matches(x, y))),
        (Value::String(e), Value::String(a)) => a.to_lowercase().contains(&e.to_lowercase()),
        (e, a) => e == a,
    }
}

fn fill(v: &Value, vars: &[(&str, &str)]) -> Value {
    match v {
        Value::String(s) => {
            let mut s = s.clone();
            for (k, val) in vars {
                s = s.replace(&format!("{{{k}}}"), val);
            }
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(x, vars)).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), fill(x, vars))).collect())
        }
        other => other.clone(),
    }
}

/// Runs the suite on the running Ancilo. `vars`: `model`, `embed`, `cwd`, …
pub async fn run(
    assistant: &Assistant,
    registry: &Arc<Registry>,
    suite: &Suite,
    model: Option<&str>,
    vars: &[(&str, &str)],
) -> Result<Report> {
    let started_at = Utc::now();
    let mut tasks = Vec::new();
    let mut used_model = String::new();
    for task in &suite.tasks {
        let t0 = std::time::Instant::now();
        for c in &task.setup {
            registry
                .call(&c.op, OpCtx::internal(), fill(&c.input, vars))
                .await
                .map_err(|e| Error::internal(format!("setup of {}: {}", task.id, e.message())))?;
        }
        let prompt = fill(&Value::String(task.prompt.clone()), vars);
        let result = assistant
            .ask(AskInput {
                prompt: prompt.as_str().unwrap_or_default().to_string(),
                model: model.map(String::from),
                conversation: None,
                remember: false,
                kind: None,
                greeting: None,
                web: None,
            })
            .await;
        let (passed, failure, answer, operations) = match result {
            Err(e) => (false, Some(e.message()), String::new(), Vec::new()),
            Ok(out) => {
                used_model = out.model.clone();
                let ops: Vec<String> = out
                    .operations
                    .iter()
                    .map(|o| format!("{}:{}", o.operation, o.outcome))
                    .collect();
                let proposed: Vec<String> =
                    out.pending.iter().map(|p| p.operation.clone()).collect();
                let mut confirm_error = None;
                if task.confirm {
                    for p in &out.pending {
                        if let Err(e) = assistant.confirm(&p.id).await {
                            confirm_error = Some(format!(
                                "confirmed {} {} failed: {}",
                                p.operation,
                                p.input,
                                e.message()
                            ));
                        }
                    }
                } else {
                    for p in &out.pending {
                        assistant.reject(&p.id).ok();
                    }
                }
                let mut failure = confirm_error;
                for c in task.checks.iter().filter(|_| failure.is_none()) {
                    let wanted: Vec<String> = c
                        .answer_contains
                        .iter()
                        .map(|w| {
                            fill(&Value::String(w.clone()), vars)
                                .as_str()
                                .unwrap_or_default()
                                .to_lowercase()
                        })
                        .collect();
                    if !wanted.is_empty()
                        && !wanted.iter().any(|w| out.answer.to_lowercase().contains(w))
                    {
                        failure = Some(format!("answer lacks any of {:?}", c.answer_contains));
                        break;
                    }
                    if let Some(p) = c.proposed.iter().find(|p| !proposed.contains(p)) {
                        failure = Some(format!("'{p}' was not proposed (proposed: {proposed:?})"));
                        break;
                    }
                    // Grounding in the knowledge base counts as a search.
                    let executed: Vec<&str> = out
                        .operations
                        .iter()
                        .filter(|o| o.outcome == "executed")
                        .map(|o| o.operation.as_str())
                        .chain(out.grounded.then_some("search"))
                        .collect();
                    if let Some(e) = c.executed.iter().find(|e| !executed.contains(&e.as_str())) {
                        failure = Some(format!("'{e}' was not executed (executed: {executed:?})"));
                        break;
                    }
                    if !c.executed_any.is_empty()
                        && !c
                            .executed_any
                            .iter()
                            .any(|e| executed.contains(&e.as_str()))
                    {
                        failure = Some(format!(
                            "none of {:?} was executed (executed: {executed:?})",
                            c.executed_any
                        ));
                        break;
                    }
                    if let Some(op) = &c.op {
                        match registry
                            .call(op, OpCtx::internal(), fill(&c.input, vars))
                            .await
                        {
                            Ok(v) if matches(&fill(&c.expect, vars), &v) => {}
                            Ok(v) => {
                                failure = Some(format!(
                                    "{op}: expected {} in {}",
                                    c.expect,
                                    crate::clip(&v.to_string(), 300)
                                ));
                                break;
                            }
                            Err(e) => {
                                failure = Some(format!("{op}: {}", e.message()));
                                break;
                            }
                        }
                    }
                }
                (failure.is_none(), failure, out.answer, ops)
            }
        };
        for c in &task.cleanup {
            registry
                .call(&c.op, OpCtx::internal(), fill(&c.input, vars))
                .await
                .ok();
        }
        tasks.push(TaskReport {
            id: task.id.clone(),
            passed,
            failure,
            answer: crate::clip(&answer, 500),
            operations,
            duration_ms: t0.elapsed().as_millis() as u64,
        });
    }
    let passed = tasks.iter().filter(|t| t.passed).count() as f64;
    Ok(Report {
        suite: suite.name.clone(),
        model: used_model,
        started_at,
        success_rate: if tasks.is_empty() {
            0.0
        } else {
            passed / tasks.len() as f64
        },
        tasks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_matching() {
        assert!(matches(
            &json!({"model": "Qwen"}),
            &json!({"model": "qwen3-4b", "via": "role"})
        ));
        assert!(matches(
            &json!({"kind": "tests"}),
            &json!([{"kind": "docs"}, {"kind": "tests", "model": "x"}])
        ));
        assert!(!matches(
            &json!({"status": "running"}),
            &json!({"status": "stopped"})
        ));
        assert!(matches(&json!(null), &json!(1)));
    }

    #[test]
    fn the_builtin_suite_parses() {
        let s = builtin("assistant").unwrap();
        assert!(s.tasks.len() >= 10);
        assert!(s.tasks.iter().all(|t| !t.checks.is_empty()));
    }
}
