//! M5: code index, knowledge base and `search` – over the real daemon with a
//! fake chat model and a fake embedding model (deterministic hashed
//! bag-of-words embeddings from fake-llm).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin, home::git_repo};
use serde_json::{Value, json};

struct Env {
    home: TestHome,
    _hf: FakeHf,
    d: Option<DaemonHandle>,
}

fn repo(id: &str, arch: &str) -> FakeRepo {
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
            arch,
            2048,
            150_000,
        )],
    )
    .with_gguf_meta(json!({"architecture": arch, "context_length": 2048}))
}

impl Env {
    async fn start(scripts: &[(&str, &str)]) -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(vec![
            repo("o/Chat-GGUF", "qwen3"),
            repo("o/Tiny-Embed-GGUF", "bert"),
        ])
        .await;
        let dir = home.scratch("scripts");
        for (stem, script) in scripts {
            std::fs::write(dir.join(format!("{stem}.yaml")), script).unwrap();
        }
        let hw = home.scratch("hw").join("hw.json");
        std::fs::write(
            &hw,
            serde_json::to_string(&HardwareProfile::apple(64)).unwrap(),
        )
        .unwrap();
        let config = Config {
            hf_endpoint: hf.url(),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(vec![]),
            hardware_override: Some(hw),
            llama_server_env: [("FAKE_LLM_SCRIPT_DIR".to_string(), dir.display().to_string())]
                .into_iter()
                .collect(),
            ancilo_bin: Some(fake_llama_server_bin().with_file_name("ancilo")),
            ..home.config()
        };
        home.write_config(&config);
        let options = DaemonOptions {
            manager: ManagerOptions {
                measure_speed: false,
                restart_backoff: Duration::from_millis(50),
                ..Default::default()
            },
            llama_build: Some(None),
            tasks: ancilo_tasks::Options {
                shell: ancilo_agent::ShellSettings {
                    sandbox: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let d = ancilo_daemon::start(home.paths.clone(), config, options)
            .await
            .unwrap();
        let env = Self {
            home,
            _hf: hf,
            d: Some(d),
        };
        env.add("o/Chat-GGUF").await;
        env.add("o/Tiny-Embed-GGUF").await;
        env
    }

    fn d(&self) -> &DaemonHandle {
        self.d.as_ref().unwrap()
    }

    async fn op(&self, name: &str, input: Value) -> Value {
        let r = reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{name}", self.d().url()))
            .bearer_auth(&self.d().token)
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

    async fn add(&self, address: &str) -> String {
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

    async fn stop(mut self) {
        if let Some(d) = self.d.take() {
            d.stop().await;
        }
    }
}

fn project(env: &Env) -> PathBuf {
    let dir = env.home.scratch("project");
    git_repo(
        &dir,
        &[
            (
                "src/auth.rs",
                "/// Checks the bearer token of a request.\npub fn verify_token(token: &str) -> bool {\n    !token.is_empty()\n}\n",
            ),
            (
                "src/db.rs",
                "/// Opens the connection pool.\npub fn connect_pool(max_connections: u32) {}\n",
            ),
            ("README.md", "# Demo\n\nA demo project.\n"),
        ],
    );
    std::fs::canonicalize(dir).unwrap()
}

fn paths(v: &Value) -> Vec<String> {
    v["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap().to_string())
        .collect()
}

// covers: M5-AC-08, M5-AC-02
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projects_are_searchable_with_deterministic_embeddings_and_stay_current() {
    let env = Env::start(&[]).await;
    let roles = env.op("list_models", json!({})).await;
    assert!(roles.to_string().contains("\"embed\""), "{roles}");
    let dir = project(&env);
    let status = env.op("index_project", json!({"cwd": dir})).await;
    assert_eq!(status["mode"], "hybrid");
    assert_eq!(status["files"], 3);
    assert_eq!(status["embedded"], status["chunks"]);

    let r = env
        .op(
            "search",
            json!({"query": "where is the bearer token checked", "cwd": dir}),
        )
        .await;
    assert_eq!(r["mode"], "hybrid");
    assert_eq!(paths(&r)[0], "src/auth.rs", "{r}");
    let r = env
        .op(
            "search",
            json!({"query": "connect_pool", "cwd": dir, "limit": 1}),
        )
        .await;
    assert_eq!(r["hits"][0]["symbol"], "connect_pool");

    // The index follows changes without an explicit re-index.
    std::fs::write(
        dir.join("src/cache.rs"),
        "/// Evicts least recently used entries.\npub fn evict_lru() {}\n",
    )
    .unwrap();
    std::fs::remove_file(dir.join("src/db.rs")).unwrap();
    let r = env
        .op("search", json!({"query": "evict_lru", "cwd": dir}))
        .await;
    assert_eq!(paths(&r)[0], "src/cache.rs");
    // That search brought the index up to date – exactly the changed files.
    let status = env.op("index_status", json!({"cwd": dir})).await;
    assert_eq!(status["files"], 3);
    assert_eq!(status["last_refresh"]["added"], 1, "{status}");
    assert_eq!(status["last_refresh"]["removed"], 1);
    assert_eq!(status["last_refresh"]["unchanged"], 2);
    let r = env
        .op("search", json!({"query": "connect_pool", "cwd": dir}))
        .await;
    assert!(!paths(&r).contains(&"src/db.rs".to_string()), "{r}");
    env.op("remove_index", json!({"cwd": dir})).await;
    assert_eq!(
        env.op("index_status", json!({"cwd": dir})).await["files"],
        0
    );
    env.stop().await;
}

// covers: M5-AC-03, M5-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retrieval_and_knowledge_evals_meet_their_floor_with_fake_embeddings() {
    let env = Env::start(&[]).await;
    // Knowledge base is built in the background at start.
    let mut status = Value::Null;
    for _ in 0..1200 {
        status = env.op("knowledge_status", json!({})).await;
        if status["files"].as_u64().unwrap_or(0) >= 14 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(status["files"].as_u64().unwrap() >= 14, "{status}");
    let code = env
        .op("run_retrieval_eval", json!({"suite": "retrieval", "k": 5}))
        .await;
    let knowledge = env
        .op("run_retrieval_eval", json!({"suite": "knowledge", "k": 5}))
        .await;
    // Floors for the deterministic (meaningless) embeddings – full text and
    // symbols carry most of it. Real-model thresholds: M5 spec, "Erkenntnisse".
    assert!(code["recall_at_k"].as_f64().unwrap() >= 0.8, "{code}");
    assert!(
        knowledge["recall_at_k"].as_f64().unwrap() >= 0.8,
        "{knowledge}"
    );
    assert_eq!(code["mode"], "hybrid");
    env.stop().await;
}

// covers: M5-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_knowledge_base_follows_new_results() {
    let env = Env::start(&[]).await;
    let r = env
        .op(
            "search",
            json!({"query": "how do I connect Codex", "scope": "knowledge"}),
        )
        .await;
    assert!(paths(&r).contains(&"docs/delegation.md".to_string()), "{r}");
    let r = env
        .op(
            "search",
            json!({"query": "mixture of experts", "scope": "knowledge"}),
        )
        .await;
    assert!(paths(&r).iter().any(|p| p.starts_with("models/")), "{r}");
    // A new eval result shows up after the run.
    env.op(
        "run_eval",
        json!({"suite": "sample", "model": "chat-q8_0", "repeat": 1}),
    )
    .await;
    let mut found = false;
    for _ in 0..1200 {
        let r = env
            .op(
                "search",
                json!({"query": "Eval result sample chat-q8_0", "scope": "knowledge"}),
            )
            .await;
        if paths(&r)
            .iter()
            .any(|p| p.starts_with("evals/sample/chat-q8_0"))
        {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(found, "eval result not in the knowledge base");
    env.stop().await;
}

// covers: M5-AC-09
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_and_mcp_clients_can_search() {
    // The worker searches first and must see the hit in the tool result.
    let script = r#"
steps:
  - expect: { has_tools: true }
    respond: { tool_calls: [{ name: search, arguments: { query: "where is the token verified" } }] }
  - expect: { any_message_contains: "src/auth.rs:1-4 (verify_token)" }
    respond: { text: "The token is verified in src/auth.rs (verify_token)." }
"#;
    let env = Env::start(&[("Chat-Q8_0", script)]).await;
    let dir = project(&env);
    let t = env
        .op(
            "delegate",
            json!({"task": "Where is the token verified?", "cwd": dir, "allow": "read"}),
        )
        .await;
    assert_eq!(t["status"], "done", "{t}");
    assert!(t["summary"].as_str().unwrap().contains("verify_token"));

    // MCP: `search` is a tool of its own.
    let home = env.home.path().to_path_buf();
    let d = dir.clone();
    let replies = tokio::task::spawn_blocking(move || {
        let mut child = std::process::Command::new(fake_llama_server_bin().with_file_name("ancilo"))
            .arg("mcp")
            .env("ANCILO_HOME", &home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let msgs = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "search", "arguments": {"query": "verify_token", "cwd": d}}}),
        ];
        let mut replies = Vec::new();
        for m in msgs {
            writeln!(stdin, "{m}").unwrap();
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            replies.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        drop(stdin);
        child.wait().ok();
        replies
    })
    .await
    .unwrap();
    let tools: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(tools.contains(&"search"), "{tools:?}");
    assert_eq!(
        replies[2]["result"]["structuredContent"]["hits"][0]["path"], "src/auth.rs",
        "{}",
        replies[2]
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worktree_indexes_start_from_the_main_index_and_are_cleaned_up() {
    let env = Env::start(&[]).await;
    let dir = project(&env);
    env.op("index_project", json!({"cwd": dir})).await;
    // A worktree of the project (as background tasks and comparisons use).
    let wt = env.home.scratch("wt").join("w1");
    let out = std::process::Command::new("git")
        .args(["worktree", "add", "-q", "--detach"])
        .arg(&wt)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wt = std::fs::canonicalize(wt).unwrap();
    let before = env.op("index_status", json!({"cwd": wt})).await;
    // Seeded: the main project's chunks are there before any refresh.
    assert_eq!(before["files"], 3, "{before}");
    let after = env.op("index_project", json!({"cwd": wt})).await;
    assert_eq!(
        after["last_refresh"]["embedded"], 0,
        "unchanged files must not be embedded again: {after}"
    );
    assert_eq!(after["last_refresh"]["added"], 0);
    std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&wt)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!Path::new(&wt).exists());
    let removed = env.d().indexer.collect_garbage();
    assert_eq!(removed, 1);
    env.stop().await;
}
