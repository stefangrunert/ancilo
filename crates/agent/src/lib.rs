//! The agent runtime: one loop for every role (worker for delegated tasks,
//! coder in the app, assistant for Ancilo itself) – they differ only in
//! system prompt, tools and permissions.
//!
//! Model calls go through the gateway, so the reliability pipeline applies.

pub mod sandbox;
pub mod tools;

use ancilo_core::EventBus;
use ancilo_gateway::reliability::ReliabilityConfig;
use ancilo_gateway::scheduler::Priority;
use ancilo_gateway::{CallOpts, ChatReply, Gateway};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub use tools::{Access, CodeSearch, FileChange, ShellSettings, ToolOutput, Toolbox, Workspace};

/// Instructions for delegated work. Short on purpose: small models do better
/// with few, clear rules.
pub const WORKER_PROMPT: &str = "You are Ancilo, a coding worker. You complete one well-defined task in the user's project with the tools.
Work step by step: first look at the relevant files (read_file, grep, glob), then make the smallest change that completes the task (edit_file for changes, write_file for new files). Paths are relative to the project root.
Do not ask questions – decide sensibly yourself.
When you are done, reply with a short summary of what you changed (at most 5 lines). If the task cannot be done, reply with the reason.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The task is complete.
    Done,
    /// The model gave up or an error made it impossible.
    Failed,
    /// Stopped by a limit before finishing; changes so far are kept.
    Partial,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub model: String,
    pub system: String,
    pub task: String,
    pub max_steps: u32,
    pub priority: Priority,
    pub reliability: Option<ReliabilityConfig>,
    /// Sampling temperature (default 0.2).
    pub temperature: Option<f64>,
    /// Seed for reproducible sampling (comparisons).
    pub seed: Option<u64>,
    /// Earlier turns of a conversation (without the system prompt).
    pub history: Vec<Value>,
    /// Work on code: only a local model, resolved strictly (an unknown model
    /// is an error, never a fallback that might be a cloud model).
    pub local_only: bool,
}

/// Default sampling temperature for agent work.
pub const DEFAULT_TEMPERATURE: f64 = 0.2;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentOutcome {
    pub status: Status,
    /// The model's final words (or why it stopped).
    pub summary: String,
    pub steps: u32,
    pub tool_calls: u32,
    pub changed_files: Vec<FileChange>,
    /// Unified diff of all changes (possibly truncated).
    pub diff: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Model calls where the reliability pipeline had to step in.
    #[serde(default)]
    pub interventions: u32,
    /// Time spent generating (model calls without queueing and loading).
    #[serde(default)]
    pub generation_ms: u64,
    /// Time spent waiting for the model to load.
    #[serde(default)]
    pub load_ms: u64,
    /// The conversation after this run (history + this turn, no system
    /// prompt) – for chat sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Value>,
}

const MAX_DIFF_CHARS: usize = 40_000;

fn emit(bus: &Option<(EventBus, String)>, kind: &str, data: Value) {
    if let Some((b, s)) = bus {
        b.emit(kind, Some(s), data);
    }
}

fn preview(s: &str) -> String {
    let p: String = s.chars().take(300).collect();
    if s.chars().count() > 300 {
        format!("{p}…")
    } else {
        p
    }
}

/// Runs the agent loop until the model answers without tool calls, a limit is
/// reached, or `cancel` fires.
pub async fn run(
    gateway: &Gateway,
    ws: &dyn Toolbox,
    spec: AgentSpec,
    cancel: CancellationToken,
    events: Option<(EventBus, String)>,
) -> AgentOutcome {
    let tools = ws.definitions();
    let mut messages = vec![json!({"role": "system", "content": spec.system})];
    messages.extend(spec.history.iter().cloned());
    messages.push(json!({"role": "user", "content": match ws.context() {
        Some(c) if spec.history.is_empty() => format!("{}\n\n{c}", spec.task),
        _ => spec.task.clone(),
    }}));
    let (mut steps, mut calls, mut ptok, mut ctok) = (0u32, 0u32, 0u64, 0u64);
    let timing = std::sync::Mutex::new((0u32, 0u64, 0u64)); // interventions, generation, load
    let mut recent: Vec<String> = Vec::new();
    let mut warned_repeat = false;
    let finish = |status: Status, summary: String, steps, calls, ptok, ctok, msgs: &[Value]| {
        let (interventions, generation_ms, load_ms) = *timing.lock().unwrap();
        let (changed_files, mut diff) = ws.changes();
        if diff.len() > MAX_DIFF_CHARS {
            diff.truncate(MAX_DIFF_CHARS);
            diff.push_str("\n… (diff truncated)");
        }
        AgentOutcome {
            status,
            summary,
            steps,
            tool_calls: calls,
            changed_files,
            diff,
            prompt_tokens: ptok,
            completion_tokens: ctok,
            interventions,
            generation_ms,
            load_ms,
            messages: msgs.get(1..).map(<[Value]>::to_vec).unwrap_or_default(),
        }
    };
    emit(
        &events,
        "agent.started",
        json!({"model": spec.model, "max_steps": spec.max_steps}),
    );
    loop {
        if cancel.is_cancelled() {
            return finish(
                Status::Cancelled,
                "cancelled".into(),
                steps,
                calls,
                ptok,
                ctok,
                &messages,
            );
        }
        if steps >= spec.max_steps {
            return finish(
                Status::Partial,
                format!(
                    "stopped after {steps} steps without finishing; the changes so far are kept"
                ),
                steps,
                calls,
                ptok,
                ctok,
                &messages,
            );
        }
        steps += 1;
        let mut req = json!({
            "model": spec.model, "messages": messages, "tools": tools,
            "temperature": spec.temperature.unwrap_or(DEFAULT_TEMPERATURE),
        });
        if let Some(seed) = spec.seed {
            req["seed"] = json!(seed);
        }
        let opts = CallOpts {
            priority: spec.priority,
            reliability: spec.reliability.clone(),
            api: "agent",
            local_only: spec.local_only,
        };
        let reply = tokio::select! {
            _ = cancel.cancelled() => return finish(Status::Cancelled, "cancelled".into(), steps, calls, ptok, ctok, &messages),
            r = gateway.chat(req, opts) => r,
        };
        if let Ok((_, rec)) = &reply {
            let mut t = timing.lock().unwrap();
            let (i, g, l) = *t;
            let intervened = matches!(
                rec.outcome.as_str(),
                "repaired" | "retried" | "constrained" | "nudged"
            );
            *t = (
                i + u32::from(intervened),
                g + rec.latency_ms.saturating_sub(rec.load_ms + rec.queue_ms),
                l + rec.load_ms,
            );
        }
        let resp = match reply {
            Ok((ChatReply::Complete(v), _)) => v,
            Ok((ChatReply::Stream(_), _)) => {
                return finish(
                    Status::Failed,
                    "unexpected stream from the gateway".into(),
                    steps,
                    calls,
                    ptok,
                    ctok,
                    &messages,
                );
            }
            Err(e) => {
                return finish(
                    Status::Failed,
                    format!("model call failed: {}", e.message()),
                    steps,
                    calls,
                    ptok,
                    ctok,
                    &messages,
                );
            }
        };
        ptok += resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        ctok += resp["usage"]["completion_tokens"].as_u64().unwrap_or(0);
        let message = resp["choices"][0]["message"].clone();
        let tool_calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        emit(
            &events,
            "agent.step",
            json!({"step": steps, "tool_calls": tool_calls.len()}),
        );
        if tool_calls.is_empty() {
            let text = message["content"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            emit(&events, "agent.message", json!({"text": preview(&text)}));
            let status = if text.is_empty() {
                Status::Failed
            } else {
                Status::Done
            };
            let summary = if text.is_empty() {
                "the model ended without an answer".into()
            } else {
                text
            };
            messages.push(json!({"role": "assistant", "content": summary}));
            return finish(status, summary, steps, calls, ptok, ctok, &messages);
        }
        messages.push(
            json!({"role": "assistant", "content": message["content"], "tool_calls": tool_calls}),
        );
        for call in &tool_calls {
            calls += 1;
            let name = call["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let args: Value = match &call["function"]["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                v => v.clone(),
            };
            emit(
                &events,
                "agent.tool_called",
                json!({"name": name, "arguments": args}),
            );
            let out = ws.execute(&name, &args).await;
            emit(
                &events,
                "agent.tool_result",
                json!({"name": name, "is_error": out.is_error, "preview": preview(&out.content)}),
            );
            messages
                .push(json!({"role": "tool", "tool_call_id": call["id"], "content": out.content}));
            // Loop protection: the same call again and again.
            let key = format!("{name}{args}");
            recent.push(key);
            if recent.len() > 3 {
                recent.remove(0);
            }
        }
        if recent.len() == 3 && recent.iter().all(|k| *k == recent[0]) {
            if warned_repeat {
                return finish(
                    Status::Failed,
                    "the model kept repeating the same tool call".into(),
                    steps,
                    calls,
                    ptok,
                    ctok,
                    &messages,
                );
            }
            warned_repeat = true;
            messages.push(json!({"role": "user", "content": "You are repeating the same tool call. Change your approach, or finish with a summary."}));
        }
    }
}
