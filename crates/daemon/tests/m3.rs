//! M3: agent, delegation, task runner, MCP and connecting clients – over the
//! real daemon with fake Hugging Face and fake llama-server processes.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ancilo_core::{Config, Paths};
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin, home::git_repo};
use serde_json::{Value, json};

struct Env {
    home: TestHome,
    _hf: FakeHf,
    d: Option<DaemonHandle>,
    config: Config,
}

fn repo(id: &str) -> FakeRepo {
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
            150_000,
        )],
    )
    .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 32768}))
}

fn ancilo_bin() -> PathBuf {
    // target/<profile>/ancilo, next to the fake-llama-server binary.
    fake_llama_server_bin().with_file_name("ancilo")
}

impl Env {
    async fn start(repos: Vec<FakeRepo>, script: &str, tasks: ancilo_tasks::Options) -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(repos).await;
        let s = home.scratch("script").join("script.yaml");
        std::fs::write(&s, script).unwrap();
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
            llama_server_env: [("FAKE_LLM_SCRIPT".to_string(), s.display().to_string())]
                .into_iter()
                .collect(),
            ancilo_bin: Some(ancilo_bin()),
            ..home.config()
        };
        home.write_config(&config);
        let d = ancilo_daemon::start(home.paths.clone(), config.clone(), options(tasks))
            .await
            .unwrap();
        Self {
            home,
            _hf: hf,
            d: Some(d),
            config,
        }
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

    async fn op_err(&self, name: &str, input: Value) -> Value {
        let r = reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{name}", self.d().url()))
            .bearer_auth(&self.d().token)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert!(!r.status().is_success());
        r.json().await.unwrap()
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

    async fn restart(&mut self, tasks: ancilo_tasks::Options) {
        self.d.take().unwrap().stop().await;
        self.d = Some(
            ancilo_daemon::start(self.home.paths.clone(), self.config.clone(), options(tasks))
                .await
                .unwrap(),
        );
    }

    async fn stop(mut self) {
        if let Some(d) = self.d.take() {
            d.stop().await;
        }
    }
}

fn options(tasks: ancilo_tasks::Options) -> DaemonOptions {
    DaemonOptions {
        manager: ManagerOptions {
            measure_speed: false,
            restart_backoff: Duration::from_millis(50),
            ..Default::default()
        },
        llama_build: Some(None),
        tasks,
    }
}

fn tasks_opts() -> ancilo_tasks::Options {
    ancilo_tasks::Options {
        shell: ancilo_agent::ShellSettings {
            sandbox: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn project(env: &Env) -> PathBuf {
    let dir = env.home.scratch("project");
    git_repo(
        &dir,
        &[
            ("src/lib.rs", "pub fn old_name() -> u32 {\n    42\n}\n"),
            ("README.md", "# Project\n"),
        ],
    );
    std::fs::canonicalize(dir).unwrap()
}

/// Script: read the file, edit it, answer.
const RENAME: &str = r#"
steps:
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "src/lib.rs" } }] }
  - expect: { any_message_contains: "pub fn old_name" }
    respond: { tool_calls: [{ name: edit_file, arguments: { path: "src/lib.rs", old_text: "old_name", new_text: "new_name" } }] }
  - respond: { text: "Renamed old_name to new_name in src/lib.rs." }
"#;

// covers: M3-AC-01, M3-AC-04
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synchronous_delegation_changes_files_and_returns_a_compact_result() {
    let env = Env::start(vec![repo("o/W-GGUF")], RENAME, tasks_opts()).await;
    env.add("o/W-GGUF").await;
    let dir = project(&env);
    let mut events = env.d().bus.subscribe();
    let v = env
        .op(
            "delegate",
            json!({"task": "Rename old_name to new_name", "cwd": dir, "kind": "refactor"}),
        )
        .await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["summary"], "Renamed old_name to new_name in src/lib.rs.");
    assert_eq!(v["changed_files"][0]["path"], "src/lib.rs");
    assert_eq!(v["diff_stat"], "+1 −1 in 1 file");
    assert!(v["diff"].is_null(), "the diff only on request");
    assert!(
        v.to_string().len() < 2000,
        "compact result: {} bytes",
        v.to_string().len()
    );
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("pub fn new_name()")
    );
    // The event sequence documents every step.
    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        if e.kind.starts_with("agent.") || e.kind.starts_with("task.") {
            kinds.push(e.kind);
        }
    }
    let expected = [
        "task.queued",
        "task.started",
        "agent.started",
        "agent.step",
        "agent.tool_called",
        "agent.tool_result",
        "agent.step",
        "agent.tool_called",
        "agent.file_changed",
        "agent.tool_result",
        "agent.step",
        "agent.message",
        "task.finished",
    ];
    assert_eq!(kinds, expected);
    let detail = env
        .op(
            "task_result",
            json!({"task_id": v["task_id"], "detail": true}),
        )
        .await;
    assert!(
        detail["diff"]
            .as_str()
            .unwrap()
            .contains("+pub fn new_name() -> u32 {")
    );
    env.stop().await;
}

// covers: M3-AC-05
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_tasks_work_on_their_own_branch() {
    let env = Env::start(vec![repo("o/W-GGUF")], RENAME, tasks_opts()).await;
    env.add("o/W-GGUF").await;
    let dir = project(&env);
    let v = env
        .op(
            "delegate",
            json!({"task": "Rename old_name to new_name", "cwd": dir, "background": true}),
        )
        .await;
    assert_eq!(v["status"], "queued");
    let branch = v["branch"].as_str().unwrap().to_string();
    assert!(branch.starts_with("ancilo/"));
    let done = env
        .op(
            "task_result",
            json!({"task_id": v["task_id"], "wait_s": 30}),
        )
        .await;
    assert_eq!(done["status"], "done", "{done}");
    assert!(done["commit"].is_string());
    assert!(
        done["next_step"]
            .as_str()
            .unwrap()
            .contains(&format!("git merge {branch}"))
    );
    // The project itself is untouched …
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("old_name")
    );
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty());
    // … and the branch carries the change.
    let show = std::process::Command::new("git")
        .args(["show", &format!("{branch}:src/lib.rs")])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&show.stdout).contains("new_name"));
    // Background work needs git when it may change files.
    let plain = env.home.scratch("plain");
    let err = env
        .op_err(
            "delegate",
            json!({"task": "x", "cwd": std::fs::canonicalize(&plain).unwrap(), "background": true}),
        )
        .await;
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("git repository")
    );
    env.stop().await;
}

// covers: M3-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_task_queue_survives_cancel_timeouts_and_restarts() {
    // Distinct calls (identical ones would trip the loop protection), 300 ms each.
    let slow: String = std::iter::once("steps:\n".to_string())
        .chain((0..60).map(|i| format!("  - respond: {{ tool_calls: [{{ name: glob, arguments: {{ pattern: \"*{i}.rs\" }} }}], delay_ms: 300 }}\n")))
        .collect();
    let slow = slow.as_str();
    let short = ancilo_tasks::Options {
        timeout: Duration::from_millis(900),
        ..tasks_opts()
    };
    let mut env = Env::start(vec![repo("o/W-GGUF")], slow, short).await;
    env.add("o/W-GGUF").await;
    let dir = project(&env);
    // Timeout → partial, changes kept.
    let v = env
        .op("delegate", json!({"task": "loop forever", "cwd": dir}))
        .await;
    assert_eq!(v["status"], "partial", "{v}");
    assert!(v["summary"].as_str().unwrap().contains("time limit"));
    // Cancel a running background task.
    env.restart(tasks_opts()).await;
    let v = env
        .op(
            "delegate",
            json!({"task": "loop forever", "cwd": dir, "background": true, "max_steps": 1000}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    env.op("cancel_task", json!({"task_id": v["task_id"]}))
        .await;
    let r = env
        .op(
            "task_result",
            json!({"task_id": v["task_id"], "wait_s": 10}),
        )
        .await;
    assert_eq!(r["status"], "cancelled", "{r}");
    // A background task interrupted by a restart continues afterwards.
    let v = env
        .op(
            "delegate",
            json!({"task": "loop", "cwd": dir, "background": true, "max_steps": 6}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    env.restart(tasks_opts()).await;
    let r = env
        .op(
            "task_result",
            json!({"task_id": v["task_id"], "wait_s": 30}),
        )
        .await;
    assert_eq!(
        r["status"], "partial",
        "resumed and ran to its step limit: {r}"
    );
    let list = env.op("list_tasks", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    env.stop().await;
}

fn mcp_session(home: &Path, messages: &[Value]) -> Vec<Value> {
    let mut child = std::process::Command::new(ancilo_bin())
        .arg("mcp")
        .env("ANCILO_HOME", home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut replies = Vec::new();
    for m in messages {
        writeln!(stdin, "{m}").unwrap();
        stdin.flush().unwrap();
        if m.get("id").is_some() {
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            replies.push(serde_json::from_str(&line).unwrap());
        }
    }
    drop(stdin);
    child.wait().unwrap();
    replies
}

// covers: M3-AC-08, M3-AC-14
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_over_stdio_delegates_and_reaches_every_operation() {
    let env = Env::start(vec![repo("o/W-GGUF")], RENAME, tasks_opts()).await;
    env.add("o/W-GGUF").await;
    let dir = project(&env);
    let home = env.home.path().to_path_buf();
    let names: Vec<String> = env
        .d()
        .gateway
        .manager()
        .list()
        .await
        .unwrap()
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(names.len(), 1);
    let ops: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/ops", env.d().url()))
        .bearer_auth(&env.d().token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Every operation – except the user's own (their log of what left this
    // computer), which a model never reaches (checked last).
    let op_names: Vec<String> = ops
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["own"] != true)
        .map(|o| o["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ops.as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "outbound_log" && o["own"] == true),
        "the log is the user's own"
    );
    let dir2 = dir.clone();
    let replies = tokio::task::spawn_blocking(move || {
        let mut msgs = vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "delegate", "arguments": {"task": "Rename old_name to new_name", "cwd": dir2}}}),
        ];
        for (i, op) in op_names.iter().filter(|o| *o != "daemon_shutdown").enumerate() {
            msgs.push(json!({"jsonrpc": "2.0", "id": 100 + i, "method": "tools/call", "params": {"name": "ancilo", "arguments": {"operation": op, "input": {}}}}));
        }
        msgs.push(json!({"jsonrpc": "2.0", "id": 99, "method": "tools/call", "params": {"name": "ancilo", "arguments": {"operation": "outbound_log", "input": {}}}}));
        mcp_session(&home, &msgs)
    })
    .await
    .unwrap();
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "ancilo");
    let tools: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"delegate") && tools.contains(&"task_result") && tools.contains(&"ancilo"),
        "{tools:?}"
    );
    assert_eq!(
        replies[2]["result"]["structuredContent"]["status"], "done",
        "{}",
        replies[2]
    );
    let (own, others) = replies[3..].split_last().unwrap();
    for r in others {
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(!text.contains("unknown operation"), "{r}");
    }
    assert!(
        own["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown operation"),
        "the log of what left this computer is not for a model: {own}"
    );
    env.stop().await;
}

// covers: M3-AC-09
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connecting_claude_code_and_codex_is_verified_and_reversible() {
    let env = Env::start(vec![], "fallback: { text: ok }\n", tasks_opts()).await;
    // Fake `claude`/`codex` that record what they are asked to do.
    let bin = env.home.scratch("bin");
    let state = env.home.scratch("state");
    let log = |name: &str| state.join(format!("{name}.log"));
    for name in ["claude", "codex"] {
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$*\" in *\" list\"*) echo ancilo ;; esac\n",
            log(name).display()
        );
        let p = bin.join(name);
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut env = env;
    env.config.claude_bin = Some(bin.join("claude"));
    env.config.codex_bin = Some(bin.join("codex"));
    env.config.codex_home = Some(env.home.scratch("codex-home"));
    env.home.write_config(&env.config);
    env.restart(tasks_opts()).await;
    let r = env.op("connect_claude_code", json!({})).await;
    assert_eq!(r["connected"], true, "{r}");
    assert_eq!(
        r["verified"], true,
        "a real MCP handshake through `ancilo mcp`: {r}"
    );
    let calls = std::fs::read_to_string(log("claude")).unwrap();
    assert!(calls.contains("plugin install ancilo@ancilo-local"));
    let r = env.op("connect_codex", json!({})).await;
    assert_eq!(r["verified"], true, "{r}");
    let codex_calls = std::fs::read_to_string(log("codex")).unwrap();
    assert!(
        codex_calls.contains(&format!(
            "mcp add ancilo --env ANCILO_HOME={} -- {} mcp",
            env.home.path().display(),
            ancilo_bin().display()
        )),
        "{codex_calls}"
    );
    // Claude Code's plugin starts the MCP server for this home, too.
    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            env.home
                .path()
                .join("clients/claude-plugins/ancilo/.mcp.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mcp["mcpServers"]["ancilo"]["env"]["ANCILO_HOME"],
        env.home.path().display().to_string()
    );
    env.op("disconnect_claude_code", json!({})).await;
    env.op("disconnect_codex", json!({})).await;
    assert!(!env.home.path().join("clients/claude-plugins").exists());
    assert!(
        !env.config
            .codex_home
            .clone()
            .unwrap()
            .join("AGENTS.md")
            .exists()
    );
    // Connecting needs confirmation from MCP/assistant callers.
    let err = env.op_err("connect_codex", json!({})).await;
    assert_eq!(err["error"]["code"], "confirmation_required");
    let _ = Paths::from_home(env.home.path());
    env.stop().await;
}

// covers: M3-AC-15
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callers_choose_the_model_per_task() {
    let env = Env::start(
        vec![repo("o/Big-GGUF"), repo("o/Small-GGUF")],
        "fallback: { text: \"done\" }\n",
        tasks_opts(),
    )
    .await;
    let big = env.add("o/Big-GGUF").await;
    let small = env.add("o/Small-GGUF").await;
    let dir = project(&env);
    let v = env.op("delegate", json!({"task": "summarise README.md", "cwd": dir, "allow": "read", "model": small, "kind": "summary"})).await;
    assert_eq!(v["model"], small.as_str());
    let v = env
        .op(
            "delegate",
            json!({"task": "summarise README.md", "cwd": dir, "allow": "read"}),
        )
        .await;
    assert_eq!(
        v["model"],
        big.as_str(),
        "without a choice: role delegation → default model"
    );
    env.op("assign_role", json!({"role": "delegation", "model": small}))
        .await;
    let v = env
        .op(
            "delegate",
            json!({"task": "summarise", "cwd": dir, "allow": "read"}),
        )
        .await;
    assert_eq!(v["model"], small.as_str(), "role delegation is used");
    let stats = env.op("gateway_stats", json!({"model": small})).await;
    assert_eq!(stats["requests"], 2);
    env.stop().await;
}

// covers: M3-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_stuck_in_a_loop_is_stopped() {
    let stuck = "fallback: { tool_calls: [{ name: glob, arguments: { pattern: \"*.rs\" } }] }\n";
    let env = Env::start(vec![repo("o/W-GGUF")], stuck, tasks_opts()).await;
    env.add("o/W-GGUF").await;
    let dir = project(&env);
    let v = env
        .op(
            "delegate",
            json!({"task": "find files", "cwd": dir, "allow": "read"}),
        )
        .await;
    assert_eq!(v["status"], "failed", "{v}");
    assert!(v["summary"].as_str().unwrap().contains("repeating"), "{v}");
    assert!(v["steps"].as_u64().unwrap() < 10, "stopped early: {v}");
    env.stop().await;
}
