//! M8: coding sessions in the app – approvals, persistence, "retry with
//! another model", changes that reach the project only when applied, and
//! terminals – over the real daemon with scripted fake models.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin, home::git_repo};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

struct Env {
    home: TestHome,
    _hf: FakeHf,
    config: Config,
    scripts: PathBuf,
    d: Option<DaemonHandle>,
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

fn options() -> DaemonOptions {
    DaemonOptions {
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
    }
}

impl Env {
    /// Two chat models, `chat-q8_0` and `other-q8_0`; scripts per model file
    /// stem (`Chat-Q8_0`, `Other-Q8_0`).
    async fn start(scripts: &[(&str, &str)]) -> (Self, String, String) {
        let home = TestHome::new();
        let hf = FakeHf::start(vec![repo("o/Chat-GGUF"), repo("o/Other-GGUF")]).await;
        let dir = home.scratch("scripts");
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
            ..home.config()
        };
        home.write_config(&config);
        let mut env = Self {
            home,
            _hf: hf,
            config,
            scripts: dir,
            d: None,
        };
        env.set_scripts(scripts);
        env.d = Some(
            ancilo_daemon::start(env.home.paths.clone(), env.config.clone(), options())
                .await
                .unwrap(),
        );
        let chat = env.add("o/Chat-GGUF").await;
        let other = env.add("o/Other-GGUF").await;
        (env, chat, other)
    }

    /// Scripts are read when a fake model server starts.
    fn set_scripts(&self, scripts: &[(&str, &str)]) {
        for (stem, script) in scripts {
            std::fs::write(self.scripts.join(format!("{stem}.yaml")), script).unwrap();
        }
    }

    fn d(&self) -> &DaemonHandle {
        self.d.as_ref().unwrap()
    }

    async fn call(&self, name: &str, input: Value, confirm: bool) -> (bool, Value) {
        let mut r = reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{name}", self.d().url()))
            .bearer_auth(&self.d().token)
            .json(&input);
        if confirm {
            r = r.header("x-ancilo-confirm", "true");
        }
        let r = r.send().await.unwrap();
        let ok = r.status().is_success();
        (ok, r.json().await.unwrap_or(Value::Null))
    }

    async fn op(&self, name: &str, input: Value) -> Value {
        let (ok, v) = self.call(name, input, true).await;
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

    async fn session(&self, id: &str) -> Value {
        self.op("get_session", json!({"session": id})).await
    }

    /// Waits until `f` holds for the session.
    async fn wait_for(&self, id: &str, what: &str, f: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..600 {
            let s = self.session(id).await;
            if f(&s) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{what}: {}", self.session(id).await);
    }

    async fn next_approval(&self, id: &str) -> Value {
        let s = self
            .wait_for(id, "no approval requested", |s| {
                !s["approvals"].as_array().unwrap().is_empty()
            })
            .await;
        s["approvals"][0].clone()
    }

    async fn idle(&self, id: &str) -> Value {
        self.wait_for(id, "turn did not finish", |s| s["status"] != "running")
            .await
    }

    async fn restart(&mut self) {
        self.d.take().unwrap().stop().await;
        self.d = Some(
            ancilo_daemon::start(self.home.paths.clone(), self.config.clone(), options())
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

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn git_status(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn files(v: &Value) -> Vec<String> {
    let mut f: Vec<String> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["path"].as_str().unwrap().to_string())
        .collect();
    f.sort();
    f
}

const GATED: &str = r#"
steps:
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "src/lib.rs" } }] }
  - expect: { any_message_contains: "pub fn old_name" }
    respond: { tool_calls: [{ name: write_file, arguments: { path: "NOTES.md", content: "notes\n" } }] }
  - expect: { any_message_contains: "did not allow" }
    respond: { tool_calls: [{ name: edit_file, arguments: { path: "src/lib.rs", old_text: "old_name", new_text: "new_name" } }] }
  - respond: { tool_calls: [{ name: bash, arguments: { command: "echo built > out.txt" } }] }
  - respond: { text: "Renamed old_name to new_name and wrote out.txt." }
"#;

// covers: M8-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approvals_gate_every_action_above_the_permission() {
    let (env, _, _) = Env::start(&[("Chat-Q8_0", GATED)]).await;
    let dir = project(&env);
    let mut events = env.d().bus.subscribe();
    let s = env
        .op("create_session", json!({"cwd": dir, "permission": "read"}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["isolated"], true);
    assert_eq!(s["model"], "chat-q8_0");
    env.op(
        "send_message",
        json!({"session": id, "text": "Rename old_name"}),
    )
    .await;

    // Writing a file needs "edit": the agent waits; nothing is written.
    let a = env.next_approval(&id).await;
    // The request shows while the turn runs (it joins the history at its end).
    let running = env.session(&id).await;
    assert_eq!(
        running["messages"].as_array().unwrap().last().unwrap()["text"],
        "Rename old_name"
    );
    assert_eq!(a["tool"], "write_file");
    assert_eq!(a["needs"], "edit");
    assert_eq!(env.session(&id).await["status"], "running");
    assert!(files(&env.session(&id).await["changes"]).is_empty());
    env.op("reject", json!({"approval": a["id"]})).await;

    // Editing: allowed once.
    let a = env
        .wait_for(&id, "no edit approval", |s| {
            s["approvals"][0]["tool"] == "edit_file"
        })
        .await["approvals"][0]
        .clone();
    assert!(files(&env.session(&id).await["changes"]).is_empty());
    env.op("approve", json!({"approval": a["id"]})).await;

    // A command needs "shell"; allowed for the rest of the session.
    let a = env
        .wait_for(&id, "no shell approval", |s| {
            s["approvals"][0]["tool"] == "bash"
        })
        .await["approvals"][0]
        .clone();
    assert_eq!(a["needs"], "shell");
    assert!(!dir.join("out.txt").exists());
    env.op("approve", json!({"approval": a["id"], "remember": true}))
        .await;

    let s = env.idle(&id).await;
    assert_eq!(s["status"], "idle");
    let asked = s["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user" && m["text"] == "Rename old_name")
        .count();
    assert_eq!(asked, 1, "the request once, not twice");
    assert_eq!(s["permission"], "shell", "remembered for the session");
    assert_eq!(files(&s["changes"]), ["out.txt", "src/lib.rs"]);
    // Approved actions worked in the session's own area – the project is untouched.
    assert!(read(&dir.join("src/lib.rs")).contains("old_name"));
    assert!(!dir.join("NOTES.md").exists() && !dir.join("out.txt").exists());
    assert_eq!(git_status(&dir), "");

    // Deciding twice or on an unknown approval fails.
    let (ok, _) = env
        .call("approve", json!({"approval": a["id"]}), true)
        .await;
    assert!(!ok);

    // Applying is consequential: it needs the confirmation.
    let (ok, _) = env
        .call("apply_changes", json!({"session": id}), false)
        .await;
    assert!(!ok);
    assert_eq!(git_status(&dir), "");
    let applied = env.op("apply_changes", json!({"session": id})).await;
    assert_eq!(
        files(&json!(
            applied["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| json!({"path": f}))
                .collect::<Vec<_>>()
        )),
        ["out.txt", "src/lib.rs"]
    );
    assert!(read(&dir.join("src/lib.rs")).contains("pub fn new_name()"));
    assert_eq!(read(&dir.join("out.txt")).trim(), "built");
    assert!(!dir.join("NOTES.md").exists());
    assert!(files(&env.session(&id).await["changes"]).is_empty());

    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        kinds.push(e.kind);
    }
    assert_eq!(
        kinds
            .iter()
            .filter(|k| *k == "session.approval_required")
            .count(),
        3
    );
    for k in [
        "session.created",
        "session.turn_started",
        "session.rejected",
        "session.approved",
        "session.changes_ready",
        "session.turn_finished",
        "session.applied",
    ] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing: {kinds:?}");
    }
    env.stop().await;
}

// covers: M8-AC-06
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_survive_a_daemon_restart() {
    let first = r#"
steps:
  - respond: { tool_calls: [{ name: edit_file, arguments: { path: "src/lib.rs", old_text: "old_name", new_text: "new_name" } }] }
  - respond: { text: "Renamed old_name to new_name." }
  - respond: { text: "never", delay_ms: 30000 }
"#;
    let (mut env, _, _) = Env::start(&[("Chat-Q8_0", first)]).await;
    let dir = project(&env);
    let s = env.op("create_session", json!({"cwd": dir})).await;
    let id = s["id"].as_str().unwrap().to_string();
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Rename old_name", "wait": true}),
        )
        .await;
    assert_eq!(s["title"], "Rename old_name");
    assert_eq!(files(&s["changes"]), ["src/lib.rs"]);
    // A turn that is still running when the daemon stops.
    env.op(
        "send_message",
        json!({"session": id, "text": "Now document it"}),
    )
    .await;
    env.wait_for(&id, "not running", |s| s["status"] == "running")
        .await;
    env.set_scripts(&[(
        "Chat-Q8_0",
        r#"
steps:
  - expect: { any_message_contains: "Renamed old_name to new_name." }
    respond: { text: "Continuing where we left off." }
"#,
    )]);
    env.restart().await;

    let s = env.session(&id).await;
    assert_eq!(s["status"], "interrupted");
    assert_eq!(s["turns"], 2);
    assert_eq!(files(&s["changes"]), ["src/lib.rs"], "open changes kept");
    let texts: Vec<&str> = s["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert!(texts.contains(&"Rename old_name"), "{texts:?}");
    assert!(
        texts.contains(&"Renamed old_name to new_name."),
        "{texts:?}"
    );
    let listed = env.op("list_sessions", json!({"project": dir})).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    // The conversation continues with its history.
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Go on", "wait": true}),
        )
        .await;
    assert_eq!(s["status"], "idle");
    assert_eq!(
        s["messages"].as_array().unwrap().last().unwrap()["text"],
        "Continuing where we left off."
    );
    // The change is still only in the session's area.
    assert!(read(&dir.join("src/lib.rs")).contains("old_name"));
    env.stop().await;
}

// covers: M8-AC-05, M8-AC-02
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_with_another_model_runs_side_by_side_and_discarding_leaves_no_trace() {
    let chat = r#"
steps:
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "src/lib.rs" } }] }
  - expect: { any_message_contains: "pub fn old_name" }
    respond: { tool_calls: [{ name: edit_file, arguments: { path: "src/lib.rs", old_text: "old_name", new_text: "chat_name" } }] }
  - respond: { text: "Renamed to chat_name." }
"#;
    // The other model starts from the same state: it sees old_name too.
    let other = r#"
steps:
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "src/lib.rs" } }] }
  - expect: { any_message_contains: "pub fn old_name" }
    respond: { tool_calls: [{ name: edit_file, arguments: { path: "src/lib.rs", old_text: "old_name", new_text: "other_name" } }] }
  - respond: { text: "Renamed to other_name." }
  - expect: { any_message_contains: "Renamed to other_name." }
    respond: { tool_calls: [{ name: write_file, arguments: { path: "src/extra.rs", content: "// extra\n" } }] }
  - respond: { text: "Added src/extra.rs." }
"#;
    let (env, chat_id, other_id) = Env::start(&[("Chat-Q8_0", chat), ("Other-Q8_0", other)]).await;
    let dir = project(&env);
    let s = env
        .op("create_session", json!({"cwd": dir, "model": chat_id}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["can_retry"], false);
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Rename old_name", "wait": true}),
        )
        .await;
    assert_eq!(s["can_retry"], true);
    let s = env
        .op(
            "retry_with_model",
            json!({"session": id, "model": other_id, "wait": true}),
        )
        .await;
    let v = &s["variants"][0];
    assert_eq!(v["model"], other_id.as_str());
    assert_eq!(v["status"], "idle");
    assert_eq!(v["summary"], "Renamed to other_name.");
    // Both results side by side, in separate areas; the project is untouched.
    let mine = env.op("session_diff", json!({"session": id})).await;
    assert!(
        mine["patch"]
            .as_str()
            .unwrap()
            .contains("+pub fn chat_name()")
    );
    let theirs = env
        .op("session_diff", json!({"session": id, "variant": v["id"]}))
        .await;
    let patch = theirs["patch"].as_str().unwrap();
    assert!(patch.contains("+pub fn other_name()") && !patch.contains("chat_name"));
    assert_eq!(git_status(&dir), "");

    // The other model's result is taken; the session continues from it.
    env.op("apply_changes", json!({"session": id, "variant": v["id"]}))
        .await;
    assert!(read(&dir.join("src/lib.rs")).contains("pub fn other_name()"));
    let s = env.session(&id).await;
    assert!(s["variants"].as_array().unwrap().is_empty());
    assert!(files(&s["changes"]).is_empty());
    let msgs = s["messages"].as_array().unwrap();
    assert_eq!(msgs.last().unwrap()["text"], "Renamed to other_name.");
    // The choice counts for the leaderboard – as the user's (subjective) choice.
    let board = env.op("leaderboard", json!({})).await;
    assert_eq!(
        board["choices"],
        json!([
            {"model": other_id, "chosen": 1, "passed_over": 0},
            {"model": chat_id, "chosen": 0, "passed_over": 1}
        ])
    );
    assert!(
        board["markdown"]
            .as_str()
            .unwrap()
            .contains("Your choices in coding sessions")
    );
    assert!(!msgs.iter().any(|m| m["text"] == "Renamed to chat_name."));

    // Discarded changes leave no trace – in the project and in the session.
    let before = git_status(&dir);
    env.op("update_session", json!({"session": id, "model": other_id}))
        .await;
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Add an extra module", "wait": true}),
        )
        .await;
    assert_eq!(files(&s["changes"]), ["src/extra.rs"]);
    env.op("discard_changes", json!({"session": id})).await;
    assert!(files(&env.session(&id).await["changes"]).is_empty());
    assert!(!dir.join("src/extra.rs").exists());
    assert_eq!(git_status(&dir), before);

    // Deleting removes the session's work area.
    let (ok, _) = env
        .call("delete_session", json!({"session": id}), false)
        .await;
    assert!(!ok, "deleting is consequential");
    env.op("delete_session", json!({"session": id})).await;
    assert!(!env.home.paths.home().join("sessions").join(&id).exists());
    let worktrees = std::process::Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&worktrees.stdout).lines().count(),
        1
    );
    env.stop().await;
}

// covers: M8-AC-02
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projects_without_git_are_protected_too() {
    let script = r#"
steps:
  - respond: { tool_calls: [{ name: write_file, arguments: { path: "notes.txt", content: "new\n" } }] }
  - respond: { text: "Wrote notes.txt." }
  - respond: { tool_calls: [{ name: edit_file, arguments: { path: "a.txt", old_text: "one", new_text: "two" } }] }
  - respond: { text: "Changed a.txt." }
  - respond: { tool_calls: [{ name: edit_file, arguments: { path: "a.txt", old_text: "one", new_text: "three" } }] }
  - respond: { text: "Changed a.txt differently." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let dir = env.home.scratch("plain");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let p = env.op("open_project", json!({"path": dir})).await;
    assert_eq!(p["git"], false);
    let s = env.op("create_session", json!({"cwd": dir})).await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["isolated"], true);
    env.op(
        "send_message",
        json!({"session": id, "text": "Write notes", "wait": true}),
    )
    .await;
    // Without git the agent works in a copy of its own, too.
    assert_eq!(files(&env.session(&id).await["changes"]), ["notes.txt"]);
    assert!(!dir.join("notes.txt").exists());
    // The user keeps working on the project; discarding never touches it.
    std::fs::write(dir.join("mine.txt"), "mine\n").unwrap();
    env.op("discard_changes", json!({"session": id})).await;
    assert_eq!(read(&dir.join("mine.txt")), "mine\n");
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Change a", "wait": true}),
        )
        .await;
    assert_eq!(files(&s["changes"]), ["a.txt"]);
    assert_eq!(read(&dir.join("a.txt")), "one\n");
    // Retrying works without git as well.
    assert_eq!(s["can_retry"], true);
    let s = env
        .op(
            "retry_with_model",
            json!({"session": id, "model": "chat-q8_0", "wait": true}),
        )
        .await;
    let v = s["variants"][0]["id"].as_str().unwrap().to_string();
    env.op("apply_changes", json!({"session": id, "variant": v}))
        .await;
    assert_eq!(read(&dir.join("a.txt")), "three\n");
    assert!(files(&env.session(&id).await["changes"]).is_empty());
    // No shadow data inside the project.
    let mut entries: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, ["a.txt", "mine.txt"]);
    env.stop().await;
}

// covers: M8-AC-05
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_retries_never_race_the_session() {
    let chat = r#"
steps:
  - respond: { tool_calls: [{ name: edit_file, arguments: { path: "src/lib.rs", old_text: "old_name", new_text: "chat_name" } }] }
  - respond: { text: "Renamed." }
fallback: { text: "ok" }
"#;
    // The other model is slow: its retry is still running when the user acts.
    let other = r#"
steps:
  - respond: { text: "thinking", delay_ms: 20000 }
fallback: { text: "late", delay_ms: 20000 }
"#;
    let (env, chat_id, other_id) = Env::start(&[("Chat-Q8_0", chat), ("Other-Q8_0", other)]).await;
    let dir = project(&env);
    let s = env
        .op("create_session", json!({"cwd": dir, "model": chat_id}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    env.op(
        "send_message",
        json!({"session": id, "text": "Rename", "wait": true}),
    )
    .await;
    let s = env
        .op(
            "retry_with_model",
            json!({"session": id, "model": other_id}),
        )
        .await;
    let v = s["variants"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(s["variants"][0]["status"], "running");
    // A running retry cannot be taken.
    let (ok, e) = env
        .call("apply_changes", json!({"session": id, "variant": v}), true)
        .await;
    assert!(
        !ok && e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("still running"),
        "{e}"
    );
    // Applying the session's own changes stops the retry first – cleanly.
    env.op("apply_changes", json!({"session": id})).await;
    assert!(read(&dir.join("src/lib.rs")).contains("chat_name"));
    let s = env.session(&id).await;
    assert!(
        s["variants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["status"] != "running"),
        "{s}"
    );
    // Deleting while a retry runs stops it and leaves no work area behind.
    env.op(
        "retry_with_model",
        json!({"session": id, "model": other_id}),
    )
    .await;
    env.op("delete_session", json!({"session": id})).await;
    assert!(!env.home.paths.home().join("sessions").join(&id).exists());
    let worktrees = std::process::Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&worktrees.stdout).lines().count(),
        1
    );
    env.stop().await;
}

// covers: M8-AC-08, M8-AC-04
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_session_action_is_an_operation_and_terminals_need_a_ticket() {
    let (env, _, _) = Env::start(&[]).await;
    let dir = project(&env);
    let ops: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/ops", env.d().url()))
        .bearer_auth(&env.d().token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = ops
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    for op in [
        "open_project",
        "create_session",
        "send_message",
        "approve",
        "reject",
        "get_session",
        "list_sessions",
        "session_diff",
        "apply_changes",
        "discard_changes",
        "retry_with_model",
        "cancel_turn",
        "update_session",
        "delete_session",
        "open_terminal",
        "terminal_ticket",
        "list_terminals",
        "close_terminal",
    ] {
        assert!(names.contains(&op), "{op} missing");
    }

    // A terminal lives in the daemon; connections need a one-time ticket.
    let t = env
        .op("open_terminal", json!({"cwd": dir, "cols": 80, "rows": 24}))
        .await;
    let term = t["terminal"]["id"].as_str().unwrap().to_string();
    let ws_url = |path: &str| format!("{}{}", env.d().url().replace("http://", "ws://"), path);
    let bad =
        tokio_tungstenite::connect_async(ws_url(&format!("/api/v1/pty/{term}?ticket=nope"))).await;
    assert!(bad.is_err(), "an invalid ticket is refused");
    let path = t["ticket"]["path"].as_str().unwrap().to_string();
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&path))
        .await
        .unwrap();
    assert!(
        tokio_tungstenite::connect_async(ws_url(&path))
            .await
            .is_err(),
        "tickets are single use"
    );
    ws.send(Message::Text(r#"{"resize":[120,40]}"#.into()))
        .await
        .unwrap();
    ws.send(Message::Binary(
        b"stty size; echo marker-$((6*7))\r".to_vec().into(),
    ))
    .await
    .unwrap();
    let mut seen = String::new();
    let got = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(Ok(m)) = ws.next().await {
            if let Message::Binary(b) = m {
                seen.push_str(&String::from_utf8_lossy(&b));
                if seen.contains("marker-42") && seen.contains("40 120") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(got, "terminal output: {seen}");
    drop(ws);

    // Reconnecting (e.g. after reloading the window) shows what happened.
    let ticket = env.op("terminal_ticket", json!({"terminal": term})).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(ticket["path"].as_str().unwrap()))
        .await
        .unwrap();
    let backlog = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&backlog.into_data()).contains("marker-42"));

    // A session's terminal opens in the project folder – where the user works
    // (not in the agent's hidden work area).
    let s = env.op("create_session", json!({"cwd": dir})).await;
    let t2 = env.op("open_terminal", json!({"session": s["id"]})).await;
    assert_eq!(t2["terminal"]["cwd"], s["project"]);
    assert_ne!(s["workdir"], s["project"], "isolated work area");
    assert_eq!(
        env.op("list_terminals", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    env.op("close_terminal", json!({"terminal": term})).await;
    env.op("close_terminal", json!({"terminal": t2["terminal"]["id"]}))
        .await;
    assert!(
        env.op("list_terminals", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    env.stop().await;
}

// covers: M8-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_coding_eval_runs_tasks_through_sessions_and_judges_the_project() {
    let script = r#"
steps:
  - expect: { last_user_contains: "hello.txt" }
    respond: { tool_calls: [{ name: write_file, arguments: { path: "hello.txt", content: "hi\n" } }] }
  - respond: { text: "Created hello.txt." }
  - expect: { last_user_contains: "Now say bye" }
    respond: { tool_calls: [{ name: edit_file, arguments: { path: "hello.txt", old_text: "hi", new_text: "bye" } }] }
  - respond: { text: "Changed it to bye." }
  - respond: { text: "I won't do that." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let suite = env.home.scratch("suite").join("coding.yaml");
    std::fs::write(
        &suite,
        r#"
name: mini-coding
tasks:
  - id: two-turns
    files: { "README.md": "x\n" }
    turns: ["Create hello.txt with hi", "Now say bye instead"]
    checks:
      - file_contains: { path: hello.txt, text: "bye" }
      - summary_contains: "bye"
  - id: refused
    files: { "README.md": "x\n" }
    turns: ["Create other.txt"]
    checks:
      - file_contains: { path: other.txt, text: "x" }
"#,
    )
    .unwrap();
    let r = env
        .op(
            "run_coding_eval",
            json!({"suite": suite, "model": "chat-q8_0", "repeat": 1}),
        )
        .await;
    assert_eq!(r["model"], "chat-q8_0");
    assert_eq!(r["tasks"][0]["success_rate"], 1.0, "{r}");
    assert_eq!(r["tasks"][0]["runs"][0]["steps"], 2, "two turns");
    assert_eq!(r["tasks"][1]["success_rate"], 0.0);
    assert!(
        r["tasks"][1]["runs"][0]["failure"]
            .as_str()
            .unwrap()
            .contains("other.txt missing")
    );
    assert_eq!(r["success_rate"], 0.5);
    // Sessions of the eval are cleaned up; the report is kept.
    assert!(
        env.op("list_sessions", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let saved = std::fs::read_dir(env.home.paths.home().join("evals"))
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("mini-coding-chat-q8_0")
        });
    assert!(saved);
    env.stop().await;
}

// covers: M8-AC-09
/// The agent works in the session's copy but only ever sees the project's
/// path: absolute paths and commands with it reach the copy, never the
/// project itself, and the work area's path never reaches the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agent_works_under_the_project_path() {
    let (env, _, _) = Env::start(&[]).await;
    let dir = project(&env);
    let root = dir.display().to_string();
    let script = format!(
        r#"
steps:
  - expect: {{ any_message_contains: "Project root: {root} ", no_message_contains: "/sessions/" }}
    respond: {{ tool_calls: [{{ name: write_file, arguments: {{ path: "{root}/ancilo.txt", content: "Hallo Ancilo!\n" }} }}] }}
  - expect: {{ no_message_contains: "/sessions/" }}
    respond: {{ tool_calls: [{{ name: bash, arguments: {{ command: "cd {root} && pwd && cat ancilo.txt" }} }}] }}
  - expect: {{ any_message_contains: "Hallo Ancilo!", no_message_contains: "/sessions/" }}
    respond: {{ text: "Created ancilo.txt in {root}." }}
"#
    );
    env.set_scripts(&[("Chat-Q8_0", &script)]);
    let s = env
        .op("create_session", json!({"cwd": dir, "permission": "shell"}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Create ancilo.txt", "wait": true}),
        )
        .await;
    let last = s["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        last["text"],
        format!("Created ancilo.txt in {root}."),
        "{s}"
    );
    assert_eq!(files(&s["changes"]), ["ancilo.txt"]);
    // Nothing reached the project before applying.
    assert!(!dir.join("ancilo.txt").exists());
    env.op("apply_changes", json!({"session": id})).await;
    assert_eq!(read(&dir.join("ancilo.txt")), "Hallo Ancilo!\n");
    env.stop().await;
}

// covers: M8-AC-10
/// The app's project list: opened projects stay listed even without
/// sessions; removing one removes its sessions, never the folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projects_are_listed_and_removed() {
    let (env, _, _) = Env::start(&[]).await;
    let dir = project(&env);
    let other = env.home.scratch("other");
    assert!(
        env.op("list_projects", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    env.op("open_project", json!({"path": dir})).await;
    env.op("open_project", json!({"path": other})).await;
    let list = env.op("list_projects", json!({})).await;
    let roots: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["root"].as_str().unwrap())
        .collect();
    let other = std::fs::canonicalize(other).unwrap();
    assert_eq!(
        roots,
        [other.to_str().unwrap(), dir.to_str().unwrap()],
        "most recent first"
    );
    assert_eq!(list[1]["sessions"], 0);
    assert_eq!(list[1]["name"], "project");
    assert_eq!(list[1]["exists"], true);
    let s = env.op("create_session", json!({"cwd": dir})).await;
    let list = env.op("list_projects", json!({})).await;
    assert_eq!(
        list[0]["root"],
        dir.to_str().unwrap(),
        "a new session makes it the latest"
    );
    assert_eq!(list[0]["sessions"], 1);
    // Removing is consequential and takes the sessions with it.
    let (ok, _) = env
        .call("remove_project", json!({"path": dir}), false)
        .await;
    assert!(!ok);
    env.op("remove_project", json!({"path": dir})).await;
    let list = env.op("list_projects", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (ok, _) = env
        .call("get_session", json!({"session": s["id"]}), false)
        .await;
    assert!(!ok);
    assert!(dir.join("README.md").exists(), "the folder stays");
    env.stop().await;
}

// covers: M8-AC-11
/// Something new to build: a name is all it takes – Ancilo creates the
/// folder (with git, so changes can be reviewed and undone) and lists it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_project_needs_only_a_name() {
    let (env, _, _) = Env::start(&[]).await;
    let p = env
        .op("create_project", json!({"name": "Meine Rezepte/Webseite"}))
        .await;
    let root = PathBuf::from(p["root"].as_str().unwrap());
    assert_eq!(p["name"], "Meine Rezepte-Webseite");
    assert_eq!(p["git"], true);
    assert!(root.join("README.md").exists());
    assert_eq!(git_status(&root), "", "a clean start");
    // The same name again gets its own folder.
    let again = env
        .op("create_project", json!({"name": "Meine Rezepte/Webseite"}))
        .await;
    assert_eq!(again["name"], "Meine Rezepte-Webseite 2");
    let listed = env.op("list_projects", json!({})).await;
    assert_eq!(listed.as_array().unwrap().len(), 2);
    // A session works in it right away.
    let s = env.op("create_session", json!({"cwd": root})).await;
    assert_eq!(s["isolated"], true);
    let (ok, _) = env
        .call("create_project", json!({"name": " / "}), true)
        .await;
    assert!(!ok, "a name is needed");
    // The folder it goes into can be chosen (like Codex) – it must exist.
    let place = env.home.scratch("my-www");
    let p = env
        .op(
            "create_project",
            json!({"name": "Vogelquiz", "parent": place}),
        )
        .await;
    assert_eq!(
        p["root"],
        place
            .canonicalize()
            .unwrap()
            .join("Vogelquiz")
            .display()
            .to_string()
    );
    assert!(place.join("Vogelquiz/README.md").is_file());
    let (ok, _) = env
        .call(
            "create_project",
            json!({"name": "X", "parent": "relative/path"}),
            true,
        )
        .await;
    assert!(!ok, "only an existing absolute folder");
    env.stop().await;
}

// covers: M8-AC-12
/// Projects and their sessions can be renamed and put in an order by hand;
/// new ones come first, the folder itself is never renamed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn projects_and_sessions_are_renamed_and_ordered_by_hand() {
    let (env, _, _) = Env::start(&[]).await;
    let a = env.op("create_project", json!({"name": "Alpha"})).await;
    let b = env.op("create_project", json!({"name": "Beta"})).await;
    let names = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(&env.op("list_projects", json!({})).await),
        ["Beta", "Alpha"]
    );
    // By hand: Alpha first.
    let ordered = env
        .op("reorder_projects", json!({"paths": [a["root"], b["root"]]}))
        .await;
    assert_eq!(names(&ordered), ["Alpha", "Beta"]);
    // A new project comes first; the order set by hand stays below.
    env.op("create_project", json!({"name": "Gamma"})).await;
    assert_eq!(
        names(&env.op("list_projects", json!({})).await),
        ["Gamma", "Alpha", "Beta"]
    );
    // Renamed in the list – the folder stays.
    let renamed = env
        .op(
            "rename_project",
            json!({"path": a["root"], "name": "Meine Webseite"}),
        )
        .await;
    assert_eq!(renamed["name"], "Meine Webseite");
    assert!(PathBuf::from(a["root"].as_str().unwrap()).ends_with("Alpha"));
    let back = env
        .op("rename_project", json!({"path": a["root"], "name": " "}))
        .await;
    assert_eq!(back["name"], "Alpha", "empty: the folder's name again");

    // Sessions: renamed with update_session, ordered by hand.
    let s1 = env.op("create_session", json!({"cwd": a["root"]})).await;
    let s2 = env.op("create_session", json!({"cwd": a["root"]})).await;
    let order = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    };
    let listed = env.op("list_sessions", json!({"project": a["root"]})).await;
    assert_eq!(
        order(&listed)[0],
        s2["id"].as_str().unwrap(),
        "newest first"
    );
    env.op(
        "update_session",
        json!({"session": s1["id"], "title": "Startseite bauen"}),
    )
    .await;
    env.op(
        "reorder_sessions",
        json!({"sessions": [s1["id"], s2["id"]]}),
    )
    .await;
    let listed = env.op("list_sessions", json!({"project": a["root"]})).await;
    assert_eq!(
        order(&listed),
        [s1["id"].as_str().unwrap(), s2["id"].as_str().unwrap()]
    );
    assert_eq!(listed[0]["title"], "Startseite bauen");
    let (ok, _) = env
        .call("reorder_sessions", json!({"sessions": ["s-nope"]}), true)
        .await;
    assert!(!ok);
    env.stop().await;
}

// covers: M8-AC-03
/// A turn whose model call fails says why in the chat – it must never look
/// as if nothing happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_turn_says_why() {
    let script = r#"
fallback: { http_error: { status: 400, message: "the model refused the request" } }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let dir = project(&env);
    let s = env.op("create_session", json!({"cwd": dir})).await;
    let id = s["id"].as_str().unwrap().to_string();
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Fix the quiz facts", "wait": true}),
        )
        .await;
    let messages = s["messages"].as_array().unwrap();
    let texts: Vec<&str> = messages.iter().filter_map(|m| m["text"].as_str()).collect();
    assert_eq!(
        texts.iter().filter(|t| **t == "Fix the quiz facts").count(),
        1,
        "the request shows once: {s}"
    );
    let last = texts.last().unwrap();
    assert!(last.starts_with("(failed: model call failed"), "{s}");
    assert!(last.contains("the model refused the request"), "{s}");
    assert_eq!(s["status"], "idle");
    env.stop().await;
}

// covers: M10-AC-04, M10-AC-05
/// A task: the agent works with documents in a copy of the folder; the folder
/// changes only when the user keeps the changes – and that can be undone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_works_on_a_copy_and_the_folder_changes_only_when_kept() {
    let script = r#"
steps:
  - expect: { last_user_contains: "Tabelle", offers_tool: write_spreadsheet, lacks_tool: bash }
    respond: { text: "Ich sehe mir zuerst den Ordner an.", tool_calls: [{ name: list_files, arguments: {} }] }
  - expect: { any_message_contains: "strom.txt" }
    respond: { tool_calls: [{ name: read_document, arguments: { path: "Rechnungen/strom.txt" } }] }
  - expect: { any_message_contains: "120 Euro" }
    respond: { tool_calls: [{ name: write_spreadsheet, arguments: { path: "Übersicht.xlsx", sheets: [{ name: "2025", rows: [["Firma", "Betrag"], ["Stadtwerke", "120"]] }] } }] }
  - respond: { tool_calls: [{ name: move_file, arguments: { from: "Rechnungen/strom.txt", to: "2025/Strom.txt" } }] }
  - respond: { text: "Fertig: Übersicht.xlsx, und die Stromrechnung liegt jetzt in 2025." }
  # On its own: the write needs no OK.
  - expect: { last_user_contains: "Notiz" }
    respond: { tool_calls: [{ name: write_file, arguments: { path: "notiz.txt", content: "hallo" } }] }
  - respond: { text: "Notiz geschrieben." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let folder = env.home.scratch("Belege");
    std::fs::create_dir_all(folder.join("Rechnungen")).unwrap();
    std::fs::write(folder.join("Rechnungen/strom.txt"), "Stadtwerke: 120 Euro").unwrap();
    let folder = folder.canonicalize().unwrap();
    env.op("open_task_folder", json!({"path": folder})).await;
    let projects = env.op("list_projects", json!({})).await;
    assert_eq!(projects[0]["area"], "tasks", "{projects}");
    let s = env
        .op(
            "create_task",
            json!({"folder": folder, "title": "Rechnungen"}),
        )
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["kind"], "task");
    let mut events = env.d().bus.subscribe();
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Mach eine Tabelle der Rechnungen", "wait": true}),
        )
        .await;
    // What the agent says on the way goes out at once – not only with the answer.
    let mut notes = Vec::new();
    while let Ok(e) = events.try_recv() {
        if e.kind == "agent.note" && e.subject.as_deref() == Some(id.as_str()) {
            notes.push(e.data["text"].clone());
        }
    }
    assert_eq!(notes, [json!("Ich sehe mir zuerst den Ordner an.")]);
    let last = s["messages"].as_array().unwrap().last().unwrap()["text"].clone();
    assert!(last.as_str().unwrap().starts_with("Fertig"), "{s}");
    // The changes – nothing of it in the folder yet.
    let changes: Vec<(String, String, Option<String>)> = s["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["change"].as_str().unwrap().to_string(),
                c["path"].as_str().unwrap().to_string(),
                c["from"].as_str().map(String::from),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            (
                "renamed".into(),
                "2025/Strom.txt".into(),
                Some("Rechnungen/strom.txt".into())
            ),
            ("added".into(), "Übersicht.xlsx".into(), None),
        ]
    );
    assert!(
        folder.join("Rechnungen/strom.txt").exists() && !folder.join("Übersicht.xlsx").exists()
    );
    // Kept: in the folder.
    env.op("apply_changes", json!({"session": id})).await;
    assert!(folder.join("Übersicht.xlsx").exists());
    assert_eq!(
        std::fs::read_to_string(folder.join("2025/Strom.txt")).unwrap(),
        "Stadtwerke: 120 Euro"
    );
    let s = env.op("get_session", json!({"session": id})).await;
    assert!(s["changes"].as_array().unwrap().is_empty());
    assert_eq!(s["applied"]["changes"].as_array().unwrap().len(), 2);
    // And taken back.
    env.op("undo_apply", json!({"session": id})).await;
    assert!(!folder.join("Übersicht.xlsx").exists());
    assert!(
        folder.join("Rechnungen/strom.txt").exists() && !folder.join("2025/Strom.txt").exists()
    );

    // A task works on its own: no mode that asks before each step – the
    // write goes into the copy at once.
    let (ok, e) = env
        .call(
            "update_session",
            json!({"session": id, "permission": "read"}),
            true,
        )
        .await;
    assert!(!ok && e.to_string().contains("works on its own"), "{e}");
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Schreib eine Notiz", "wait": true}),
        )
        .await;
    assert!(s["approvals"].as_array().unwrap().is_empty(), "{s}");
    assert!(
        s["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == "notiz.txt"),
        "{s}"
    );
    assert!(!folder.join("notiz.txt").exists(), "still only in the copy");
    env.stop().await;
}

// covers: M10-AC-04
/// A free task: files given to it are material; its results are saved where
/// the user wants – never over a file that is there; no folder of its own
/// appears anywhere for the user.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_free_task_takes_material_and_saves_its_results() {
    let script = r#"
steps:
  - expect: { any_message_contains: "Tabelle" }
    respond: { tool_calls: [{ name: read_document, arguments: { path: "strom.txt" } }] }
  - expect: { any_message_contains: "120 Euro" }
    respond: { tool_calls: [{ name: write_spreadsheet, arguments: { path: "Übersicht.xlsx", sheets: [{ name: "2025", rows: [["Firma", "Betrag"], ["Stadtwerke", "120"]] }] } }] }
  - respond: { text: "Fertig: Übersicht.xlsx." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let s = env
        .op("create_task", json!({"title": "Rechnungen 2025"}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["free"], true);
    // Material: not a change, not a result.
    let r = reqwest::Client::new()
        .post(format!("{}/api/v1/sessions/{id}/files", env.d().url()))
        .query(&[("name", "strom.txt")])
        .bearer_auth(&env.d().token)
        .body("Stadtwerke: 120 Euro")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let s = env.op("get_session", json!({"session": id})).await;
    assert!(s["changes"].as_array().unwrap().is_empty(), "{s}");
    let (ok, e) = env.call("save_results", json!({"session": id}), true).await;
    assert!(!ok && e.to_string().contains("nothing to save yet"), "{e}");
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Mach eine Tabelle", "wait": true}),
        )
        .await;
    assert_eq!(s["changes"][0]["path"], "Übersicht.xlsx", "{s}");
    // Saved where the user wants – next to a file of the same name, not over it.
    let docs = env.home.scratch("Dokumente");
    std::fs::write(docs.join("Übersicht.xlsx"), "schon da").unwrap();
    let saved = env
        .op("save_results", json!({"session": id, "dir": docs}))
        .await;
    assert!(
        saved["files"][0]
            .as_str()
            .unwrap()
            .ends_with("Übersicht 2.xlsx"),
        "{saved}"
    );
    assert_eq!(
        std::fs::read_to_string(docs.join("Übersicht.xlsx")).unwrap(),
        "schon da"
    );
    let s = env.op("get_session", json!({"session": id})).await;
    assert!(s["changes"].as_array().unwrap().is_empty());
    assert!(s["saved"]["files"].as_array().unwrap().len() == 1);
    // No folder of the task anywhere the user looks.
    let projects = env.op("list_projects", json!({})).await;
    assert!(projects.as_array().unwrap().is_empty(), "{projects}");
    // Opening is for documents only.
    let (ok, _) = env
        .call("open_document", json!({"path": "/bin/ls"}), true)
        .await;
    assert!(!ok);
    env.stop().await;
}

// covers: M10-AC-04
/// Files given to a task in a folder: one the folder already holds is meant
/// as it is (no second copy); a new one goes into the copy. The agent learns
/// where they are with the next message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn files_given_to_a_task_are_named_to_the_agent_and_never_doubled() {
    let script = r#"
steps:
  - expect: { last_user_contains: "(Files the user gave for this:\n- Rechnungen/strom.txt\n- quittung.txt)" }
    respond: { text: "Gesehen." }
  - expect: { last_user_contains: "Und jetzt?" }
    respond: { text: "Nichts weiter." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let folder = env.home.scratch("Quittungen");
    std::fs::create_dir_all(folder.join("Rechnungen")).unwrap();
    std::fs::write(folder.join("Rechnungen/strom.txt"), "Stadtwerke: 120 Euro").unwrap();
    let folder = folder.canonicalize().unwrap();
    let s = env
        .op(
            "create_task",
            json!({"folder": folder, "title": "Quittungen"}),
        )
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    let give = |name: &'static str, body: &'static str| {
        let (url, token, id) = (env.d().url(), env.d().token.clone(), id.clone());
        async move {
            let r = reqwest::Client::new()
                .post(format!("{url}/api/v1/sessions/{id}/files"))
                .query(&[("name", name)])
                .bearer_auth(token)
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()
        }
    };
    give("strom.txt", "Stadtwerke: 120 Euro").await;
    give("quittung.txt", "Bäcker: 4 Euro").await;
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Fass die Quittungen zusammen", "wait": true}),
        )
        .await;
    let last = s["messages"].as_array().unwrap().last().unwrap()["text"].clone();
    assert_eq!(last, "Gesehen.", "{s}");
    // Only the new file is a change – the folder's own is not doubled.
    let changes: Vec<(String, String)> = s["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["change"].as_str().unwrap().to_string(),
                c["path"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(changes, [("added".to_string(), "quittung.txt".to_string())]);
    assert!(!folder.join("quittung.txt").exists() && !folder.join("strom.txt").exists());
    // Named once: the next message carries no files.
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "Und jetzt?", "wait": true}),
        )
        .await;
    let last = s["messages"].as_array().unwrap().last().unwrap()["text"].clone();
    assert_eq!(last, "Nichts weiter.", "{s}");
    env.stop().await;
}

// covers: M10-AC-04
/// A task sees everything in its folder that is not hidden: the home folder
/// itself, Library and system folders are never one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_never_works_in_the_home_folder_or_system_folders() {
    let (env, _, _) = Env::start(&[("Chat-Q8_0", "fallback: { text: \"-\" }\n")]).await;
    let home = dirs::home_dir().unwrap();
    for folder in [
        home.clone(),
        home.join("Library"),
        "/".into(),
        "/usr".into(),
    ] {
        if !folder.is_dir() {
            continue;
        }
        let (ok, e) = env
            .call("create_task", json!({"folder": folder, "title": "x"}), true)
            .await;
        assert!(!ok && e.to_string().contains("too wide"), "{folder:?}: {e}");
        let (ok, _) = env
            .call("open_task_folder", json!({"path": folder}), true)
            .await;
        assert!(!ok, "{folder:?}");
    }
    env.stop().await;
}

// covers: M10-AC-05
/// An agent's model that only thinks – until its token limit, no answer –
/// is asked once more for the answer at once (instead of "the model ended
/// without an answer").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_that_only_thinks_is_asked_for_the_answer() {
    let script = r#"
steps:
  - expect: { thinking: true }
    respond: { reasoning: "Hmm, let me think about this for a very long time …", finish_reason: "length" }
  - expect: { thinking: false }
    respond: { text: "Fertig: nichts zu tun." }
# (Asked whether something is to be done after all: the same answer.)
fallback: { text: "Fertig: nichts zu tun." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let folder = env.home.scratch("Leer");
    let folder = folder.canonicalize().unwrap();
    let s = env
        .op("create_task", json!({"folder": folder, "title": "Nichts"}))
        .await;
    let s = env
        .op(
            "send_message",
            json!({"session": s["id"], "text": "Schau dir den Ordner an", "wait": true}),
        )
        .await;
    let last = s["messages"].as_array().unwrap().last().unwrap()["text"].clone();
    assert_eq!(last, "Fertig: nichts zu tun.", "{s}");
    env.stop().await;
}

// covers: FPL-02 – a long result keeps its failure through the cut between
// turns, stays whole in its session (also after a restart) and is read
// there again; another session cannot read it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_results_keep_their_failure_and_are_read_again_in_their_session() {
    // 3000 lines, one failing check in the middle, the verdict at the end.
    let check = "python3 -c \"import sys\nfor i in range(3000):\n    print('check_%04d ... FAILED: total 12.49 != 12.50' % i if i == 1700 else 'check_%04d ... ok' % i)\nprint('== 2999 passed, 1 failed ==')\nsys.exit(1)\"";
    let script = format!(
        r#"
steps:
  - respond: {{ tool_calls: [{{ name: bash, arguments: {{ command: {check:?} }} }}] }}
  - expect: {{ any_message_contains: "check_1700 ... FAILED" }}
    respond: {{ text: "One check failed." }}
  - respond: {{ tool_calls: [{{ name: read_file, arguments: {{ path: "src/lib.rs" }} }}] }}
  - respond: {{ text: "It returns 42." }}
  - respond: {{ text: "Noted." }}
  - expect: {{ any_message_contains: "[bash · FAILED · shortened from 3002 lines · read_result r1]" }}
    respond: {{ tool_calls: [{{ name: read_result, arguments: {{ id: "r1", query: "failed" }} }}] }}
  - expect: {{ any_message_contains: "1701\tcheck_1700 ... FAILED: total 12.49 != 12.50" }}
    respond: {{ text: "check_1700 failed: total 12.49 != 12.50." }}
"#
    );
    let after_restart = r#"
steps:
  - expect: { offers_tool: read_result }
    respond: { tool_calls: [{ name: read_result, arguments: { id: "r1", from_line: -1 } }] }
  - expect: { any_message_contains: "[exit code 1]" }
    respond: { text: "Still there after the restart." }
  - respond: { tool_calls: [{ name: read_result, arguments: { id: "r1" } }] }
  - expect: { any_message_contains: "there is no result r1 in this conversation" }
    respond: { text: "Not mine." }
"#;
    let (mut env, _, _) = Env::start(&[("Chat-Q8_0", &script)]).await;
    let dir = project(&env);
    let s = env
        .op("create_session", json!({"cwd": dir, "permission": "shell"}))
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    for (text, answer) in [
        ("Run the checks", "One check failed."),
        ("What does old_name return?", "It returns 42."),
        ("Thanks", "Noted."),
        // Two turns later the check run is cut – by what it is: its failure
        // and its end stay, and the cut names where the whole of it is.
        (
            "Which check failed?",
            "check_1700 failed: total 12.49 != 12.50.",
        ),
    ] {
        let s = env
            .op(
                "send_message",
                json!({"session": id, "text": text, "wait": true}),
            )
            .await;
        let last = s["messages"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["text"], answer, "{s}");
    }
    // The whole result is kept with the session – and survives a restart.
    env.set_scripts(&[("Chat-Q8_0", after_restart)]);
    env.restart().await;
    let s = env
        .op(
            "send_message",
            json!({"session": id, "text": "And the verdict?", "wait": true}),
        )
        .await;
    assert_eq!(
        s["messages"].as_array().unwrap().last().unwrap()["text"],
        "Still there after the restart."
    );
    // Another session does not get it.
    let other = env
        .op("create_session", json!({"cwd": dir, "permission": "shell"}))
        .await;
    let s = env
        .op(
            "send_message",
            json!({"session": other["id"], "text": "Show r1", "wait": true}),
        )
        .await;
    assert_eq!(
        s["messages"].as_array().unwrap().last().unwrap()["text"],
        "Not mine."
    );
    // Deleting the session deletes its results.
    let results = env
        .home
        .paths
        .home()
        .join("sessions")
        .join(&id)
        .join("results");
    assert!(results.join("r1.json").exists());
    env.op("delete_session", json!({"session": id})).await;
    assert!(!results.exists());
    env.stop().await;
}

// covers: FPL-03 – a task's result is looked at and checked before it is
// kept: a wrong total is found with its place, what was shown and checked
// is what gets saved – a newer version is refused until looked at again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn results_are_checked_and_what_was_checked_is_what_is_saved() {
    let script = r#"
steps:
  - respond: { tool_calls: [{ name: write_spreadsheet, arguments: { path: "Kosten.xlsx", sheets: [{ name: "2025", rows: [["Posten", "Betrag"], ["Miete", "900"], ["Strom", "80"], ["Summe", "990"]] }] } }] }
  - respond: { text: "Fertig: Kosten.xlsx." }
  - respond: { tool_calls: [{ name: write_spreadsheet, arguments: { path: "Kosten.xlsx", overwrite: true, sheets: [{ name: "2025", rows: [["Posten", "Betrag"], ["Miete", "900"], ["Strom", "80"], ["Summe", "980"]] }] } }] }
  - respond: { text: "Korrigiert." }
"#;
    let (env, _, _) = Env::start(&[("Chat-Q8_0", script)]).await;
    let s = env.op("create_task", json!({"title": "Kosten"})).await;
    let id = s["id"].as_str().unwrap().to_string();
    env.op(
        "send_message",
        json!({"session": id, "text": "Mach eine Tabelle der Kosten mit den Spalten Posten und Betrag und einer Summe", "wait": true}),
    )
    .await;
    let checks = env.op("check_results", json!({"session": id})).await;
    assert_eq!(checks["files"][0]["path"], "Kosten.xlsx", "{checks}");
    assert_eq!(checks["files"][0]["worst"], "error", "{checks}");
    let first = checks["version"].as_str().unwrap().to_string();
    let p = env
        .op(
            "preview_result",
            json!({"session": id, "path": "Kosten.xlsx"}),
        )
        .await;
    assert_eq!(p["version"], first.as_str());
    assert_eq!(
        p["layout"]["sheets"][0]["rows"][3]["cells"],
        json!(["Summe", "990"])
    );
    let wrong: Vec<&Value> = p["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["level"] == "error")
        .collect();
    assert_eq!(wrong.len(), 1, "{p}");
    assert_eq!(wrong[0]["place"], "2025!B4");
    assert!(wrong[0]["message"].as_str().unwrap().contains("980"), "{p}");
    // Only a result of this task is shown – never another path.
    let (ok, _) = env
        .call(
            "preview_result",
            json!({"session": id, "path": "../../token"}),
            true,
        )
        .await;
    assert!(!ok);
    // The task corrects it: what was checked before is not what would be saved.
    env.op(
        "send_message",
        json!({"session": id, "text": "Die Summe stimmt nicht", "wait": true}),
    )
    .await;
    let docs = env.home.scratch("Dokumente");
    let (ok, e) = env
        .call(
            "save_results",
            json!({"session": id, "dir": docs, "version": first}),
            true,
        )
        .await;
    assert!(!ok && e.to_string().contains("not the ones you saw"), "{e}");
    assert_eq!(
        std::fs::read_dir(&docs).unwrap().count(),
        0,
        "nothing saved"
    );
    // Looked at again: right now – and exactly that file is saved.
    let checks = env.op("check_results", json!({"session": id})).await;
    assert_eq!(checks["files"][0]["worst"], "ok", "{checks}");
    let saved = env
        .op(
            "save_results",
            json!({"session": id, "dir": docs, "version": checks["version"]}),
        )
        .await;
    let file = std::path::PathBuf::from(saved["files"][0].as_str().unwrap());
    use sha2::Digest;
    let hash = hex::encode(&sha2::Sha256::digest(std::fs::read(&file).unwrap())[..8]);
    assert_eq!(hash, checks["files"][0]["file"].as_str().unwrap());
    env.stop().await;
}
