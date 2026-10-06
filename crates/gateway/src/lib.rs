//! The inference gateway: every model call – external (Codex, Claude Code,
//! Aider) and internal (agent, assistant) – goes through here.
//!
//! route (model name → model) → schedule (priority) → load on demand →
//! reliability pipeline (for tool calls) → upstream llama-server → metrics.

pub mod anthropic;
pub mod ops;
pub mod reliability;
pub mod responses;
pub mod routes;
pub mod scheduler;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ancilo_core::{Error, EventBus, Result};
use ancilo_models::ModelManager;
use ancilo_storage::Db;
use ancilo_storage::rusqlite::params;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::{Value, json};

use ancilo_models::manager::ROLE_DEFAULT;
use ancilo_models::routing::{AbOutcome, AbSource, RouteRequest};
use reliability::{ReliabilityConfig, Stage, Verdict};
use scheduler::{Priority, Scheduler};

/// Per-call options.
#[derive(Debug, Clone)]
pub struct CallOpts {
    pub priority: Priority,
    /// `None`: the gateway default.
    pub reliability: Option<ReliabilityConfig>,
    /// Which API the call came in on (for metrics): `openai`, `anthropic`, `internal`.
    pub api: &'static str,
    /// Only a local model, resolved strictly: code must never reach a cloud
    /// model – not even through a fallback when a model was removed meanwhile.
    pub local_only: bool,
    /// Let a local model think before it answers (reasoning models such as
    /// Qwen3). `false`: the answer at once – a chat on a small model
    /// otherwise thinks for minutes, or until no room is left for the answer.
    pub think: bool,
}

impl Default for CallOpts {
    fn default() -> Self {
        Self {
            priority: Priority::Interactive,
            reliability: None,
            api: "internal",
            local_only: false,
            think: true,
        }
    }
}

/// Result of a chat call.
pub enum ChatReply {
    /// A complete OpenAI chat completion.
    Complete(Value),
    /// Upstream SSE bytes, passed through (requests without tools).
    Stream(std::pin::Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>),
}

/// A model that did not fit when it was asked, and the one that answered.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct Fallback {
    pub from: String,
    pub to: String,
    /// Why `from` did not fit (with the numbers).
    pub why: String,
}

/// What happened to one call – for metrics and tests.
#[derive(Debug, Clone, Default)]
pub struct CallRecord {
    /// Answered with another model: `from` did not fit in memory now.
    pub fallback: Option<Fallback>,
    pub model: String,
    pub stages: Vec<String>,
    pub outcome: String,
    pub queue_ms: u64,
    pub load_ms: u64,
    pub latency_ms: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Gateway {
    inner: Arc<Inner>,
}

struct Inner {
    manager: ModelManager,
    db: Db,
    bus: EventBus,
    http: ancilo_net::Net,
    scheduler: Scheduler,
    defaults: Mutex<ReliabilityConfig>,
    /// Per-model pipeline settings (e.g. measured with `tune_reliability`).
    per_model: Mutex<std::collections::BTreeMap<String, ReliabilityConfig>>,
    load_timeout: Duration,
    /// When set: upstream exchanges are recorded as fake-llm scripts.
    recording: Mutex<Option<Recording>>,
}

/// Records real model answers as fake-llm scripts (one file per model), so
/// real failure modes become deterministic regression tests.
struct Recording {
    dir: std::path::PathBuf,
    steps: std::collections::BTreeMap<String, Vec<Value>>,
}

impl Recording {
    fn step(req: &Value, resp: &Value) -> Value {
        let message = &resp["choices"][0]["message"];
        let mut respond = serde_json::Map::new();
        if let Some(calls) = message["tool_calls"].as_array().filter(|c| !c.is_empty()) {
            let calls: Vec<Value> = calls
                .iter()
                .map(|c| {
                    let raw = c["function"]["arguments"].as_str().unwrap_or("{}");
                    // Keep invalid arguments verbatim – they are the point.
                    let args = serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!(raw));
                    json!({"name": c["function"]["name"], "arguments": args})
                })
                .collect();
            respond.insert("tool_calls".into(), Value::Array(calls));
            if let Some(t) = message["content"].as_str().filter(|t| !t.is_empty()) {
                respond.insert("text".into(), json!(t));
            }
        } else {
            respond.insert(
                "raw".into(),
                json!(message["content"].as_str().unwrap_or_default()),
            );
        }
        let user = reliability::last_user_text(req);
        let snippet: String = user.chars().take(60).collect();
        let mut step = json!({"respond": respond});
        if !snippet.trim().is_empty() {
            step["expect"] = json!({"last_user_contains": snippet});
        }
        step
    }

    fn append(&mut self, model: &str, step: Value) -> Result<()> {
        let steps = self.steps.entry(model.to_string()).or_default();
        steps.push(step);
        std::fs::create_dir_all(&self.dir)?;
        let text = serde_yaml::to_string(&json!({"model": model, "steps": steps}))
            .map_err(Error::internal)?;
        std::fs::write(self.dir.join(format!("{model}.yaml")), text)?;
        Ok(())
    }
}

const SETTING: &str = "gateway.reliability";
const SETTING_PER_MODEL: &str = "gateway.reliability.models";

/// Answer length when the client sets no limit.
pub const DEFAULT_MAX_TOKENS: u64 = 8192;

/// Answer length after a nudge (a tool call or a short sentence is expected).
pub const NUDGE_MAX_TOKENS: u64 = 1024;

impl Gateway {
    pub fn new(manager: ModelManager, db: Db, bus: EventBus) -> Self {
        let defaults = db
            .get_setting(SETTING)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let per_model = db
            .get_setting(SETTING_PER_MODEL)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let outbound = manager.outbound();
        let hosts = manager.hosts().clone();
        Self {
            inner: Arc::new(Inner {
                manager,
                db,
                bus,
                // A local model's answers stay here; what goes to a cloud
                // model is logged (System › What left this Mac).
                http: ancilo_net::Net::new(
                    ancilo_net::with_hosts(reqwest::Client::builder(), hosts.iter())
                        .connect_timeout(Duration::from_secs(10))
                        .build()
                        .expect("http client"),
                    outbound,
                ),
                scheduler: Scheduler::new(2),
                defaults: Mutex::new(defaults),
                per_model: Mutex::new(per_model),
                load_timeout: Duration::from_secs(600),
                recording: Mutex::new(None),
            }),
        }
    }

    /// Keeps `model` loaded while an agent works with it (between its
    /// requests, too) – see `ModelManager::begin_work`.
    pub fn hold(&self, model: &str) -> Option<ancilo_models::manager::WorkGuard> {
        let id = self.inner.manager.resolve_name(model).ok()?;
        Some(self.inner.manager.begin_work(&id))
    }

    pub fn manager(&self) -> &ModelManager {
        &self.inner.manager
    }

    pub fn bus(&self) -> &EventBus {
        &self.inner.bus
    }

    pub fn reliability(&self) -> ReliabilityConfig {
        self.inner.defaults.lock().unwrap().clone()
    }

    pub fn set_reliability(&self, cfg: ReliabilityConfig) -> Result<()> {
        self.inner
            .db
            .set_setting(SETTING, &serde_json::to_string(&cfg)?)?;
        *self.inner.defaults.lock().unwrap() = cfg;
        Ok(())
    }

    /// The pipeline for a model: its own setting, else the global one.
    pub fn reliability_for(&self, model: &str) -> ReliabilityConfig {
        self.inner
            .per_model
            .lock()
            .unwrap()
            .get(model)
            .cloned()
            .unwrap_or_else(|| self.reliability())
    }

    pub fn model_reliability(&self) -> std::collections::BTreeMap<String, ReliabilityConfig> {
        self.inner.per_model.lock().unwrap().clone()
    }

    /// Sets (`Some`) or clears (`None`) the pipeline of one model.
    pub fn set_model_reliability(&self, model: &str, cfg: Option<ReliabilityConfig>) -> Result<()> {
        // Persist first: a failed write must not change live behaviour.
        let mut map = self.inner.per_model.lock().unwrap();
        let mut next = map.clone();
        match cfg {
            Some(c) => next.insert(model.to_string(), c),
            None => next.remove(model),
        };
        self.inner
            .db
            .set_setting(SETTING_PER_MODEL, &serde_json::to_string(&next)?)?;
        *map = next;
        Ok(())
    }

    /// `/v1/models`: every model and role.
    pub fn models(&self) -> Result<Value> {
        let ancilo_models::manager::ModelNames { models, roles } = self.inner.manager.names()?;
        let mut data: Vec<Value> = models
            .iter()
            .map(|m| json!({"id": m.id, "object": "model", "owned_by": "ancilo", "created": m.added_at.timestamp()}))
            .collect();
        for (role, model) in roles {
            data.push(
                json!({"id": role, "object": "model", "owned_by": "ancilo", "alias_of": model}),
            );
        }
        Ok(json!({"object": "list", "data": data}))
    }

    fn log(&self, rec: &CallRecord, api: &str, priority: Priority, stream: bool, tools: bool) {
        let tps = match (rec.completion_tokens, rec.latency_ms) {
            (Some(t), ms) if ms > 0 => Some(t as f64 / (ms as f64 / 1000.0)),
            _ => None,
        };
        let stages = serde_json::to_string(&rec.stages).unwrap_or_default();
        let priority = serde_json::to_value(priority)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let r = self.inner.db.with(|c| {
            c.execute(
                "INSERT INTO inference_log(ts, model, api, priority, stream, tools, latency_ms, queue_ms, load_ms, prompt_tokens, completion_tokens, tokens_per_sec, stages, outcome, error)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    chrono::Utc::now().to_rfc3339(), rec.model, api, priority, stream, tools,
                    rec.latency_ms as i64, rec.queue_ms as i64, rec.load_ms as i64,
                    rec.prompt_tokens.map(|v| v as i64), rec.completion_tokens.map(|v| v as i64), tps,
                    stages, rec.outcome, rec.error
                ],
            )
            .map(|_| ())
        });
        if let Err(e) = r {
            tracing::warn!(error = %e, "failed to log inference");
        }
        if !rec.stages.is_empty() {
            self.inner.bus.emit(
                "gateway.intervened",
                Some(&rec.model),
                json!({"stages": rec.stages, "outcome": rec.outcome}),
            );
        }
    }

    async fn upstream(&self, base: &str, key: &str, body: &Value) -> Result<reqwest::Response> {
        let note = cloud_note(body);
        let resp = self
            .inner
            .http
            .send(
                self.inner
                    .http
                    .post(format!("{base}/chat/completions"))
                    .bearer_auth(key)
                    .json(body),
                note,
            )
            .await
            .map_err(|e| {
                Error::unavailable(ancilo_core::msg("model.no_answer_http", &[("why", &e)]))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(text);
            return Err(if status.as_u16() == 400 {
                Error::invalid(ancilo_core::msg("model.rejected", &[("why", &msg)]))
            } else {
                Error::unavailable(ancilo_core::msg(
                    "model.failed_http",
                    &[("status", &status), ("why", &msg)],
                ))
            });
        }
        Ok(resp)
    }

    /// One answer of the model. A local model that only thought – until its
    /// token limit, no answer, no tool call – is asked once more with
    /// thinking switched off, and `req` keeps it off for the calls after
    /// (observed: a 2B model thinking 8,192 tokens, then "no answer").
    async fn upstream_json(&self, base: &str, key: &str, req: &mut Value) -> Result<Value> {
        let resp = self.upstream_json_once(base, key, req).await?;
        let message = &resp["choices"][0]["message"];
        let no_answer = message["content"]
            .as_str()
            .is_none_or(|c| c.trim().is_empty())
            && message["tool_calls"]
                .as_array()
                .is_none_or(|t| t.is_empty());
        let thought = message["reasoning_content"]
            .as_str()
            .is_some_and(|r| !r.trim().is_empty())
            || resp["choices"][0]["finish_reason"] == "length";
        let local = base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost");
        if no_answer
            && thought
            && local
            && req["chat_template_kwargs"]["enable_thinking"] != json!(false)
        {
            tracing::info!(model = %req["model"], "only thinking, no answer – asked again without thinking");
            req["chat_template_kwargs"]["enable_thinking"] = json!(false);
            return self.upstream_json_once(base, key, req).await;
        }
        Ok(resp)
    }

    async fn upstream_json_once(&self, base: &str, key: &str, body: &Value) -> Result<Value> {
        let mut body = body.clone();
        body["stream"] = json!(false);
        let resp: Value = self
            .upstream(base, key, &body)
            .await?
            .json()
            .await
            .map_err(|e| Error::unavailable(format!("invalid answer from the model: {e}")))?;
        let mut rec = self.inner.recording.lock().unwrap();
        if let Some(r) = rec.as_mut() {
            let model = body["model"].as_str().unwrap_or("model").to_string();
            if let Err(e) = r.append(&model, Recording::step(&body, &resp)) {
                tracing::warn!(error = %e, "recording failed");
            }
        }
        Ok(resp)
    }

    /// Starts (`Some(dir)`) or stops (`None`) recording upstream answers.
    pub fn set_recording(&self, dir: Option<std::path::PathBuf>) {
        *self.inner.recording.lock().unwrap() = dir.map(|dir| Recording {
            dir,
            steps: Default::default(),
        });
    }

    /// OpenAI chat completion through the gateway. If a request does not fit
    /// the model's context, the context grows (model restart) and the request
    /// is repeated once – agents like Codex and Claude Code send large prompts.
    ///
    /// Requests from clients take part in a running A/B test of their role;
    /// the outcome (latency, error, interventions) is reported back. For
    /// streams the latency is the time until the answer starts – the same for
    /// both arms, so the comparison stays fair.
    pub async fn chat(&self, mut req: Value, opts: CallOpts) -> Result<(ChatReply, CallRecord)> {
        let requested = req["model"].as_str().unwrap_or_default().to_string();
        // Measurements (evals, comparisons) never take part in A/B tests.
        let from_client =
            !matches!(opts.api, "agent" | "internal") && opts.priority != Priority::Compare;
        let routed = if opts.local_only {
            let id = self.inner.manager.resolve_strict(&requested)?;
            if self.inner.manager.is_cloud(&id) {
                return Err(Error::PermissionDenied(format!(
                    "'{id}' is a cloud model – code is never sent to the cloud"
                )));
            }
            ancilo_models::routing::Routed {
                model: id,
                via: ancilo_models::routing::Via::Explicit,
                kind: None,
                kind_estimated: false,
                ab: None,
            }
        } else {
            self.inner.manager.route(&RouteRequest {
                model: Some(&requested),
                role: ROLE_DEFAULT,
                ab: from_client.then_some(AbSource::Api),
                ..Default::default()
            })?
        };
        req["model"] = json!(routed.model);
        let result = match self.chat_once(req.clone(), opts.clone()).await {
            Err(e) if context_overflow(&e).is_some() => {
                let needed = context_overflow(&e).unwrap_or(0);
                if self
                    .inner
                    .manager
                    .grow_context(&routed.model, needed, self.inner.load_timeout)
                    .await?
                {
                    self.chat_once(req, opts).await
                } else {
                    Err(e)
                }
            }
            other => other,
        };
        if let Some(ticket) = &routed.ab {
            let outcome = match &result {
                Ok((_, rec)) => AbOutcome {
                    success: None,
                    latency_ms: rec.latency_ms,
                    error: matches!(rec.outcome.as_str(), "error" | "failed"),
                    interventions: u32::from(matches!(
                        rec.outcome.as_str(),
                        "repaired" | "retried" | "constrained" | "nudged"
                    )),
                },
                Err(_) => AbOutcome {
                    error: true,
                    ..Default::default()
                },
            };
            if let Err(e) = self.inner.manager.ab_record(ticket, &outcome) {
                tracing::warn!(error = %e.message(), "recording the A/B outcome failed");
            }
        }
        result
    }

    async fn chat_once(&self, mut req: Value, opts: CallOpts) -> Result<(ChatReply, CallRecord)> {
        let started = Instant::now();
        let requested = req["model"].as_str().unwrap_or_default().to_string();
        let mut model = self.inner.manager.resolve_name(&requested)?;
        let mut rec = CallRecord {
            model: model.clone(),
            ..Default::default()
        };
        let permit = self.inner.scheduler.acquire(&model, opts.priority).await;
        rec.queue_ms = started.elapsed().as_millis() as u64;
        let guard = self.inner.manager.begin_use(&model);
        let load_started = Instant::now();
        let mut fallback_guard = None;
        let ep = match self
            .inner
            .manager
            .ensure_running(&model, self.inner.load_timeout)
            .await
        {
            Ok(ep) => ep,
            // Does not fit now (not even with a smaller context): another
            // local model answers – said, never silently – instead of the
            // request failing. Back to the first as soon as it fits again.
            Err(Error::InsufficientResources(why)) if !self.inner.manager.is_cloud(&model) => {
                let Some(alt) = self.inner.manager.fallback_for(&model).await else {
                    return Err(Error::InsufficientResources(why));
                };
                fallback_guard = Some(self.inner.manager.begin_use(&alt));
                let ep = self
                    .inner
                    .manager
                    .ensure_running(&alt, self.inner.load_timeout)
                    .await
                    .map_err(|_| Error::InsufficientResources(why.clone()))?;
                tracing::warn!(from = %model, to = %alt, %why, "does not fit now – another model answers");
                self.inner.bus.emit(
                    "model.fallback",
                    Some(&model),
                    json!({"to": alt, "why": why}),
                );
                rec.fallback = Some(Fallback {
                    from: model.clone(),
                    to: alt.clone(),
                    why,
                });
                rec.model = alt.clone();
                model = alt;
                ep
            }
            Err(e) => return Err(e),
        };
        let _fallback_guard = fallback_guard;
        let (base, key) = (ep.base.clone(), ep.key.clone());
        rec.load_ms = load_started.elapsed().as_millis() as u64;
        req["model"] = json!(ep.model);
        // No thinking asked: a local model's chat template is told so (cloud
        // APIs would not know the field).
        if !opts.think && !self.inner.manager.is_cloud(&model) {
            req["chat_template_kwargs"]["enable_thinking"] = json!(false);
        }
        // Without a limit, a looping model generates until the context is full
        // (observed: 32k tokens, 4 minutes). Clients that need more say so.
        if req["max_tokens"].is_null() && req["max_completion_tokens"].is_null() {
            req["max_tokens"] = json!(DEFAULT_MAX_TOKENS);
        }
        let stream = req["stream"].as_bool().unwrap_or(false);
        let tools: Vec<Value> = req["tools"].as_array().cloned().unwrap_or_default();
        let cfg = opts
            .reliability
            .clone()
            .unwrap_or_else(|| self.reliability_for(&model));
        let use_pipeline = !tools.is_empty() && !cfg.is_off() && req["tool_choice"] != "none";

        if !use_pipeline && stream {
            // Pass the stream through; log when it ends.
            if req.get("stream_options").is_none() {
                req["stream_options"] = json!({"include_usage": true});
            }
            let resp = match self.upstream(&base, &key, &req).await {
                Ok(r) => r,
                Err(e) => {
                    rec.outcome = "error".into();
                    rec.error = Some(e.message());
                    rec.latency_ms = started.elapsed().as_millis() as u64;
                    self.log(&rec, opts.api, opts.priority, true, !tools.is_empty());
                    return Err(e);
                }
            };
            let me = self.clone();
            let api = opts.api;
            let priority = opts.priority;
            let has_tools = !tools.is_empty();
            let mut rec2 = rec.clone();
            rec2.outcome = "ok".into();
            let body = resp.bytes_stream();
            let s = futures::stream::unfold(
                (body, Some((permit, guard, rec2, String::new()))),
                move |(mut body, state)| {
                    let me = me.clone();
                    async move {
                        let (permit, guard, mut rec, mut tail) = state?;
                        match body.next().await {
                            Some(Ok(chunk)) => {
                                // Track usage from the final chunk.
                                tail.push_str(&String::from_utf8_lossy(&chunk));
                                if tail.len() > 8192 {
                                    tail = tail[tail.len() - 4096..].to_string();
                                }
                                Some((Ok(chunk), (body, Some((permit, guard, rec, tail)))))
                            }
                            Some(Err(e)) => {
                                rec.outcome = "error".into();
                                rec.error = Some(e.to_string());
                                rec.latency_ms = started.elapsed().as_millis() as u64;
                                me.log(&rec, api, priority, true, has_tools);
                                Some((
                                    Err(Error::unavailable(format!("stream interrupted: {e}"))),
                                    (body, None),
                                ))
                            }
                            None => {
                                for line in
                                    tail.lines().rev().filter_map(|l| l.strip_prefix("data: "))
                                {
                                    if let Ok(v) = serde_json::from_str::<Value>(line)
                                        && v["usage"].is_object()
                                    {
                                        rec.prompt_tokens = v["usage"]["prompt_tokens"].as_u64();
                                        rec.completion_tokens =
                                            v["usage"]["completion_tokens"].as_u64();
                                        break;
                                    }
                                }
                                rec.latency_ms = started.elapsed().as_millis() as u64;
                                me.log(&rec, api, priority, true, has_tools);
                                drop((permit, guard));
                                None
                            }
                        }
                    }
                },
            );
            return Ok((ChatReply::Stream(Box::pin(s)), rec));
        }

        let result = if use_pipeline {
            self.pipeline(&base, &key, req, &tools, &cfg, &mut rec)
                .await
        } else {
            let r = self.upstream_json(&base, &key, &mut req).await;
            if r.is_ok() {
                rec.outcome = "ok".into();
            }
            r
        };
        drop(permit);
        drop(guard);
        rec.latency_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(v) => {
                rec.prompt_tokens = v["usage"]["prompt_tokens"].as_u64();
                rec.completion_tokens = v["usage"]["completion_tokens"].as_u64();
                self.log(&rec, opts.api, opts.priority, stream, !tools.is_empty());
                Ok((ChatReply::Complete(v), rec))
            }
            Err(e) => {
                rec.outcome = "error".into();
                rec.error = Some(e.message());
                self.log(&rec, opts.api, opts.priority, stream, !tools.is_empty());
                Err(e)
            }
        }
    }

    /// The reliability pipeline around a tool-using request.
    async fn pipeline(
        &self,
        base: &str,
        key: &str,
        mut req: Value,
        tools: &[Value],
        cfg: &ReliabilityConfig,
        rec: &mut CallRecord,
    ) -> Result<Value> {
        if cfg.has(Stage::Prompt) {
            reliability::adapt_prompt(&mut req);
            rec.stages.push("prompt".into());
        }
        let prompt_only = rec.stages.len();
        let mut retries = 0;
        let mut constrained_used = false;
        let mut nudged = false;
        // The answer before a nudge – kept in case the nudged one runs away.
        let mut before_nudge: Option<Value> = None;
        let mut last_resp;
        loop {
            let resp = self.upstream_json(base, key, &mut req).await?;
            // A nudged answer that ran into its length limit without a tool
            // call is no improvement: the original answer stands.
            if let Some(before) = before_nudge.take()
                && resp["choices"][0]["finish_reason"] == "length"
                && resp["choices"][0]["message"]["tool_calls"].is_null()
            {
                rec.stages.push("nudge:ran-away".into());
                rec.outcome = "text".into();
                return Ok(before);
            }
            let message = resp["choices"][0]["message"].clone();
            let analysis = reliability::analyze(&message, tools, cfg);
            rec.stages.extend(analysis.applied.iter().cloned());
            last_resp = resp;
            match analysis.verdict {
                Verdict::Calls(calls) => {
                    let mut out = last_resp.clone();
                    out["choices"][0]["message"] =
                        reliability::message_with_calls(analysis.content, &calls);
                    out["choices"][0]["finish_reason"] = json!("tool_calls");
                    rec.outcome = if rec.stages.len() > prompt_only {
                        "repaired".into()
                    } else {
                        "ok".into()
                    };
                    if retries > 0 {
                        rec.outcome = "retried".into();
                    }
                    if constrained_used {
                        rec.outcome = "constrained".into();
                    }
                    if nudged {
                        rec.outcome = "nudged".into();
                    }
                    return Ok(out);
                }
                Verdict::Text => {
                    let answer = analysis.content.clone().unwrap_or_default();
                    let decision = if cfg.has(Stage::Nudge) && !nudged {
                        reliability::nudge_decision(&req, &answer)
                    } else {
                        None
                    };
                    if let Some(n) = decision {
                        nudged = true;
                        rec.stages.push(if n.force {
                            "nudge:force".into()
                        } else {
                            "nudge".into()
                        });
                        let mut original = last_resp.clone();
                        if let Some(c) = &analysis.content {
                            original["choices"][0]["message"]["content"] = json!(c);
                        }
                        before_nudge = Some(original);
                        if let Some(msgs) = req["messages"].as_array_mut() {
                            msgs.push(json!({"role": "assistant", "content": answer}));
                            msgs.push(reliability::nudge_message(tools));
                        }
                        // The answer to a nudge is a tool call or a short
                        // sentence; small models sometimes ramble on to the
                        // limit instead (observed: 8192 tokens, 80 s).
                        let limit = req["max_tokens"].as_u64().unwrap_or(DEFAULT_MAX_TOKENS);
                        req["max_tokens"] = json!(limit.min(NUDGE_MAX_TOKENS));
                        // Force a call only for read-only intents (llama.cpp enforces
                        // it with the tool-call grammar) – a false positive can then
                        // never cause a write or a shell command. A client's explicit
                        // choice other than `auto` is never overridden.
                        if n.force
                            && cfg.has(Stage::Constrained)
                            && (req["tool_choice"].is_null() || req["tool_choice"] == "auto")
                        {
                            req["tool_choice"] = json!("required");
                        }
                        continue;
                    }
                    rec.outcome = "text".into();
                    if let Some(c) = analysis.content {
                        last_resp["choices"][0]["message"]["content"] = json!(c);
                    }
                    return Ok(last_resp);
                }
                Verdict::Invalid(errors) => {
                    let can_constrain = cfg.has(Stage::Constrained)
                        && !constrained_used
                        && req["tool_choice"] != "required";
                    let can_retry = cfg.has(Stage::Retry) && retries < cfg.max_retries;
                    if !can_constrain && !can_retry {
                        rec.outcome = "failed".into();
                        rec.error = Some(errors.join("; "));
                        return Ok(last_resp);
                    }
                    // Feedback helps both a constrained and a plain retry.
                    if let Some(msgs) = req["messages"].as_array_mut() {
                        let mut assistant = message.clone();
                        if assistant["content"].is_null() && assistant["tool_calls"].is_array() {
                            // Replay the broken calls as text: providing malformed
                            // structured calls back can break chat templates.
                            assistant = json!({"role": "assistant", "content": message["tool_calls"].to_string()});
                        } else {
                            assistant.as_object_mut().map(|o| o.remove("tool_calls"));
                        }
                        msgs.push(assistant);
                        msgs.push(reliability::feedback(&errors, tools));
                    }
                    if can_constrain {
                        req["tool_choice"] = json!("required");
                        constrained_used = true;
                        rec.stages.push("constrained".into());
                    } else {
                        retries += 1;
                        rec.stages.push(format!("retry:{retries}"));
                    }
                }
            }
        }
    }

    /// `/v1/embeddings` → the `embed` role (or the requested model).
    pub async fn embeddings(&self, mut req: Value) -> Result<Value> {
        let requested = req["model"].as_str().unwrap_or_default().to_string();
        let model = match self.inner.manager.resolve_name(&requested) {
            Ok(id)
                if !requested.is_empty()
                    && self.inner.manager.find(&id).is_ok_and(|r| r.embedding) =>
            {
                id
            }
            _ => self.inner.manager.resolve_name("embed")?,
        };
        let _permit = self
            .inner
            .scheduler
            .acquire(&model, Priority::Interactive)
            .await;
        let _guard = self.inner.manager.begin_use(&model);
        let ep = self
            .inner
            .manager
            .ensure_running(&model, self.inner.load_timeout)
            .await?;
        let (base, key) = (ep.base, ep.key);
        req["model"] = json!(ep.model);
        let note = cloud_note(&req);
        let resp = self
            .inner
            .http
            .send(
                self.inner
                    .http
                    .post(format!("{base}/embeddings"))
                    .bearer_auth(key)
                    .json(&req),
                note,
            )
            .await
            .map_err(|e| Error::unavailable(format!("the embedding model did not answer: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            return Err(Error::unavailable(format!(
                "the embedding model failed: HTTP {status}{}",
                body["error"]["message"]
                    .as_str()
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            )));
        }
        resp.json()
            .await
            .map_err(|e| Error::unavailable(e.to_string()))
    }
}

/// Tokens a rejected request needed, if it was rejected for its size
/// (llama.cpp: "request (11537 tokens) exceeds the available context size").
pub fn context_overflow(e: &Error) -> Option<u64> {
    let msg = e.message();
    if !msg.contains("exceeds the available context size") {
        return None;
    }
    let start = msg.find("request (")? + "request (".len();
    msg[start..].split_whitespace().next()?.parse().ok()
}

/// OpenAI chat completion → SSE (for replies computed as a whole).
pub fn completion_to_sse(resp: &Value) -> String {
    let model = resp["model"].clone();
    let message = &resp["choices"][0]["message"];
    let finish = resp["choices"][0]["finish_reason"].clone();
    let chunk = |delta: Value, finish: Value| {
        json!({"id": resp["id"], "object": "chat.completion.chunk", "created": resp["created"], "model": model,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let mut out = String::new();
    let mut push = |v: Value| out.push_str(&format!("data: {v}\n\n"));
    push(chunk(
        json!({"role": "assistant", "content": ""}),
        Value::Null,
    ));
    if let Some(t) = message["content"].as_str().filter(|t| !t.is_empty()) {
        push(chunk(json!({"content": t}), Value::Null));
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        let indexed: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut c = c.clone();
                c["index"] = json!(i);
                c
            })
            .collect();
        push(chunk(json!({"tool_calls": indexed}), Value::Null));
    }
    let mut last = chunk(json!({}), finish);
    last["usage"] = resp["usage"].clone();
    push(last);
    out.push_str("data: [DONE]\n\n");
    out
}

/// What a request to a model is, should it leave this computer (a cloud
/// model): the model it is for.
fn cloud_note(body: &Value) -> ancilo_net::Note {
    ancilo_net::Note::new(
        ancilo_net::Purpose::CloudModel,
        body["model"].as_str().unwrap_or("a model"),
        ancilo_net::By::You,
    )
}
