//! M2: the model API and the reliability pipeline over real HTTP, with fake
//! Hugging Face and fake llama-server processes.

use std::path::PathBuf;
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::download::DownloadOptions;
use ancilo_models::hardware::{GIB, Gpu, HardwareProfile};
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use futures::StreamExt;
use serde_json::{Value, json};

pub struct Env {
    pub home: TestHome,
    _hf: FakeHf,
    pub d: Option<DaemonHandle>,
}

pub fn repo(id: &str) -> FakeRepo {
    let name = id
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches("-GGUF")
        .to_string();
    FakeRepo::new(
        id,
        vec![FakeFile::gguf(
            &format!("{name}-Q8_0.gguf"),
            "qwen3",
            32768,
            200_000,
        )],
    )
    .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 32768}))
}

impl Env {
    pub async fn start(
        repos: Vec<FakeRepo>,
        script: Option<&str>,
        hw: HardwareProfile,
        env: &[(&str, &str)],
    ) -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(repos).await;
        let hw_file = home.scratch("hw").join("hw.json");
        std::fs::write(&hw_file, serde_json::to_string(&hw).unwrap()).unwrap();
        let mut llama_env: std::collections::BTreeMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(s) = script {
            let p: PathBuf = home.scratch("script").join("script.yaml");
            std::fs::write(&p, s).unwrap();
            llama_env.insert("FAKE_LLM_SCRIPT".into(), p.display().to_string());
        }
        let config = Config {
            hf_endpoint: hf.url(),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(vec![]),
            hardware_override: Some(hw_file),
            llama_server_env: llama_env,
            model_aliases: [("claude-haiku-4-5".to_string(), "small".to_string())]
                .into_iter()
                .collect(),
            ..home.config()
        };
        let options = DaemonOptions {
            manager: ManagerOptions {
                download: DownloadOptions {
                    max_attempts: 2,
                    base_backoff: Duration::from_millis(10),
                    progress_interval: Duration::from_millis(50),
                },
                restart_backoff: Duration::from_millis(50),
                measure_speed: false,
                ..Default::default()
            },
            llama_build: Some(None),
            ..Default::default()
        };
        let d = ancilo_daemon::start(home.paths.clone(), config, options)
            .await
            .unwrap();
        Self {
            home,
            _hf: hf,
            d: Some(d),
        }
    }

    pub fn url(&self) -> String {
        self.d.as_ref().unwrap().url().to_string()
    }

    pub fn token(&self) -> String {
        self.d.as_ref().unwrap().token.clone()
    }

    pub async fn op(&self, name: &str, input: Value) -> Value {
        let r = reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{name}", self.url()))
            .bearer_auth(self.token())
            .header("x-ancilo-confirm", "true")
            .json(&input)
            .send()
            .await
            .unwrap();
        let ok = r.status().is_success();
        let v: Value = r.json().await.unwrap();
        assert!(ok, "{name}: {v}");
        v
    }

    /// Adds a model without starting it (the gateway loads on demand).
    pub async fn add(&self, address: &str) -> String {
        let v = self
            .op(
                "add_model",
                json!({"address": address, "start": false, "context": "small"}),
            )
            .await;
        let id = v["id"].as_str().unwrap().to_string();
        for _ in 0..1200 {
            if self.op("model_status", json!({"model": id})).await["status"] == "ready" {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{id} not ready");
    }

    pub async fn chat(&self, body: Value, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut r = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", self.url()))
            .bearer_auth(self.token())
            .json(&body);
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        let r = r.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    pub async fn stop(mut self) {
        if let Some(d) = self.d.take() {
            d.stop().await;
        }
    }
}

fn tools() -> Value {
    json!([{"type": "function", "function": {"name": "read_file", "description": "Read a file",
        "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}}])
}

fn ask(text: &str) -> Value {
    json!({"model": "default", "messages": [{"role": "user", "content": text}], "tools": tools()})
}

// covers: M2-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nudge_that_runs_away_keeps_the_original_answer() {
    // The model dodges the action; after the nudge it rambles on to its
    // length limit (seen with Qwen3-0.6B: 8192 tokens, 80 s) – the original
    // answer stands, and the nudged request was kept short.
    let script = r#"
steps:
  - respond: { text: "You can open a.rs in your editor to see what it does." }
  - expect: { any_message_contains: "Check your last answer" }
    respond: { text: "and then and then and then and then", finish_reason: "length" }
  - respond: { text: "unused" }
"#;
    let env = Env::start(
        vec![repo("o/M-GGUF")],
        Some(script),
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    env.add("o/M-GGUF").await;
    let (s, r) = env
        .chat(
            ask("Read the file a.rs and tell me what it does."),
            &[("x-ancilo-reliability", "validate,nudge")],
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(
        r["choices"][0]["message"]["content"],
        "You can open a.rs in your editor to see what it does."
    );
    env.stop().await;
}

// covers: M2-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_repairs_retries_and_constrains_over_http() {
    let script = r#"
steps:
  # 1: broken JSON → repaired
  - respond: { tool_calls: [{ name: read_file, arguments: '{"path": "a.rs"' }] }
  # 2: unknown tool → retried with feedback → valid
  - respond: { tool_calls: [{ name: open_everything, arguments: { path: "b.rs" } }] }
  - expect: { any_message_contains: "could not be used" }
    respond: { tool_calls: [{ name: read_file, arguments: { path: "b.rs" } }] }
  # 3: missing required argument → constrained re-ask
  - respond: { tool_calls: [{ name: read_file, arguments: {} }] }
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "c.rs" } }] }
  # 4: pipeline off → the broken call is passed through untouched
  - respond: { raw: '{"name": "read_file", "arguments": {"path": "d.rs"}}' }
  # 5: a plain answer stays a plain answer
  - respond: { text: "All done." }
"#;
    let env = Env::start(
        vec![repo("o/M-GGUF")],
        Some(script),
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    env.add("o/M-GGUF").await;

    let (s, r) = env.chat(ask("one"), &[]).await;
    assert_eq!(s, 200, "{r}");
    let args: Value = serde_json::from_str(
        r["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(args, json!({"path": "a.rs"}));

    let (_, r) = env
        .chat(
            ask("two"),
            &[("x-ancilo-reliability", "validate,repair,retry")],
        )
        .await;
    assert_eq!(
        r["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "read_file",
        "{r}"
    );

    let (_, r) = env
        .chat(
            ask("three"),
            &[("x-ancilo-reliability", "validate,constrained")],
        )
        .await;
    let args: Value = serde_json::from_str(
        r["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(args, json!({"path": "c.rs"}), "{r}");

    let (_, r) = env
        .chat(ask("four"), &[("x-ancilo-reliability", "off")])
        .await;
    assert!(
        r["choices"][0]["message"]["tool_calls"].is_null(),
        "off must not repair: {r}"
    );

    let (_, r) = env.chat(ask("five"), &[]).await;
    assert_eq!(r["choices"][0]["message"]["content"], "All done.");

    let stats = env.op("gateway_stats", json!({})).await;
    assert_eq!(stats["requests"], 5, "{stats}");
    // With the pipeline off the answer is not analysed at all ("ok").
    for (key, n) in [
        ("repaired", 1),
        ("retried", 1),
        ("constrained", 1),
        ("ok", 1),
        ("text", 1),
    ] {
        assert_eq!(stats["outcomes"][key], n, "{key}: {stats}");
    }
    assert!(stats["interventions"]["repair:json"].as_u64().unwrap() >= 1);
    env.stop().await;
}

// covers: M2-AC-09
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_reach_the_intended_model() {
    let env = Env::start(
        vec![repo("o/Big-GGUF"), repo("o/Small-GGUF")],
        None,
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    let big = env.add("o/Big-GGUF").await;
    let small = env.add("o/Small-GGUF").await;
    env.op("assign_role", json!({"role": "small", "model": small}))
        .await;
    let answered_by = |r: &Value| r["model"].as_str().unwrap_or_default().to_string();
    for (name, expected) in [
        (big.as_str(), big.as_str()), // exact id
        (small.as_str(), small.as_str()),
        ("default", big.as_str()),            // role
        ("small", small.as_str()),            // custom role
        ("claude-sonnet-4-6", big.as_str()),  // unknown client name → default
        ("claude-haiku-4-5", small.as_str()), // configured alias → role
        ("", big.as_str()),
    ] {
        let (s, r) = env
            .chat(
                json!({"model": name, "messages": [{"role": "user", "content": "hi"}]}),
                &[],
            )
            .await;
        assert_eq!(s, 200, "{name}: {r}");
        assert_eq!(answered_by(&r), expected, "{name}");
    }
    let models: Value = reqwest::Client::new()
        .get(format!("{}/v1/models", env.url()))
        .bearer_auth(env.token())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    for id in [big.as_str(), small.as_str(), "default", "small"] {
        assert!(ids.contains(&id), "{ids:?}");
    }
    env.stop().await;
}

// covers: M2-AC-10
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn models_load_on_demand_within_the_memory_budget() {
    // Budget for exactly one model at a time; loading takes 3.5 s.
    let hw = HardwareProfile {
        total_ram_bytes: 2 * GIB,
        gpu: Gpu::Metal,
        gpu_memory_bytes: Some(GIB),
        ..HardwareProfile::apple(2)
    };
    let env = Env::start(
        vec![repo("o/A-GGUF"), repo("o/B-GGUF")],
        None,
        hw,
        &[("FAKE_LLM_LOAD_MS", "3500")],
    )
    .await;
    let a = env.add("o/A-GGUF").await;
    let b = env.add("o/B-GGUF").await;
    let mut events = env.d.as_ref().unwrap().bus.subscribe();

    let (s, _) = env
        .chat(
            json!({"model": a, "messages": [{"role": "user", "content": "x"}]}),
            &[],
        )
        .await;
    assert_eq!(s, 200);

    // B needs A's memory. A streaming client sees progress while B loads.
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", env.url()))
        .bearer_auth(env.token())
        .json(&json!({"model": b, "stream": true, "messages": [{"role": "user", "content": "hello b"}]}))
        .send()
        .await
        .unwrap();
    let mut body = resp.bytes_stream();
    let mut text = String::new();
    while let Some(Ok(chunk)) = body.next().await {
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(text.starts_with(": loading model"), "{text}");
    let content: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(content, "hello b");
    assert!(text.trim_end().ends_with("data: [DONE]"));

    // A was evicted before B started – never both in memory.
    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        kinds.push(format!("{}:{}", e.kind, e.subject.unwrap_or_default()));
    }
    let evicted = kinds
        .iter()
        .position(|k| k == &format!("model.evicted:{a}"))
        .expect("A evicted");
    let b_start = kinds
        .iter()
        .position(|k| k == &format!("instance.starting:{b}"))
        .expect("B started");
    assert!(evicted < b_start, "{kinds:?}");
    let hwv = env.op("hardware_info", json!({})).await;
    assert!(hwv["used_bytes"].as_u64().unwrap() <= hwv["model_budget_bytes"].as_u64().unwrap());
    let list = env.op("list_models", json!({})).await;
    let status = |id: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(status(&a), "ready");
    assert_eq!(status(&b), "running");
    env.stop().await;
}

// covers: M2-AC-08
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn concurrent_requests_all_complete() {
    let script = "fallback: { text: \"ok\", delay_ms: 150 }\n";
    let env = Env::start(
        vec![repo("o/M-GGUF")],
        Some(script),
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    env.add("o/M-GGUF").await;
    let (url, token) = (env.url(), env.token());
    let mut handles = Vec::new();
    for (i, p) in [
        "background",
        "interactive",
        "compare",
        "sync",
        "interactive",
        "background",
    ]
    .into_iter()
    .enumerate()
    {
        let (url, token) = (url.clone(), token.clone());
        handles.push(tokio::spawn(async move {
            let r = reqwest::Client::new()
                .post(format!("{url}/v1/chat/completions"))
                .bearer_auth(token)
                .header("x-ancilo-priority", p)
                .json(&json!({"model": "default", "messages": [{"role": "user", "content": format!("r{i}")}]}))
                .send()
                .await
                .unwrap();
            r.status().as_u16()
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), 200);
    }
    let stats = env.op("gateway_stats", json!({})).await;
    assert_eq!(stats["requests"], 6);
    assert!(
        stats["avg_queue_ms"].as_f64().unwrap() > 0.0,
        "some requests must have waited: {stats}"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_api_requires_the_token_and_accepts_x_api_key() {
    let env = Env::start(
        vec![repo("o/M-GGUF")],
        None,
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    env.add("o/M-GGUF").await;
    let c = reqwest::Client::new();
    let body = json!({"model": "default", "max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]});
    let r = c
        .post(format!("{}/v1/messages", env.url()))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let r = c
        .post(format!("{}/v1/messages", env.url()))
        .header("x-api-key", env.token())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["content"][0]["text"], "hi");
    let cfg = env
        .op("model_api_config", json!({"client": "claude_code"}))
        .await;
    assert_eq!(cfg["env"]["ANTHROPIC_BASE_URL"], env.url());
    // Own config dir: a logged-in Claude Code must not send the user's
    // Anthropic credentials to the local server.
    assert!(
        cfg["env"]["CLAUDE_CONFIG_DIR"]
            .as_str()
            .unwrap()
            .contains("clients"),
        "{cfg}"
    );
    assert_eq!(cfg["env"]["ANTHROPIC_API_KEY"], env.token());
    let codex = env.op("model_api_config", json!({"client": "codex"})).await;
    assert!(
        codex["config"]
            .as_str()
            .unwrap()
            .contains("wire_api = \"responses\""),
        "{codex}"
    );
    env.stop().await;
}

/// A model's own pipeline setting wins over the global one; `default` removes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_model_reliability_settings_apply() {
    let script = r#"
cycle: true
steps:
  - respond: { raw: '{"name": "read_file", "arguments": {"path": "a.rs"' }
"#;
    let env = Env::start(
        vec![repo("o/M-GGUF")],
        Some(script),
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    let id = env.add("o/M-GGUF").await;
    // Global default (`standard`): the broken call is repaired.
    let (_, r) = env.chat(ask("one"), &[]).await;
    assert!(r["choices"][0]["message"]["tool_calls"].is_array(), "{r}");
    // Off for this model only → passed through as text.
    env.op("set_reliability", json!({"stages": "off", "model": id}))
        .await;
    let settings = env.op("get_reliability", json!({})).await;
    assert_eq!(settings["models"][&id]["stages"], json!([]));
    // Default: every stage except the prompt hint.
    assert_eq!(settings["global"]["stages"].as_array().unwrap().len(), 5);
    assert!(
        !settings["global"]["stages"]
            .as_array()
            .unwrap()
            .contains(&json!("prompt"))
    );
    let (_, r) = env.chat(ask("two"), &[]).await;
    assert!(r["choices"][0]["message"]["tool_calls"].is_null(), "{r}");
    // A request header still wins.
    let (_, r) = env
        .chat(ask("three"), &[("x-ancilo-reliability", "all")])
        .await;
    assert!(r["choices"][0]["message"]["tool_calls"].is_array(), "{r}");
    env.op("set_reliability", json!({"stages": "default", "model": id}))
        .await;
    assert_eq!(
        env.op("get_reliability", json!({})).await["models"],
        json!({})
    );
    let (_, r) = env.chat(ask("four"), &[]).await;
    assert!(r["choices"][0]["message"]["tool_calls"].is_array(), "{r}");
    env.stop().await;
}

// covers: M2-AC-07
/// A real run of Qwen3-0.6B (recorded with `set_recording`) replayed
/// deterministically: the same answers must lead to the same pipeline
/// decisions and the same eval result – forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_real_run_replays_deterministically() {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/recorded/qwen3-0.6b-q4_0.yaml"
    ))
    .unwrap();
    let env = Env::start(
        vec![repo("o/Q-GGUF")],
        Some(&fixture),
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    env.add("o/Q-GGUF").await;
    let report = env
        .op(
            "run_eval",
            json!({"suite": "tool-calling", "reliability": "all", "repeat": 1}),
        )
        .await;
    let failed: Vec<&str> = report["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["success_rate"] != 1.0)
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        failed,
        vec![
            "read-range",
            "multi-turn-follow-up",
            "long-content",
            "no-tool-given-context"
        ],
        "{report}"
    );
    assert_eq!(report["success_rate"], 0.8);
    let stats = env.op("gateway_stats", json!({})).await;
    // The recorded run: one nudge (forced read), which the small model still answered in text.
    assert_eq!(stats["interventions"]["nudge:force"], 1, "{stats}");
    assert_eq!(stats["outcomes"]["text"], 3, "{stats}");

    // Recording writes valid fake-llm scripts.
    let dir = env.home.scratch("rec");
    env.op("set_recording", json!({"dir": dir})).await;
    env.chat(json!({"model": "default", "messages": [{"role": "user", "content": "Show me the contents of src/main.rs."}], "tools": tools()}), &[]).await;
    env.op("set_recording", json!({})).await;
    let recorded = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let script =
        ancilo_testkit::Script::from_yaml(&std::fs::read_to_string(recorded).unwrap()).unwrap();
    // The fake echoes text, so the nudge stage asks again: two recorded steps.
    assert_eq!(script.steps.len(), 2);
    assert_eq!(
        script.steps[0]
            .expect
            .as_ref()
            .unwrap()
            .last_user_contains
            .as_deref(),
        Some("Show me the contents of src/main.rs.")
    );
    assert!(
        script.steps[1]
            .expect
            .as_ref()
            .unwrap()
            .last_user_contains
            .as_deref()
            .unwrap()
            .starts_with("Check your last answer")
    );
    env.stop().await;
}
