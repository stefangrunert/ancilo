//! M6: the assistant, cloud models, keys, diagnosis and first-start setup –
//! over the real daemon with fake Hugging Face, fake llama-servers and an
//! in-process fake "cloud provider".

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::download::DownloadOptions;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{
    FakeFile, FakeHf, FakeLlm, FakeRepo, Script, TestHome, fake_llama_server_bin,
};
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
            4096,
            150_000,
        )],
    )
    .with_gguf_meta(json!({"architecture": arch, "context_length": 4096}))
}

impl Env {
    async fn start(repos: Vec<FakeRepo>, scripts: &[(&str, &str)], hw: HardwareProfile) -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(repos).await;
        let dir = home.scratch("scripts");
        for (stem, script) in scripts {
            std::fs::write(dir.join(format!("{stem}.yaml")), script).unwrap();
        }
        let hw_file = home.scratch("hw").join("hw.json");
        std::fs::write(&hw_file, serde_json::to_string(&hw).unwrap()).unwrap();
        let config = Config {
            hf_endpoint: hf.url(),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(vec![]),
            hardware_override: Some(hw_file),
            llama_server_env: [("FAKE_LLM_SCRIPT_DIR".to_string(), dir.display().to_string())]
                .into_iter()
                .collect(),
            ..home.config()
        };
        home.write_config(&config);
        let options = DaemonOptions {
            manager: ManagerOptions {
                download: DownloadOptions {
                    max_attempts: 3,
                    base_backoff: Duration::from_millis(10),
                    progress_interval: Duration::from_millis(5),
                },
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
        Self {
            home,
            _hf: hf,
            d: Some(d),
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

    async fn stop(mut self) {
        if let Some(d) = self.d.take() {
            d.stop().await;
        }
    }
}

async fn with_models(scripts: &[(&str, &str)]) -> (Env, String, String) {
    let env = Env::start(
        vec![
            repo("o/Chat-GGUF", "qwen3"),
            repo("o/Other-GGUF", "qwen3"),
            repo("o/Tiny-Embed-GGUF", "bert"),
        ],
        scripts,
        HardwareProfile::apple(64),
    )
    .await;
    let chat = env.add("o/Chat-GGUF").await;
    let other = env.add("o/Other-GGUF").await;
    env.add("o/Tiny-Embed-GGUF").await;
    (env, chat, other)
}

// covers: M6-AC-01
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_become_the_right_operations() {
    let script = r#"
steps:
  - expect: { last_user_contains: "Which models" }
    respond: { tool_calls: [{ name: list_models, arguments: {} }] }
  - expect: { any_message_contains: "other-q8_0" }
    respond: { text: "You have chat-q8_0, other-q8_0 and tiny-embed-q8_0." }
  - expect: { last_user_contains: "suites" }
    respond: { tool_calls: [{ name: ancilo_operation, arguments: { operation: list_suites } }] }
  - respond: { text: "There is the built-in delegation suite." }
"#;
    let (env, _, _) = with_models(&[("Chat-Q8_0", script)]).await;
    let mut events = env.d().bus.subscribe();
    let r = env
        .op("ask", json!({"prompt": "Which models do I have?"}))
        .await;
    assert_eq!(r["operations"][0]["operation"], "list_models");
    assert_eq!(r["operations"][0]["outcome"], "executed");
    assert!(r["answer"].as_str().unwrap().contains("other-q8_0"));
    assert_eq!(r["model"], "chat-q8_0");
    // Operations outside the offered set are reachable through the generic tool.
    let r = env
        .op("ask", json!({"prompt": "Which suites are there?"}))
        .await;
    assert_eq!(r["operations"][0]["operation"], "list_suites");
    assert_eq!(r["operations"][0]["outcome"], "executed");
    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        kinds.push(e.kind);
    }
    for k in [
        "assistant.thinking",
        "assistant.operation",
        "assistant.answer",
    ] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing: {kinds:?}");
    }
    env.stop().await;
}

// covers: M6-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_are_grounded_in_the_knowledge_base() {
    // The model sees passages from Ancilo's docs along with the question.
    let script = r#"
steps:
  - expect: { any_message_contains: "Possibly relevant" }
    respond: { text: "It needs more memory than this machine has for models – pick a smaller quantization." }
fallback: { text: "It needs more memory than this machine has for models – pick a smaller quantization." }
"#;
    let (env, _, _) = with_models(&[("Chat-Q8_0", script)]).await;
    for _ in 0..1200 {
        if env.op("knowledge_status", json!({})).await["files"]
            .as_u64()
            .unwrap_or(0)
            >= 14
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let r = env
        .op(
            "ask",
            json!({"prompt": "What does it mean when a model does not fit into memory?"}),
        )
        .await;
    assert_eq!(r["grounded"], true, "{r}");
    assert!(
        r["answer"]
            .as_str()
            .unwrap()
            .contains("smaller quantization"),
        "{r}"
    );
    env.stop().await;
}

// covers: M6-AC-02, M6-AC-10
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_wait_for_confirmation_and_then_run_exactly() {
    let script = r#"
steps:
  - respond: { tool_calls: [{ name: assign_role, arguments: { role: delegation, model: other-q8_0 } }] }
  - expect: { any_message_contains: "Not executed yet" }
    respond: { text: "I proposed to use other-q8_0 for delegation; please confirm." }
  - respond: { tool_calls: [{ name: ab_start, arguments: { role: delegation, b: other-q8_0, share: 20 } }, { name: compare_models, arguments: { task: "x", cwd: "/tmp", models: [chat-q8_0, other-q8_0] } }] }
  - respond: { text: "Proposed an A/B test and a comparison." }
"#;
    let (env, chat, other) = with_models(&[("Chat-Q8_0", script)]).await;
    let r = env
        .op(
            "ask",
            json!({"prompt": "Use other-q8_0 for delegated tasks"}),
        )
        .await;
    let pending = r["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["operation"], "assign_role");
    assert_eq!(r["operations"][0]["outcome"], "proposed");
    // Nothing happened yet.
    assert_eq!(
        env.op("explain_route", json!({"role": "delegation"})).await["model"],
        chat.as_str()
    );
    assert_eq!(
        env.op("pending_actions", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Confirming is itself consequential: without the confirmation flag it fails.
    let id = pending[0]["id"].as_str().unwrap();
    let (ok, _) = env.call("confirm_action", json!({"id": id}), false).await;
    assert!(!ok);
    let done = env.op("confirm_action", json!({"id": id})).await;
    assert_eq!(done["action"]["operation"], "assign_role");
    assert_eq!(
        env.op("explain_route", json!({"role": "delegation"})).await["model"],
        other.as_str()
    );
    // An action runs once.
    let (ok, _) = env.call("confirm_action", json!({"id": id}), true).await;
    assert!(!ok);

    // Comparisons and A/B tests are proposed too; rejected ones never run.
    let r = env
        .op(
            "ask",
            json!({"prompt": "Start an A/B test and compare the two models"}),
        )
        .await;
    let ops: Vec<&str> = r["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["operation"].as_str().unwrap())
        .collect();
    assert_eq!(ops, vec!["ab_start", "compare_models"]);
    for p in r["pending"].as_array().unwrap() {
        env.op("reject_action", json!({"id": p["id"]})).await;
    }
    assert_eq!(env.op("ab_status", json!({})).await, json!([]));
    assert_eq!(env.op("list_comparisons", json!({})).await, json!([]));
    env.stop().await;
}

// covers: M6-AC-08
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_operation_is_an_assistant_tool() {
    let (env, _, _) = with_models(&[]).await;
    let ops: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/ops", env.d().url()))
        .bearer_auth(&env.d().token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let a = &env.d().assistant;
    let mut proposed = 0;
    for op in ops.as_array().unwrap() {
        let name = op["name"].as_str().unwrap();
        if ancilo_assistant::NOT_FOR_ASSISTANT.contains(&name) {
            continue;
        }
        let (outcome, text) = a.call_as_tool(name, json!({}), false).await.unwrap();
        assert!(!text.contains("unknown operation"), "{name}: {text}");
        assert!(
            ["executed", "failed", "proposed"].contains(&outcome.as_str()),
            "{name}: {outcome}"
        );
        if op["permission"] == "manage" && !ancilo_assistant::SAFE_CHANGES.contains(&name) {
            assert_eq!(outcome, "proposed", "{name} must wait for confirmation");
            proposed += 1;
        }
    }
    assert!(proposed > 20);
    // On a cloud model only operations that cannot reveal project code.
    let names: Vec<&str> = ops
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    for safe in ancilo_assistant::CLOUD_SAFE {
        assert!(
            names.contains(safe),
            "CLOUD_SAFE names an unknown operation: {safe}"
        );
    }
    for code in [
        "search",
        "get_session",
        "session_diff",
        "list_sessions",
        "task_result",
        "comparison_report",
        "model_logs",
        "delegate",
        "run_coding_eval",
    ] {
        let (outcome, text) = a.call_as_tool(code, json!({}), true).await.unwrap();
        assert_eq!(outcome, "failed", "{code}");
        assert!(
            text.contains("not available with a cloud model"),
            "{code}: {text}"
        );
    }
    let (outcome, _) = a
        .call_as_tool("list_models", json!({}), true)
        .await
        .unwrap();
    assert_eq!(outcome, "executed");
    env.stop().await;
}

/// Records every connection that would leave the machine through a proxy.
fn outbound_recorder() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            c.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    (url, count)
}

// covers: M6-AC-04
/// The real daemon binary in its own process, with every proxy variable
/// pointing at a recorder: any connection that is not to 127.0.0.1 would show
/// up there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_cloud_setup_nothing_leaves_the_machine() {
    let (proxy, outbound) = outbound_recorder();
    let home = TestHome::new();
    let hf = FakeHf::start(vec![
        repo("o/Chat-GGUF", "qwen3"),
        repo("o/Tiny-Embed-GGUF", "bert"),
    ])
    .await;
    let scripts = home.scratch("scripts");
    std::fs::write(
        scripts.join("Chat-Q8_0.yaml"),
        r#"
cycle: true
steps:
  - respond: { tool_calls: [{ name: search, arguments: { query: "which quantization", scope: knowledge } }] }
  - respond: { tool_calls: [{ name: diagnose, arguments: {} }] }
  - respond: { text: "Done." }
"#,
    )
    .unwrap();
    let hw = home.scratch("hw").join("hw.json");
    std::fs::write(
        &hw,
        serde_json::to_string(&HardwareProfile::apple(64)).unwrap(),
    )
    .unwrap();
    let config = Config {
        port: 0,
        hf_endpoint: hf.url(),
        llama_server_bin: Some(fake_llama_server_bin()),
        model_search_dirs: Some(vec![]),
        hardware_override: Some(hw),
        llama_server_env: [(
            "FAKE_LLM_SCRIPT_DIR".to_string(),
            scripts.display().to_string(),
        )]
        .into_iter()
        .collect(),
        ..home.config()
    };
    home.write_config(&config);
    let mut child = std::process::Command::new(fake_llama_server_bin().with_file_name("ancilo"))
        .args(["daemon", "run"])
        .env("ANCILO_HOME", home.path())
        .env("HTTP_PROXY", &proxy)
        .env("HTTPS_PROXY", &proxy)
        .env("ALL_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env("https_proxy", &proxy)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .spawn()
        .unwrap();
    let mut info = None;
    for _ in 0..1200 {
        if let Some(i) = ancilo_daemon::DaemonInfo::read(&home.paths)
            && i.pid == child.id()
        {
            info = Some(i);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let url = info.expect("daemon did not start").url;
    let token = std::fs::read_to_string(home.paths.token_file())
        .unwrap()
        .trim()
        .to_string();
    let op = |name: &'static str, input: Value| {
        let (url, token) = (url.clone(), token.clone());
        async move {
            let r = reqwest::Client::new()
                .post(format!("{url}/api/v1/ops/{name}"))
                .bearer_auth(token)
                .header("x-ancilo-confirm", "true")
                .json(&input)
                .send()
                .await
                .unwrap();
            assert!(r.status().is_success(), "{name}");
            r.json::<Value>().await.unwrap()
        }
    };
    for address in ["o/Chat-GGUF", "o/Tiny-Embed-GGUF"] {
        let id = op(
            "add_model",
            json!({"address": address, "start": false, "context": "small"}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        for _ in 0..1200 {
            if op("model_status", json!({"model": id})).await["status"] == "ready" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    for p in ["Which quantization should I choose?", "Why is it slow?"] {
        op("ask", json!({"prompt": p})).await;
    }
    op("refresh_model_knowledge", json!({})).await;
    op("diagnose", json!({})).await;
    let quiet = outbound.load(Ordering::SeqCst);
    // Positive control: an address outside the machine does reach the
    // recorder – so the zero above is real, not a recorder that sees nothing.
    reqwest::Client::new()
        .post(format!("{url}/api/v1/ops/add_model"))
        .bearer_auth(&token)
        .header("x-ancilo-confirm", "true")
        .json(&json!({"address": "http://example.invalid/v1"}))
        .send()
        .await
        .unwrap();
    let control = outbound.load(Ordering::SeqCst);
    op("daemon_shutdown", json!({})).await;
    for _ in 0..1200 {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    child.kill().ok();
    assert_eq!(quiet, 0, "a connection left the machine");
    assert!(control > 0, "the recorder did not see the control request");
}

// covers: M6-AC-05, M6-AC-06, M6-AC-14
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_models_work_keep_their_key_secret_and_never_get_code() {
    let cloud = FakeLlm::start(Script {
        model: Some("provider/big-model".into()),
        fallback: None,
        ..Default::default()
    })
    .await;
    let (env, chat, _) = with_models(&[]).await;
    let key = "sk-test-0123456789-secret";
    // Without confirmation nothing happens (it sends data out).
    let (ok, _) = env
        .call("set_cloud_provider", json!({"base_url": format!("{}/v1", cloud.url()), "model": "provider/big-model", "api_key": key}), false)
        .await;
    assert!(!ok);
    let r = env
        .op("set_cloud_provider", json!({"base_url": format!("{}/v1", cloud.url()), "model": "provider/big-model", "api_key": key}))
        .await;
    let id = r["model"]["id"].as_str().unwrap().to_string();
    assert_eq!(r["model"]["cloud"], true);
    assert!(r["privacy"].as_str().unwrap().contains("never"));
    // It takes no role by itself.
    assert_eq!(
        env.op("explain_route", json!({"role": "default"})).await["model"],
        chat.as_str()
    );

    // OpenAI-compatible round trip with the key as bearer token.
    let resp: Value = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", env.d().url()))
        .bearer_auth(&env.d().token)
        .json(&json!({"model": id, "messages": [{"role": "user", "content": "hello cloud"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        resp["choices"][0]["message"]["content"], "hello cloud",
        "{resp}"
    );
    let req = cloud
        .requests()
        .into_iter()
        .find(|r| r.path == "/v1/chat/completions")
        .unwrap();
    assert_eq!(
        req.authorization.as_deref(),
        Some(&*format!("Bearer {key}"))
    );
    assert_eq!(req.body["model"], "provider/big-model");

    // The key is nowhere but in the secret store.
    let home = env.home.path().to_path_buf();
    let mut leaks = Vec::new();
    for entry in walk(&home) {
        if let Ok(text) = std::fs::read(&entry)
            && String::from_utf8_lossy(&text).contains(key)
        {
            leaks.push(entry);
        }
    }
    assert!(leaks.is_empty(), "key found in {leaks:?}");
    for op in ["list_models", "diagnose", "get_reliability"] {
        assert!(
            !env.op(op, json!({})).await.to_string().contains(key),
            "{op}"
        );
    }
    let ep = env.d().manager.endpoint(&id).await.unwrap();
    assert_eq!(ep.key, key);

    // Code never goes to the cloud.
    let project = env.home.scratch("p");
    std::fs::write(project.join("a.rs"), "fn a() {}\n").unwrap();
    let project = std::fs::canonicalize(project).unwrap();
    let (ok, err) = env
        .call(
            "delegate",
            json!({"task": "x", "cwd": project, "model": id}),
            true,
        )
        .await;
    assert!(!ok && err.to_string().contains("cloud"), "{err}");
    let (ok, err) = env
        .call(
            "compare_models",
            json!({"task": "x", "cwd": project, "models": [id, chat]}),
            true,
        )
        .await;
    assert!(!ok && err.to_string().contains("cloud"), "{err}");
    // Work on code runs "local only" down to the gateway: a cloud model is
    // refused, and a model that vanished is an error – never a fallback
    // (the cloud model is the default now, a fallback would reach it).
    env.op("assign_role", json!({"role": "default", "model": id}))
        .await;
    let local_only = ancilo_gateway::CallOpts {
        api: "agent",
        local_only: true,
        ..Default::default()
    };
    let hello = |model: &str| json!({"model": model, "messages": [{"role": "user", "content": "fn secret() {}"}]});
    let before = cloud.requests().len();
    let err = env
        .d()
        .gateway
        .chat(hello(&id), local_only.clone())
        .await
        .err()
        .unwrap();
    assert!(err.message().contains("cloud"), "{}", err.message());
    let err = env
        .d()
        .gateway
        .chat(hello("removed-model"), local_only.clone())
        .await
        .err()
        .unwrap();
    assert!(!err.message().is_empty());
    assert_eq!(cloud.requests().len(), before, "nothing reached the cloud");
    // A chat with a cloud model gets neither the user's documents nor a web
    // search (decision `2026-10-02-websuche`).
    let docs = env.home.scratch("docs-for-cloud");
    std::fs::write(
        docs.join("kuchen.md"),
        "Für den Rührkuchen braucht man drei Eier.\n",
    )
    .unwrap();
    env.op("index_project", json!({"cwd": docs})).await;
    env.op("set_preferences", json!({"add_documents": docs}))
        .await;
    env.op(
        "set_web_search",
        json!({"provider": "wikipedia", "mode": "auto"}),
    )
    .await;
    let r = env
        .op("ask", json!({"prompt": "Wie viele Eier brauche ich für den Kuchen?", "model": id, "kind": "chat", "remember": true}))
        .await;
    assert!(r["web"].is_null(), "{r}");
    let sent = cloud.requests().last().unwrap().body.to_string();
    assert!(sent.contains("Wie viele Eier"), "{sent}");
    assert!(
        !sent.contains("drei Eier"),
        "no documents to the cloud: {sent}"
    );
    assert!(
        !sent.contains("Classify the user"),
        "no web planning: {sent}"
    );
    // A conversation with an attached document stays on this computer
    // (decision `2026-10-03-drei-bereiche`).
    let contract = docs.join("vertrag.txt");
    std::fs::write(&contract, "Die Miete beträgt 950 Euro.\n").unwrap();
    let doc = env.op("add_attachment", json!({"path": contract})).await;
    let before = cloud.requests().len();
    let (ok, err) = env
        .call(
            "ask",
            json!({"prompt": "Was kostet die Miete?", "model": id, "kind": "chat", "remember": true, "attachments": [doc["id"]]}),
            false,
        )
        .await;
    assert!(
        !ok && err
            .to_string()
            .contains("stay with the AI on this computer"),
        "{err}"
    );
    assert_eq!(
        cloud.requests().len(),
        before,
        "the document never reached the cloud"
    );
    env.op("assign_role", json!({"role": "default", "model": chat}))
        .await;
    // Removing the model removes its key.
    env.op("remove_model", json!({"model": id})).await;
    assert_eq!(env.d().manager.endpoint(&id).await, None);
    env.stop().await;
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

// covers: M6-AC-09, M6-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_start_setup_downloads_the_standard_models_robustly() {
    let chat = FakeRepo::new(
        "unsloth/Qwen3.5-4B-GGUF",
        vec![
            FakeFile::gguf("Qwen3.5-4B-Q4_K_M.gguf", "qwen3", 4096, 400_000)
                .failing_once_after(100_000),
        ],
    )
    .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 4096}));
    let embed = repo("second-state/All-MiniLM-L6-v2-Embedding-GGUF", "bert");
    let env = Env::start(vec![chat, embed], &[], HardwareProfile::apple(16)).await;
    let plan = env.op("setup", json!({"dry_run": true})).await;
    assert_eq!(
        plan["chat"]["repo"]["id"], "unsloth/Qwen3.5-4B-GGUF",
        "{plan}"
    );
    assert!(plan["download_bytes"].as_u64().unwrap() > 400_000);
    assert!(plan["added"].as_array().unwrap().is_empty());
    // The larger recommendations are explained, not silently skipped.
    assert!(plan["notes"].as_array().unwrap().len() >= 2, "{plan}");
    // A fresh Ancilo has no model to think with: `ask` proposes the setup.
    let asked = env
        .op("ask", json!({"prompt": "Richte Ancilo für mich ein"}))
        .await;
    assert!(
        asked["answer"].as_str().unwrap().contains("Qwen3.5-4B"),
        "{asked}"
    );
    assert_eq!(asked["pending"][0]["operation"], "setup");
    assert!(
        env.op("list_models", json!({}))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut events = env.d().bus.subscribe();
    let confirmed = env
        .op("confirm_action", json!({"id": asked["pending"][0]["id"]}))
        .await;
    let r = &confirmed["result"];
    assert_eq!(r["added"].as_array().unwrap().len(), 2);
    // Both end up ready despite the interrupted download.
    let mut statuses = Vec::new();
    for _ in 0..1200 {
        let list = env.op("list_models", json!({})).await;
        statuses = list
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["status"].as_str().unwrap().to_string())
            .collect();
        if statuses.len() == 2 && statuses.iter().all(|s| s == "ready" || s == "running") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        statuses.iter().all(|s| s == "ready" || s == "running"),
        "{statuses:?}"
    );
    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        kinds.push(e.kind);
    }
    assert!(kinds.iter().any(|k| k == "download.progress"), "{kinds:?}");
    assert!(kinds.iter().any(|k| k == "download.retrying"), "{kinds:?}");
    // A second setup keeps everything.
    let again = env.op("setup", json!({"dry_run": true})).await;
    assert!(
        again["chat"].is_null() && again["embed"].is_null(),
        "{again}"
    );
    // Diagnosis sees a healthy system.
    let diag = env.op("diagnose", json!({})).await;
    assert_eq!(diag["models"].as_array().unwrap().len(), 2);
    env.stop().await;
}

// covers: M6-AC-11
/// A conversation like in a chat app: a follow-up sees the earlier exchange,
/// the conversation is kept and listed, and a proposed action keeps its
/// outcome; a single `ask` without `remember` leaves nothing behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conversations_continue_and_are_kept() {
    let script = r#"
steps:
  - expect: { last_user_contains: "Which models" }
    respond: { tool_calls: [{ name: list_models, arguments: {} }] }
  - respond: { text: "You have chat-q8_0 and other-q8_0." }
  - expect: { last_user_contains: "the second", any_message_contains: "You have chat-q8_0 and other-q8_0." }
    respond: { tool_calls: [{ name: assign_role, arguments: { role: delegation, model: other-q8_0 } }] }
  - respond: { text: "I proposed **other-q8_0** for delegation." }
  - expect: { last_user_contains: "Just once", no_message_contains: "Which models" }
    respond: { tool_calls: [{ name: list_models, arguments: {} }] }
  - respond: { text: "Once." }
"#;
    let (env, _, other) = with_models(&[("Chat-Q8_0", script)]).await;
    let mut events = env.d().bus.subscribe();
    let r = env
        .op(
            "ask",
            json!({"prompt": "Which models do I have?", "remember": true}),
        )
        .await;
    let id = r["conversation"].as_str().unwrap().to_string();
    assert!(id.starts_with("c-"), "{r}");
    let r = env
        .op(
            "ask",
            json!({"prompt": "Use the second for delegation", "conversation": id}),
        )
        .await;
    assert_eq!(r["conversation"], id.as_str());
    let action = r["pending"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{r}"))
        .to_string();
    env.op("confirm_action", json!({"id": action})).await;
    assert_eq!(
        env.op("explain_route", json!({"role": "delegation"})).await["model"],
        other.as_str()
    );

    let list = env.op("list_conversations", json!({})).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["title"], "Which models do I have?");
    assert_eq!(list[0]["messages"], 4);
    let c = env.op("get_conversation", json!({"id": id})).await;
    let texts: Vec<&str> = c["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "Which models do I have?",
            "You have chat-q8_0 and other-q8_0.",
            "Use the second for delegation",
            "I proposed **other-q8_0** for delegation."
        ]
    );
    assert_eq!(c["messages"][3]["pending"][0]["operation"], "assign_role");
    assert_eq!(c["messages"][3]["pending"][0]["outcome"], "executed");
    assert_eq!(c["messages"][3]["model"], "chat-q8_0");
    let mut updated = 0;
    while let Ok(e) = events.try_recv() {
        if e.kind == "conversation.updated" && e.subject.as_deref() == Some(id.as_str()) {
            updated += 1;
        }
    }
    assert!(updated >= 4, "{updated}");

    // A single question is not kept and does not see other conversations.
    let r = env.op("ask", json!({"prompt": "Just once"})).await;
    assert_eq!(r["answer"], "Once.");
    assert!(r.get("conversation").is_none_or(|c| c.is_null()));
    assert_eq!(
        env.op("list_conversations", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (ok, _) = env
        .call(
            "ask",
            json!({"prompt": "x", "conversation": "c-nope"}),
            false,
        )
        .await;
    assert!(!ok);

    // Renaming and deleting are changes: the app asks first.
    env.op("rename_conversation", json!({"id": id, "title": "Models"}))
        .await;
    assert_eq!(
        env.op("list_conversations", json!({})).await[0]["title"],
        "Models"
    );
    env.op("delete_conversation", json!({"id": id})).await;
    assert_eq!(env.op("list_conversations", json!({})).await, json!([]));
    env.stop().await;
}

// covers: M6-AC-12
/// One kind of chat for everything: a plain chat gets no tools (fast, focused
/// answers); a request about Ancilo – or Ancilo's own setup conversation,
/// which starts with its greeting – gets Ancilo's tools.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chats_are_plain_unless_they_are_about_ancilo() {
    let script = r#"
steps:
  - expect: { last_user_contains: "Schreib", has_tools: false, any_message_contains: "help writing a text" }
    respond: { text: "Liebe Anna, leider kann ich morgen nicht." }
  - expect: { last_user_contains: "Modelle", has_tools: true }
    respond: { tool_calls: [{ name: list_models, arguments: {} }] }
  - respond: { text: "Du hast chat-q8_0 und other-q8_0." }
  - expect: { last_user_contains: "Was kannst", has_tools: true, any_message_contains: "Hei, ich bin Ancilo" }
    respond: { tool_calls: [{ name: hardware_info, arguments: {} }] }
  - respond: { text: "Ich kann Modelle für dich einrichten." }
"#;
    let (env, _, _) = with_models(&[("Chat-Q8_0", script)]).await;
    let w = env
        .op(
            "ask",
            json!({"prompt": "Schreib eine Absage an Anna", "remember": true, "kind": "write"}),
        )
        .await;
    assert_eq!(w["answer"], "Liebe Anna, leider kann ich morgen nicht.");
    assert!(w["operations"].as_array().unwrap().is_empty());
    // In the same plain chat, a question about Ancilo gets its tools.
    let m = env
        .op(
            "ask",
            json!({"prompt": "Welche Modelle habe ich?", "conversation": w["conversation"]}),
        )
        .await;
    assert_eq!(m["operations"][0]["operation"], "list_models");
    // Ancilo's own conversation starts with its greeting.
    let greeting = "Hei, ich bin Ancilo und helfe dir, mich einzurichten.";
    let s = env
        .op(
            "ask",
            json!({"prompt": "Was kannst du eigentlich?", "remember": true, "kind": "setup", "greeting": greeting}),
        )
        .await;
    assert_eq!(s["answer"], "Ich kann Modelle für dich einrichten.");
    let c = env
        .op("get_conversation", json!({"id": s["conversation"]}))
        .await;
    assert_eq!(c["kind"], "setup");
    assert_eq!(c["messages"][0]["role"], "assistant");
    assert_eq!(c["messages"][0]["text"], greeting);
    assert_eq!(c["messages"][1]["text"], "Was kannst du eigentlich?");
    let kinds: Vec<String> = env
        .op("list_conversations", json!({}))
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["setup", "write"]);
    env.stop().await;
}

// covers: M6-AC-13, M10-AC-03
/// Chats in a chat project draw on its folder's documents (with their
/// source); a plain chat does not see them (decision
/// `2026-10-03-drei-bereiche`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chats_in_a_project_draw_on_its_documents() {
    let script = r#"
steps:
  - expect: { last_user_contains: "Eier", has_tools: false, any_message_contains: "[rezepte/kuchen.md]" }
    respond: { text: "Für den Kuchen brauchst du drei Eier [rezepte/kuchen.md]." }
  - expect: { last_user_contains: "Eier", no_message_contains: "drei Eier" }
    respond: { text: "Das weiß ich nicht." }
"#;
    let (env, _, _) = with_models(&[("Chat-Q8_0", script)]).await;
    let docs = env.home.scratch("meine-dokumente");
    std::fs::create_dir_all(docs.join("rezepte")).unwrap();
    std::fs::write(
        docs.join("rezepte/kuchen.md"),
        "# Kuchen\n\nFür den Rührkuchen braucht man drei Eier, 200 g Mehl und 150 g Zucker.\n",
    )
    .unwrap();
    std::fs::write(docs.join("kaputt.pdf"), "kein pdf").unwrap();
    let p = env
        .op("set_preferences", json!({"add_documents": docs}))
        .await;
    assert_eq!(p["documents"].as_array().unwrap().len(), 1);
    // Read in the background; what could not be read says why.
    let mut st = Value::Null;
    for _ in 0..200 {
        st = env.op("folder_documents", json!({"folder": docs})).await;
        if st["read"] == 1
            && st["reading"] == false
            && st["not_read"].as_array().unwrap().len() == 1
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(st["read"], 1, "{st}");
    assert_eq!(st["not_read"][0]["path"], "kaputt.pdf", "{st}");
    let r = env
        .op(
            "ask",
            json!({"prompt": "Wie viele Eier brauche ich für den Kuchen?", "remember": true, "kind": "chat", "folder": docs}),
        )
        .await;
    assert_eq!(r["grounded"], true, "{r}");
    assert_eq!(r["documents"], true, "{r}");
    assert!(r["answer"].as_str().unwrap().contains("drei Eier"), "{r}");
    let listed = env.op("list_conversations", json!({})).await;
    assert_eq!(
        listed[0]["folder"],
        docs.canonicalize().unwrap().display().to_string()
    );
    // A plain chat does not see the project's documents.
    let r = env
        .op(
            "ask",
            json!({"prompt": "Wie viele Eier brauche ich für den Kuchen?", "remember": true, "kind": "chat"}),
        )
        .await;
    assert_eq!(r["answer"], "Das weiß ich nicht.", "{r}");
    assert_eq!(r["documents"], false);
    env.stop().await;
}
