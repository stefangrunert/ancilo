//! Eval runner.
//!
//! An eval suite is a list of tasks (chat messages, optional tools, checks).
//! The runner sends every task several times to an OpenAI-compatible endpoint,
//! applies the checks and reports success rates. Reports are JSON with a
//! published schema so results can be compared across models and
//! configurations (reliability pipeline on/off, model A vs. B, …).

pub mod coding;
pub mod delegation;

use std::path::Path;
use std::time::Instant;

use ancilo_core::{Error, Result, schema_of};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub name: String,
    /// Tools offered to every task that does not define its own.
    #[serde(default)]
    pub tools: Vec<Value>,
    /// Shared system prompt (referenced by tasks via YAML anchors).
    #[serde(default)]
    pub system: Option<String>,
    pub tasks: Vec<Task>,
}

/// Parses YAML into a JSON value (for callers without a YAML dependency).
pub fn yaml_value(text: &str) -> Result<Value> {
    serde_yaml::from_str(text).map_err(|e| Error::invalid(format!("invalid YAML: {e}")))
}

/// Suites built into Ancilo.
pub fn builtin(name: &str) -> Option<Suite> {
    let text = match name {
        "tool-calling" => include_str!("../../../evals/tool-calling.yaml"),
        "sample" => include_str!("../../../evals/sample.yaml"),
        "no-tool" => include_str!("../../../evals/no-tool.yaml"),
        "tool-calling-holdout" => include_str!("../../../evals/tool-calling-holdout.yaml"),
        "no-tool-holdout" => include_str!("../../../evals/no-tool-holdout.yaml"),
        _ => return None,
    };
    serde_yaml::from_str(text).ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub tools: Vec<Value>,
    pub checks: Vec<Check>,
}

/// A condition on the model's answer. Exactly one field is set per check;
/// all checks of a task must hold.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Check {
    /// The answer text contains this string (case-insensitive).
    pub contains: Option<String>,
    /// The answer calls this tool with valid JSON arguments.
    pub tool_call: Option<ToolCallCheck>,
    /// `true`: the answer contains no tool call.
    pub no_tool_call: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolCallCheck {
    pub name: String,
    #[serde(default)]
    pub required_args: Vec<String>,
    /// Expected argument values: strings must be contained (case-insensitive),
    /// other values equal, objects are matched recursively.
    #[serde(default)]
    pub args: Option<Value>,
    /// For string arguments: every listed piece must appear (e.g. all lines
    /// of a file the user asked for).
    #[serde(default)]
    pub args_contain_all: std::collections::BTreeMap<String, Vec<String>>,
}

/// Whether `actual` satisfies the partial expectation `expected`.
pub fn matches(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|av| matches(v, av))),
        (Value::String(e), Value::String(a)) => a.to_lowercase().contains(&e.to_lowercase()),
        (Value::Number(e), Value::Number(a)) => e.as_f64() == a.as_f64(),
        (Value::Number(e), Value::String(a)) => a.trim() == e.to_string(),
        // Every expected element must match some actual element (any order).
        (Value::Array(e), Value::Array(a)) => e.iter().all(|x| a.iter().any(|y| matches(x, y))),
        (e, a) => e == a,
    }
}

impl Suite {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_yaml::from_str(&text).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    /// Base URL of an OpenAI-compatible API (without `/v1`).
    pub base_url: String,
    pub model: String,
    /// Free-form label of the configuration under test (e.g. `reliability=on`).
    pub label: String,
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// Extra request headers (e.g. `x-ancilo-reliability`).
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub suite: String,
    pub target: TargetInfo,
    pub started_at: DateTime<Utc>,
    pub repeats: u32,
    pub tasks: Vec<TaskResult>,
    /// Passed runs / all runs.
    pub success_rate: f64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    #[serde(default)]
    pub latency_mean_ms: u64,
    /// Runs whose request failed (see [`RunResult::error`]).
    #[serde(default)]
    pub errors: u32,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TargetInfo {
    pub model: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskResult {
    pub id: String,
    pub runs: Vec<RunResult>,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunResult {
    pub passed: bool,
    pub latency_ms: u64,
    /// Why the run failed (failed check, HTTP error, …).
    pub failure: Option<String>,
    /// The request itself failed (connection, HTTP error, invalid answer) –
    /// a measurement problem, not a behaviour of the model.
    #[serde(default)]
    pub error: bool,
}

/// JSON schema of [`Report`].
pub fn report_schema() -> Value {
    schema_of::<Report>()
}

fn evaluate(checks: &[Check], message: &Value) -> std::result::Result<(), String> {
    let text = message["content"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let calls = message["tool_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for check in checks {
        if let Some(s) = &check.contains
            && !text.contains(&s.to_lowercase())
        {
            return Err(format!("answer does not contain {s:?}"));
        }
        if check.no_tool_call == Some(true) && !calls.is_empty() {
            return Err("unexpected tool call".into());
        }
        if let Some(tc) = &check.tool_call {
            let call = calls
                .iter()
                .find(|c| c["function"]["name"] == tc.name.as_str())
                .ok_or_else(|| format!("no call to tool {:?}", tc.name))?;
            let args: Value = match &call["function"]["arguments"] {
                Value::String(s) => serde_json::from_str(s)
                    .map_err(|e| format!("arguments of {:?} are not valid JSON: {e}", tc.name))?,
                v => v.clone(),
            };
            for key in &tc.required_args {
                if args.get(key).is_none() {
                    return Err(format!("tool {:?} misses argument {key:?}", tc.name));
                }
            }
            if let Some(expected) = &tc.args
                && !matches(expected, &args)
            {
                return Err(format!(
                    "tool {:?} has wrong arguments: expected {expected}, got {args}",
                    tc.name
                ));
            }
            for (key, pieces) in &tc.args_contain_all {
                let value = args[key].as_str().unwrap_or_default();
                if let Some(missing) = pieces.iter().find(|p| !value.contains(p.as_str())) {
                    return Err(format!(
                        "argument {key:?} of {:?} lacks {missing:?}: {value:?}",
                        tc.name
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn run_once(
    client: &reqwest::Client,
    target: &Target,
    task: &Task,
    suite_tools: &[Value],
) -> RunResult {
    let started = Instant::now();
    let mut body = json!({"model": target.model, "messages": task.messages, "temperature": 0});
    let tools = if task.tools.is_empty() {
        suite_tools
    } else {
        &task.tools
    };
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    let mut req = client
        .post(format!(
            "{}/v1/chat/completions",
            target.base_url.trim_end_matches('/')
        ))
        .json(&body);
    if let Some(t) = &target.bearer_token {
        req = req.bearer_auth(t);
    }
    for (k, v) in &target.headers {
        req = req.header(k, v);
    }
    // Err((message, is_infrastructure_error))
    let outcome: Result<(), (String, bool)> = async {
        let resp = req
            .send()
            .await
            .map_err(|e| (format!("request failed: {e}"), true))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| (format!("invalid response: {e}"), true))?;
        if !status.is_success() {
            return Err((format!("HTTP {status}: {}", v["error"]["message"]), true));
        }
        evaluate(&task.checks, &v["choices"][0]["message"]).map_err(|e| (e, false))
    }
    .await;
    RunResult {
        passed: outcome.is_ok(),
        latency_ms: started.elapsed().as_millis() as u64,
        error: outcome.as_ref().err().is_some_and(|e| e.1),
        failure: outcome.err().map(|e| e.0),
    }
}

/// Runs every task `repeats` times, sequentially (one GPU, comparable timings).
pub async fn run(suite: &Suite, target: &Target, repeats: u32) -> Report {
    let client = reqwest::Client::new();
    let started_at = Utc::now();
    let mut tasks = Vec::new();
    let (mut passed, mut total) = (0u32, 0u32);
    for task in &suite.tasks {
        let mut runs = Vec::new();
        for _ in 0..repeats.max(1) {
            let r = run_once(&client, target, task, &suite.tools).await;
            total += 1;
            passed += u32::from(r.passed);
            runs.push(r);
        }
        let ok = runs.iter().filter(|r| r.passed).count() as f64;
        tasks.push(TaskResult {
            id: task.id.clone(),
            success_rate: ok / runs.len() as f64,
            runs,
        });
    }
    let mut latencies: Vec<u64> = tasks
        .iter()
        .flat_map(|t| t.runs.iter().map(|r| r.latency_ms))
        .collect();
    latencies.sort_unstable();
    let errors = tasks
        .iter()
        .flat_map(|t| t.runs.iter())
        .filter(|r| r.error)
        .count() as u32;
    Report {
        suite: suite.name.clone(),
        target: TargetInfo {
            model: target.model.clone(),
            label: target.label.clone(),
        },
        started_at,
        repeats: repeats.max(1),
        tasks,
        success_rate: if total == 0 {
            0.0
        } else {
            f64::from(passed) / f64::from(total)
        },
        latency_p50_ms: percentile(&latencies, 0.5),
        latency_p95_ms: percentile(&latencies, 0.95),
        latency_mean_ms: if latencies.is_empty() {
            0
        } else {
            latencies.iter().sum::<u64>() / latencies.len() as u64
        },
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ancilo_testkit::{FakeLlm, Script};

    fn sample() -> Suite {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/sample.yaml");
        Suite::load(&root).unwrap()
    }

    // covers: M0-AC-06
    #[tokio::test]
    async fn runs_repeatedly_and_writes_schema_valid_report() {
        // greet passes twice, tool call fails once (broken JSON) then passes.
        let script = Script::from_yaml(
            r#"
steps:
  - respond: { text: "Hello there" }
  - respond: { text: "hello!" }
  - respond: { raw: '{"name": "glob", "arguments": {' }
  - respond: { tool_calls: [{ name: glob, arguments: { pattern: "*" } }] }
"#,
        )
        .unwrap();
        let fake = FakeLlm::start(script).await;
        let target = Target {
            base_url: fake.url(),
            model: "fake".into(),
            label: "test".into(),
            bearer_token: None,
            headers: Default::default(),
        };
        // Order: greet×2 then tool×2.
        let report = run(&sample(), &target, 2).await;
        assert_eq!(report.tasks[0].success_rate, 1.0);
        assert_eq!(report.tasks[1].success_rate, 0.5);
        assert_eq!(report.success_rate, 0.75);
        assert!(
            report.tasks[1].runs[0]
                .failure
                .as_ref()
                .unwrap()
                .contains("no call")
        );

        let json = serde_json::to_value(&report).unwrap();
        let validator = jsonschema::validator_for(&report_schema()).unwrap();
        assert!(validator.is_valid(&json), "report must match its schema");
    }

    #[test]
    fn builtin_suites_parse_and_share_tools() {
        let s = builtin("tool-calling").unwrap();
        assert_eq!(s.tasks.len(), 20);
        let n = builtin("no-tool").unwrap();
        assert!(n.tasks.len() >= 50, "{}", n.tasks.len());
        assert!(
            n.tasks
                .iter()
                .all(|t| t.checks.iter().any(|c| c.no_tool_call == Some(true)))
        );
        assert!(s.tools.len() >= 8);
        assert!(s.tasks.iter().all(|t| !t.checks.is_empty()));
        // Holdout sets: same tools, different tasks.
        let h = builtin("tool-calling-holdout").unwrap();
        assert_eq!(h.tasks.len(), 20);
        assert_eq!(h.tools.len(), s.tools.len());
        let nh = builtin("no-tool-holdout").unwrap();
        assert_eq!(nh.tasks.len(), 30);
        let dev: Vec<String> = s
            .tasks
            .iter()
            .chain(&n.tasks)
            .map(|t| t.id.clone())
            .collect();
        assert!(
            h.tasks
                .iter()
                .chain(&nh.tasks)
                .all(|t| !dev.contains(&t.id))
        );
        let dev_prompts: Vec<String> = s
            .tasks
            .iter()
            .chain(&n.tasks)
            .map(|t| t.messages.last().unwrap()["content"].to_string())
            .collect();
        assert!(
            h.tasks
                .iter()
                .chain(&nh.tasks)
                .all(|t| !dev_prompts.contains(&t.messages.last().unwrap()["content"].to_string()))
        );
    }

    #[test]
    fn partial_argument_matching() {
        assert!(matches(
            &json!({"path": "main.rs"}),
            &json!({"path": "src/main.rs", "limit": 3})
        ));
        assert!(matches(
            &json!({"a": {"login": "Octo"}}),
            &json!({"a": {"login": "octocat"}})
        ));
        assert!(matches(&json!({"n": 300}), &json!({"n": "300"})));
        assert!(!matches(&json!({"p": "x"}), &json!({})));
        assert!(!matches(&json!({"flag": true}), &json!({"flag": false})));
    }

    #[test]
    fn checks_tool_arguments() {
        let msg = json!({"content": null, "tool_calls": [{"function": {"name": "glob", "arguments": "{}"}}]});
        let checks = vec![Check {
            tool_call: Some(ToolCallCheck {
                name: "glob".into(),
                required_args: vec!["pattern".into()],
                args: None,
                args_contain_all: Default::default(),
            }),
            ..Default::default()
        }];
        assert!(
            evaluate(&checks, &msg)
                .unwrap_err()
                .contains("misses argument")
        );
    }
}
