//! A scriptable, OpenAI-compatible model server.
//!
//! Tests describe exactly what the "model" answers – including broken tool
//! calls, errors, dropped connections and delays – so that every behaviour of
//! Ancilo that depends on model output can be tested deterministically.
//!
//! ```yaml
//! model: fake-model
//! steps:
//!   - expect: { last_user_contains: "list files" }
//!     respond:
//!       tool_calls: [{ name: glob, arguments: { pattern: "**/*.rs" } }]
//!   - respond: { text: "There are 3 files.", stream: true, delay_ms: 20 }
//!   - respond: { raw: '{"name": "edit", "arguments": {"path": ' }   # broken
//! fallback: { text: "ok" }
//! ```

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A script: what the fake model answers, step by step.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Script {
    /// Model id reported by `/v1/models` and in responses.
    pub model: Option<String>,
    pub steps: Vec<Step>,
    /// Answer once all steps are used. Default: echo the last user message.
    pub fallback: Option<Respond>,
    /// Dimension of deterministic embeddings.
    pub embedding_dim: Option<usize>,
    /// Start over with the first step once all steps are used (instead of the
    /// fallback) – for repeated runs of the same scenario.
    pub cycle: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Step {
    pub expect: Option<Expect>,
    pub respond: Respond,
}

/// Checks on the incoming request. A mismatch answers HTTP 500 with an
/// explanation, which makes a failing test easy to diagnose.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Expect {
    pub last_user_contains: Option<String>,
    pub any_message_contains: Option<String>,
    /// No message may contain this (e.g. a path the model must not see).
    pub no_message_contains: Option<String>,
    pub model: Option<String>,
    pub has_tools: Option<bool>,
    /// A tool of this name is offered.
    pub offers_tool: Option<String>,
    /// No tool of this name is offered.
    pub lacks_tool: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Respond {
    /// Assistant text.
    pub text: Option<String>,
    /// Structured tool calls.
    pub tool_calls: Vec<ToolCall>,
    /// Raw content as the model emitted it (e.g. malformed tool-call JSON).
    pub raw: Option<String>,
    pub stream: Option<bool>,
    pub delay_ms: Option<u64>,
    pub finish_reason: Option<String>,
    pub http_error: Option<HttpError>,
    /// Close the connection without a response.
    pub drop: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpError {
    pub status: u16,
    #[serde(default)]
    pub message: String,
}

impl Script {
    pub fn from_yaml(text: &str) -> anyhow::Result<Self> {
        Ok(serde_yaml::from_str(text)?)
    }

    /// A script that always answers `text`.
    pub fn always(text: &str) -> Self {
        Script {
            fallback: Some(Respond {
                text: Some(text.into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

/// A request the fake received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recorded {
    pub path: String,
    pub body: Value,
    /// The `Authorization` header, if any.
    #[serde(default)]
    pub authorization: Option<String>,
}

/// Behaviour knobs, mostly for simulating `llama-server` start-up.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `/health` answers 503 "Loading model" for this long.
    pub load_time: Duration,
}

pub struct Inner {
    script: Script,
    next_step: usize,
    requests: Vec<Recorded>,
    started: Instant,
    options: Options,
}

pub type Shared = Arc<Mutex<Inner>>;

/// A running fake model server.
pub struct FakeLlm {
    pub addr: SocketAddr,
    state: Shared,
    task: tokio::task::JoinHandle<()>,
}

impl FakeLlm {
    pub async fn start(script: Script) -> Self {
        Self::start_with(script, Options::default(), "127.0.0.1:0".parse().unwrap()).await
    }

    pub async fn start_with(script: Script, options: Options, addr: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
        let addr = listener.local_addr().unwrap();
        let state: Shared = Arc::new(Mutex::new(Inner {
            script,
            next_step: 0,
            requests: Vec::new(),
            started: Instant::now(),
            options,
        }));
        let app = router(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Self { addr, state, task }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().requests.clone()
    }

    pub fn steps_used(&self) -> usize {
        self.state.lock().unwrap().next_step
    }
}

impl Drop for FakeLlm {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embeddings))
        .route("/__fake/requests", get(recorded))
        .with_state(state)
}

/// Shared state constructor for the binary.
pub fn shared(script: Script, options: Options) -> Shared {
    Arc::new(Mutex::new(Inner {
        script,
        next_step: 0,
        requests: Vec::new(),
        started: Instant::now(),
        options,
    }))
}

async fn health(State(s): State<Shared>) -> Response {
    let s = s.lock().unwrap();
    if s.started.elapsed() < s.options.load_time {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": {"code": 503, "message": "Loading model", "type": "unavailable_error"}})),
        )
            .into_response();
    }
    axum::Json(json!({"status": "ok"})).into_response()
}

fn model_id(s: &Inner) -> String {
    s.script
        .model
        .clone()
        .unwrap_or_else(|| "fake-model".into())
}

async fn models(State(s): State<Shared>) -> Response {
    let s = s.lock().unwrap();
    axum::Json(json!({
        "object": "list",
        "data": [{"id": model_id(&s), "object": "model", "owned_by": "fake"}]
    }))
    .into_response()
}

async fn recorded(State(s): State<Shared>) -> Response {
    axum::Json(s.lock().unwrap().requests.clone()).into_response()
}

fn last_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| content_text(&m["content"]))
        .unwrap_or_default()
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn check(expect: &Expect, body: &Value) -> Result<(), String> {
    if let Some(needle) = &expect.last_user_contains {
        let text = last_user_text(body);
        if !text.contains(needle.as_str()) {
            return Err(format!(
                "expected last user message to contain {needle:?}, got {text:?}"
            ));
        }
    }
    let all = || {
        body["messages"]
            .as_array()
            .map(|m| {
                m.iter()
                    .map(|m| content_text(&m["content"]))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    };
    if let Some(needle) = &expect.any_message_contains
        && !all().contains(needle.as_str())
    {
        return Err(format!("expected a message to contain {needle:?}"));
    }
    if let Some(needle) = &expect.no_message_contains
        && all().contains(needle.as_str())
    {
        return Err(format!("expected no message to contain {needle:?}"));
    }
    if let Some(model) = &expect.model
        && body["model"].as_str() != Some(model.as_str())
    {
        return Err(format!("expected model {model:?}, got {}", body["model"]));
    }
    if let Some(has) = expect.has_tools {
        let present = body["tools"].as_array().is_some_and(|t| !t.is_empty());
        if present != has {
            return Err(format!("expected has_tools={has}, got {present}"));
        }
    }
    let offered = |name: &str| {
        body["tools"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["function"]["name"] == name))
    };
    if let Some(name) = &expect.offers_tool
        && !offered(name)
    {
        return Err(format!("expected the tool {name:?} to be offered"));
    }
    if let Some(name) = &expect.lacks_tool
        && offered(name)
    {
        return Err(format!("expected no tool {name:?}"));
    }
    Ok(())
}

async fn chat(
    State(s): State<Shared>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let (respond, model, mismatch) = {
        let mut st = s.lock().unwrap();
        st.requests.push(Recorded {
            path: "/v1/chat/completions".into(),
            body: body.clone(),
            authorization: headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(String::from),
        });
        let model = model_id(&st);
        if st.script.cycle && !st.script.steps.is_empty() {
            st.next_step %= st.script.steps.len();
        }
        if st.next_step < st.script.steps.len() {
            let step = st.script.steps[st.next_step].clone();
            st.next_step += 1;
            let mismatch = step.expect.as_ref().and_then(|e| check(e, &body).err());
            (step.respond, model, mismatch)
        } else {
            let fallback = st.script.fallback.clone().unwrap_or_else(|| Respond {
                text: Some(last_user_text(&body)),
                ..Default::default()
            });
            (fallback, model, None)
        }
    };
    if let Some(msg) = mismatch {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(
                json!({"error": {"message": format!("fake-llm expectation failed: {msg}")}}),
            ),
        )
            .into_response();
    }
    if let Some(ms) = respond.delay_ms {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
    if respond.drop {
        // Abort the connection by returning a body stream that errors.
        let stream = futures::stream::once(async {
            Err::<bytes::Bytes, std::io::Error>(std::io::Error::other("dropped"))
        });
        return Response::new(Body::from_stream(stream));
    }
    if let Some(err) = &respond.http_error {
        return (
            StatusCode::from_u16(err.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            axum::Json(json!({"error": {"message": err.message}})),
        )
            .into_response();
    }
    let stream = respond
        .stream
        .unwrap_or_else(|| body["stream"].as_bool().unwrap_or(false));
    let content = respond.raw.clone().or(respond.text.clone());
    let tool_calls: Vec<Value> = respond
        .tool_calls
        .iter()
        .enumerate()
        .map(|(i, tc)| {
            json!({
                "id": format!("call_{i}"),
                "type": "function",
                "function": {
                    "name": tc.name,
                    "arguments": if tc.arguments.is_string() {
                        tc.arguments.as_str().unwrap().to_string()
                    } else {
                        tc.arguments.to_string()
                    },
                }
            })
        })
        .collect();
    let finish = respond.finish_reason.clone().unwrap_or_else(|| {
        if tool_calls.is_empty() {
            "stop".into()
        } else {
            "tool_calls".into()
        }
    });
    let prompt_tokens = body.to_string().len() / 4;
    let completion_tokens = content.as_deref().map_or(0, |c| c.len() / 4 + 1);
    let usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
    });

    if !stream {
        let mut message = json!({"role": "assistant", "content": content});
        if !tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(tool_calls);
        }
        return axum::Json(json!({
            "id": "chatcmpl-fake",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": usage,
        }))
        .into_response();
    }

    let mut chunks: Vec<Value> = Vec::new();
    let chunk = |delta: Value, finish: Option<&str>| {
        json!({
            "id": "chatcmpl-fake",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    chunks.push(chunk(json!({"role": "assistant", "content": ""}), None));
    if let Some(text) = &content {
        // Split into a few pieces so clients exercise incremental parsing.
        let pieces: Vec<String> = text
            .split_inclusive(' ')
            .map(str::to_string)
            .collect::<Vec<_>>();
        for p in pieces {
            chunks.push(chunk(json!({"content": p}), None));
        }
    }
    if !tool_calls.is_empty() {
        let indexed: Vec<Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                let mut tc = tc.clone();
                tc["index"] = json!(i);
                tc
            })
            .collect();
        chunks.push(chunk(json!({"tool_calls": indexed}), None));
    }
    let mut last = chunk(json!({}), Some(&finish));
    last["usage"] = usage;
    chunks.push(last);

    let events = chunks
        .into_iter()
        .map(|c| format!("data: {c}\n\n"))
        .chain(std::iter::once("data: [DONE]\n\n".to_string()))
        .map(|s| Ok::<_, Infallible>(bytes::Bytes::from(s)));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(futures::stream::iter(events)))
        .unwrap()
}

/// Deterministic embedding: hashed bag of words, L2-normalised. Texts sharing
/// words get similar vectors, which is enough to test retrieval logic.
pub fn embed(text: &str, dim: usize) -> Vec<f32> {
    let mut v = vec![0f32; dim];
    for word in text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
    {
        let h = Sha256::digest(word.to_lowercase().as_bytes());
        let idx = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as usize % dim;
        let sign = if h[4] & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

async fn embeddings(State(s): State<Shared>, axum::Json(body): axum::Json<Value>) -> Response {
    let (dim, model) = {
        let mut st = s.lock().unwrap();
        st.requests.push(Recorded {
            path: "/v1/embeddings".into(),
            body: body.clone(),
            authorization: None,
        });
        (st.script.embedding_dim.unwrap_or(64), model_id(&st))
    };
    let inputs: Vec<String> = match &body["input"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect(),
        _ => vec![],
    };
    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, t)| json!({"object": "embedding", "index": i, "embedding": embed(t, dim)}))
        .collect();
    axum::Json(json!({"object": "list", "model": model, "data": data})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn post(url: &str, body: Value) -> (u16, Value) {
        let r = reqwest::Client::new()
            .post(url)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    fn chat_body(text: &str) -> Value {
        json!({"model": "m", "messages": [{"role": "user", "content": text}]})
    }

    // covers: M0-AC-03
    #[tokio::test]
    async fn answers_text_and_tool_calls_in_order() {
        let script = Script::from_yaml(
            r#"
steps:
  - expect: { last_user_contains: "files" }
    respond: { tool_calls: [{ name: glob, arguments: { pattern: "*.rs" } }] }
  - respond: { text: "done" }
"#,
        )
        .unwrap();
        let fake = FakeLlm::start(script).await;
        let url = format!("{}/v1/chat/completions", fake.url());
        let (status, r) = post(&url, chat_body("list files")).await;
        assert_eq!(status, 200);
        let tc = &r["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "glob");
        let args: Value =
            serde_json::from_str(tc["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["pattern"], "*.rs");
        assert_eq!(r["choices"][0]["finish_reason"], "tool_calls");
        let (_, r) = post(&url, chat_body("next")).await;
        assert_eq!(r["choices"][0]["message"]["content"], "done");
        assert_eq!(fake.requests().len(), 2);
    }

    // covers: M0-AC-03
    #[tokio::test]
    async fn reports_expectation_mismatch() {
        let script = Script::from_yaml(
            "steps:\n  - expect: { last_user_contains: \"x\" }\n    respond: { text: \"y\" }\n",
        )
        .unwrap();
        let fake = FakeLlm::start(script).await;
        let (status, r) = post(
            &format!("{}/v1/chat/completions", fake.url()),
            chat_body("nope"),
        )
        .await;
        assert_eq!(status, 500);
        assert!(r["error"]["message"].as_str().unwrap().contains("expected"));
    }

    // covers: M0-AC-03
    #[tokio::test]
    async fn returns_raw_broken_output_and_http_errors() {
        let script = Script::from_yaml(
            r#"
steps:
  - respond: { raw: '{"name": "edit", "arguments": {' }
  - respond: { http_error: { status: 503, message: "busy" } }
"#,
        )
        .unwrap();
        let fake = FakeLlm::start(script).await;
        let url = format!("{}/v1/chat/completions", fake.url());
        let (_, r) = post(&url, chat_body("a")).await;
        assert_eq!(
            r["choices"][0]["message"]["content"],
            r#"{"name": "edit", "arguments": {"#
        );
        let (status, _) = post(&url, chat_body("b")).await;
        assert_eq!(status, 503);
    }

    // covers: M0-AC-03
    #[tokio::test]
    async fn streams_sse_chunks() {
        let fake = FakeLlm::start(Script::always("hello streaming world")).await;
        let text = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", fake.url()))
            .json(&json!({"model": "m", "stream": true, "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let mut assembled = String::new();
        for line in text.lines().filter_map(|l| l.strip_prefix("data: ")) {
            if line == "[DONE]" {
                break;
            }
            let v: Value = serde_json::from_str(line).unwrap();
            assembled.push_str(v["choices"][0]["delta"]["content"].as_str().unwrap_or(""));
        }
        assert_eq!(assembled, "hello streaming world");
        assert!(text.ends_with("data: [DONE]\n\n"));
    }

    // covers: M0-AC-03
    #[tokio::test]
    async fn dropped_connection_is_an_error_for_clients() {
        let script = Script::from_yaml("steps:\n  - respond: { drop: true }\n").unwrap();
        let fake = FakeLlm::start(script).await;
        let r = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", fake.url()))
            .json(&chat_body("x"))
            .send()
            .await;
        let failed = match r {
            Err(_) => true,
            Ok(resp) => resp.bytes().await.is_err(),
        };
        assert!(failed);
    }

    // covers: M0-AC-03
    #[tokio::test]
    async fn embeddings_are_deterministic_and_similar_for_shared_words() {
        let fake = FakeLlm::start(Script::default()).await;
        let (_, r) = post(
            &format!("{}/v1/embeddings", fake.url()),
            json!({"model": "e", "input": ["token check auth", "auth token", "banana"]}),
        )
        .await;
        let v: Vec<Vec<f32>> = r["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| serde_json::from_value(d["embedding"].clone()).unwrap())
            .collect();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]));
        assert_eq!(embed("token check auth", 64), v[0]);
    }

    #[tokio::test]
    async fn health_reports_loading_then_ok() {
        let fake = FakeLlm::start_with(
            Script::default(),
            Options {
                load_time: Duration::from_millis(150),
            },
            "127.0.0.1:0".parse().unwrap(),
        )
        .await;
        let url = format!("{}/health", fake.url());
        assert_eq!(reqwest::get(&url).await.unwrap().status().as_u16(), 503);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(reqwest::get(&url).await.unwrap().status().as_u16(), 200);
    }
}
