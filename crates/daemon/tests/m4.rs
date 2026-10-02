//! M4: several models side by side – routing, comparisons, A/B tests,
//! leaderboard and recommendations – over the real daemon with fake Hugging
//! Face and one scripted fake llama-server per model.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::hardware::{Gpu, HardwareProfile};
use ancilo_models::routing::{Arm, assign_arm};
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin, home::git_repo};
use serde_json::{Value, json};

const GIB: u64 = 1 << 30;

struct Env {
    home: TestHome,
    _hf: FakeHf,
    d: Option<DaemonHandle>,
}

/// `o/Alpha-GGUF` → model id `alpha-q8_0`, script `Alpha-Q8_0.yaml`.
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

impl Env {
    /// `scripts`: (model file stem, YAML script).
    async fn start(
        repos: Vec<FakeRepo>,
        scripts: &[(&str, &str)],
        hw: HardwareProfile,
        env: &[(&str, &str)],
    ) -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(repos).await;
        let dir = home.scratch("scripts");
        for (stem, script) in scripts {
            std::fs::write(dir.join(format!("{stem}.yaml")), script).unwrap();
        }
        let hw_file = home.scratch("hw").join("hw.json");
        std::fs::write(&hw_file, serde_json::to_string(&hw).unwrap()).unwrap();
        let mut llama_env: std::collections::BTreeMap<String, String> =
            [("FAKE_LLM_SCRIPT_DIR".to_string(), dir.display().to_string())]
                .into_iter()
                .collect();
        for (k, v) in env {
            llama_env.insert(k.to_string(), v.to_string());
        }
        let config = Config {
            hf_endpoint: hf.url(),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(vec![]),
            hardware_override: Some(hw_file),
            llama_server_env: llama_env,
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

    async fn chat(&self, model: &str, text: &str) -> (u16, Value) {
        let r = reqwest::Client::new()
            .post(format!("{}/v1/chat/completions", self.d().url()))
            .bearer_auth(&self.d().token)
            .json(&json!({"model": model, "messages": [{"role": "user", "content": text}]}))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    /// Model calls the gateway made to `model`.
    async fn calls(&self, model: &str) -> u64 {
        self.op("gateway_stats", json!({"model": model})).await["requests"]
            .as_u64()
            .unwrap()
    }

    async fn report(&self, id: &str) -> Value {
        let r = self
            .op("comparison_report", json!({"id": id, "wait_s": 60}))
            .await;
        assert_eq!(r["status"], "done", "{r}");
        r
    }

    async fn stop(mut self) {
        if let Some(d) = self.d.take() {
            d.stop().await;
        }
    }
}

/// Writes the right content – fast.
const GOOD: &str = r#"
cycle: true
steps:
  - respond: { tool_calls: [{ name: write_file, arguments: { path: "out.txt", content: "ok\n" } }] }
  - respond: { text: "Wrote out.txt." }
"#;

/// Writes the wrong content – slowly.
const BAD: &str = r#"
cycle: true
steps:
  - respond: { tool_calls: [{ name: write_file, arguments: { path: "out.txt", content: "wrong\n" } }], delay_ms: 150 }
  - respond: { text: "Wrote out.txt.", delay_ms: 150 }
"#;

const CHECK: &str = "grep -qx ok out.txt";

fn project(env: &Env) -> PathBuf {
    let dir = env.home.scratch("project");
    git_repo(
        &dir,
        &[
            ("README.md", "# Project\n"),
            ("src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    std::fs::canonicalize(dir).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

async fn two_models(scripts: &[(&str, &str)]) -> (Env, String, String) {
    let env = Env::start(
        vec![repo("o/Alpha-GGUF"), repo("o/Beta-GGUF")],
        scripts,
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    let a = env.add("o/Alpha-GGUF").await;
    let b = env.add("o/Beta-GGUF").await;
    (env, a, b)
}

fn explain(model: Option<&str>, kind: Option<&str>, role: &str) -> Value {
    json!({"model": model, "kind": kind, "role": role})
}

// covers: M4-AC-01
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_model_serves_every_role_and_a_second_one_can_take_a_role() {
    let env = Env::start(
        vec![repo("o/Alpha-GGUF"), repo("o/Beta-GGUF")],
        &[("Alpha-Q8_0", GOOD), ("Beta-Q8_0", GOOD)],
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    let a = env.add("o/Alpha-GGUF").await;
    for role in ["default", "delegation", "coding", "assistant"] {
        let r = env.op("explain_route", explain(None, None, role)).await;
        assert_eq!(r["model"], a.as_str(), "{role}: {r}");
    }
    let dir = project(&env);
    let t = env
        .op("delegate", json!({"task": "Create out.txt", "cwd": dir}))
        .await;
    assert_eq!(t["model"], a.as_str());
    assert_eq!(t["via"], "default");

    let b = env.add("o/Beta-GGUF").await;
    // Adding a second model changes nothing by itself.
    assert_eq!(
        env.op("explain_route", explain(None, None, "delegation"))
            .await["model"],
        a.as_str()
    );
    env.op("assign_role", json!({"role": "delegation", "model": b}))
        .await;
    let t = env
        .op("delegate", json!({"task": "Create out.txt", "cwd": dir}))
        .await;
    assert_eq!(t["model"], b.as_str());
    assert_eq!(t["via"], "role");
    // Model API requests still use the default model.
    let before = (env.calls(&a).await, env.calls(&b).await);
    let (s, v) = env.chat("claude-sonnet-4-5", "hi").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (env.calls(&a).await, env.calls(&b).await),
        (before.0 + 1, before.1)
    );
    env.stop().await;
}

// covers: M4-AC-02
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routing_follows_one_precedence() {
    let env = Env::start(
        vec![
            repo("o/Alpha-GGUF"),
            repo("o/Beta-GGUF"),
            repo("o/Gamma-GGUF"),
        ],
        &[],
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    let a = env.add("o/Alpha-GGUF").await; // default
    let b = env.add("o/Beta-GGUF").await;
    let c = env.add("o/Gamma-GGUF").await;
    env.op("assign_role", json!({"role": "delegation", "model": b}))
        .await;
    env.op("set_route", json!({"kind": "tests", "model": c}))
        .await;

    // (model, kind, role, task) → (model, via)
    type Case<'a> = (
        Option<&'a str>,
        Option<&'a str>,
        &'a str,
        Option<&'a str>,
        &'a str,
        &'a str,
    );
    let table: Vec<Case> = vec![
        (
            Some(a.as_str()),
            Some("tests"),
            "delegation",
            None,
            a.as_str(),
            "explicit",
        ),
        (None, Some("tests"), "delegation", None, c.as_str(), "kind"),
        (
            None,
            None,
            "delegation",
            Some("Write unit tests for the parser"),
            c.as_str(),
            "kind",
        ),
        (None, Some("fix"), "delegation", None, b.as_str(), "role"),
        (None, None, "coding", None, a.as_str(), "default"),
        (
            Some("delegation"),
            None,
            "default",
            None,
            b.as_str(),
            "role",
        ),
        (Some("gpt-4o"), None, "delegation", None, b.as_str(), "role"),
        (
            None,
            Some("no-such-kind"),
            "delegation",
            None,
            b.as_str(),
            "role",
        ),
    ];
    for (model, kind, role, task, want, via) in table {
        let r = env
            .op(
                "explain_route",
                json!({"model": model, "kind": kind, "role": role, "task": task}),
            )
            .await;
        assert_eq!(
            (r["model"].as_str().unwrap(), r["via"].as_str().unwrap()),
            (want, via),
            "{model:?} {kind:?} {role} {task:?}: {r}"
        );
    }
    let (ok, err) = env
        .call("set_route", json!({"kind": "testz", "model": c}), true)
        .await;
    assert!(!ok && err.to_string().contains("unknown kind"), "{err}");

    // Removing a model removes its rule and role – routing falls back.
    env.op("remove_model", json!({"model": c})).await;
    let r = env
        .op("explain_route", explain(None, Some("tests"), "delegation"))
        .await;
    assert_eq!(
        (r["model"].as_str().unwrap(), r["via"].as_str().unwrap()),
        (b.as_str(), "role")
    );
    env.op("remove_model", json!({"model": b})).await;
    let r = env
        .op("explain_route", explain(None, Some("tests"), "delegation"))
        .await;
    assert_eq!(
        (r["model"].as_str().unwrap(), r["via"].as_str().unwrap()),
        (a.as_str(), "default")
    );
    assert_eq!(env.op("list_routes", json!({})).await, json!([]));
    env.stop().await;
}

// covers: M4-AC-03, M4-AC-04
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comparisons_are_fair_leave_the_project_alone_and_report_reality() {
    let (env, a, b) = two_models(&[("Alpha-Q8_0", GOOD), ("Beta-Q8_0", BAD)]).await;
    let dir = project(&env);
    // Uncommitted work in the project must stay as it is.
    std::fs::write(dir.join("notes.txt"), "draft\n").unwrap();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let status_before = git(&dir, &["status", "--porcelain"]);

    let started = env
        .op("compare_models", json!({"task": "Create out.txt containing ok", "cwd": dir, "models": [a, b], "check": CHECK, "repeat": 2}))
        .await;
    let id = started["id"].as_str().unwrap().to_string();
    let r = env.report(&id).await;

    // Same commit, logged configuration.
    assert_eq!(r["config"]["base_commit"], head.as_str());
    assert_eq!(r["config"]["temperature"], 0.2);
    assert!(r["config"]["worker_prompt_sha256"].as_str().unwrap().len() == 64);
    assert!(
        r["config"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "write_file")
    );
    let runs = r["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4);
    for run in runs {
        let branch = run["branch"].as_str().unwrap();
        // Every run started from the same commit.
        assert_eq!(
            git(&dir, &["rev-parse", &format!("{branch}~1")]),
            head,
            "{run}"
        );
        assert_eq!(
            git(&dir, &["show", &format!("{branch}:out.txt")]),
            if run["label"] == "A" { "ok" } else { "wrong" }
        );
    }
    // Worktrees are gone, the project is untouched.
    assert_eq!(git(&dir, &["worktree", "list"]).lines().count(), 1);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&dir, &["status", "--porcelain"]), status_before);
    assert!(!dir.join("out.txt").exists());

    // The report reflects reality: A passes and is faster; B fails the check.
    let ranking = r["ranking"].as_array().unwrap();
    assert_eq!(ranking[0]["model"], a.as_str());
    assert_eq!(ranking[0]["success"]["rate"], 1.0);
    assert_eq!(ranking[1]["model"], b.as_str());
    assert_eq!(ranking[1]["success"]["rate"], 0.0);
    assert!(
        ranking[1]["duration_p50_ms"].as_u64().unwrap()
            > ranking[0]["duration_p50_ms"].as_u64().unwrap() + 200,
        "{r}"
    );
    let failed = runs.iter().find(|x| x["model"] == b.as_str()).unwrap();
    assert_eq!(failed["check"]["passed"], false);
    assert!(failed["failure"].as_str().unwrap().contains("check failed"));
    assert!(ranking[0]["load_ms"].is_u64());
    assert_eq!(ranking[0]["typical_diff"], "+1 −0 in 1 file");
    assert_eq!(r["kind"], "other");
    env.stop().await;
}

// covers: M4-AC-05
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comparisons_never_exceed_the_memory_budget() {
    // Budget for exactly one model at a time.
    let hw = HardwareProfile {
        total_ram_bytes: 2 * GIB,
        gpu: Gpu::Metal,
        gpu_memory_bytes: Some(GIB),
        ..HardwareProfile::apple(2)
    };
    let env = Env::start(
        vec![repo("o/Alpha-GGUF"), repo("o/Beta-GGUF")],
        &[("Alpha-Q8_0", GOOD), ("Beta-Q8_0", GOOD)],
        hw,
        &[],
    )
    .await;
    let a = env.add("o/Alpha-GGUF").await;
    let b = env.add("o/Beta-GGUF").await;
    let dir = project(&env);
    let mut events = env.d().bus.subscribe();
    let started = env
        .op(
            "compare_models",
            json!({"task": "Create out.txt", "cwd": dir, "models": [a, b], "check": CHECK}),
        )
        .await;
    let id = started["id"].as_str().unwrap().to_string();
    let mut max_running = 0;
    for _ in 0..600 {
        let list = env.op("list_models", json!({})).await;
        let running = list
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["status"] == "running" || m["status"] == "starting")
            .count();
        max_running = max_running.max(running);
        let hwv = env.op("hardware_info", json!({})).await;
        assert!(hwv["used_bytes"].as_u64().unwrap() <= hwv["model_budget_bytes"].as_u64().unwrap());
        if env.op("comparison_status", json!({"id": id})).await["status"] == "done" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(max_running, 1);
    let r = env.report(&id).await;
    assert_eq!(r["ranking"][0]["success"]["rate"], 1.0);
    assert_eq!(r["ranking"][1]["success"]["rate"], 1.0);
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
    env.stop().await;
}

// covers: M4-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ab_assignment_is_reproducible_logged_and_honoured() {
    let (env, a, b) = two_models(&[]).await;
    let t = env
        .op(
            "ab_start",
            json!({"role": "default", "b": b, "share": 50, "seed": 7}),
        )
        .await;
    assert_eq!(t["model_a"], a.as_str());
    assert_eq!(t["share"], 0.5);
    let (ok, err) = env
        .call("ab_start", json!({"role": "default", "b": b}), true)
        .await;
    assert!(!ok && err.to_string().contains("already running"));
    let mut events = env.d().bus.subscribe();
    for i in 0..40 {
        let (s, v) = env.chat("default", &format!("request {i}")).await;
        assert_eq!(s, 200, "{v}");
    }
    // An explicit model wish is never overridden.
    env.chat(&a, "explicit").await;
    let mut arms = Vec::new();
    while let Ok(e) = events.try_recv() {
        if e.kind == "ab.assigned" {
            arms.push((
                e.data["seq"].as_u64().unwrap(),
                e.data["arm"].as_str().unwrap().to_string(),
                e.data["model"].as_str().unwrap().to_string(),
            ));
        }
    }
    assert_eq!(arms.len(), 40);
    for (seq, arm, model) in &arms {
        let want = assign_arm(7, *seq, 0.5);
        assert_eq!(arm, if want == Arm::B { "B" } else { "A" });
        assert_eq!(model, if want == Arm::B { &b } else { &a });
    }
    let n_b = arms.iter().filter(|x| x.1 == "B").count();
    assert!((10..=30).contains(&n_b), "{n_b}");
    let report = env.op("ab_report", json!({"test": "default"})).await;
    assert_eq!(
        report["a"]["assigned"].as_u64().unwrap() + report["b"]["assigned"].as_u64().unwrap(),
        40
    );
    assert_eq!(report["b"]["completed"].as_u64().unwrap(), n_b as u64);
    assert_eq!(report["metric"], "no_error");
    assert_eq!(report["verdict"], "insufficient_data");
    // Each arm really served its requests.
    assert_eq!(env.calls(&b).await, n_b as u64);
    assert_eq!(env.calls(&a).await, 40 - n_b as u64 + 1);
    env.op("ab_stop", json!({"test": "default"})).await;
    assert_eq!(env.op("ab_status", json!({})).await[0]["status"], "stopped");
    env.stop().await;
}

// covers: M4-AC-08
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clearly_worse_b_is_stopped_by_the_guardrail() {
    let failing = r#"
cycle: true
steps:
  - respond: { http_error: { status: 500, message: "boom" } }
"#;
    let (env, a, _b) = two_models(&[("Beta-Q8_0", failing)]).await;
    let t = env
        .op(
            "ab_start",
            json!({"role": "default", "b": "beta-q8_0", "share": 0.5, "seed": 3, "guard_min": 5}),
        )
        .await;
    let id = t["id"].as_str().unwrap().to_string();
    let mut events = env.d().bus.subscribe();
    let mut stopped_after = None;
    for i in 0..80 {
        env.chat("default", "hi").await;
        if env.op("ab_status", json!({})).await[0]["status"] == "guardrail_stopped" {
            stopped_after = Some(i);
            break;
        }
    }
    assert!(stopped_after.is_some(), "guardrail never fired");
    let mut seen = false;
    while let Ok(e) = events.try_recv() {
        seen |= e.kind == "ab.guardrail_stopped" && e.subject.as_deref() == Some(id.as_str());
    }
    assert!(seen);
    // From now on everything goes to A again.
    for _ in 0..5 {
        let (s, _) = env.chat("default", "after").await;
        assert_eq!(s, 200);
    }
    let report = env.op("ab_report", json!({"test": id})).await;
    assert_eq!(report["test"]["status"], "guardrail_stopped");
    assert!(
        report["test"]["end_reason"]
            .as_str()
            .unwrap()
            .contains("guardrail")
    );
    assert!(report["b"]["assigned"].as_u64().unwrap() >= 5);
    assert_eq!(report["a"]["model"], a.as_str());
    env.stop().await;
}

// covers: M4-AC-09, M4-AC-11
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recommendations_need_data_and_a_confirmation() {
    let (env, a, b) = two_models(&[("Alpha-Q8_0", BAD), ("Beta-Q8_0", GOOD)]).await;
    let dir = project(&env);
    // Too few runs: no recommendation.
    let c = env.op("compare_models", json!({"task": "Write tests into out.txt", "cwd": dir, "models": [a, b], "check": CHECK, "repeat": 3, "kind": "tests"})).await;
    env.report(c["id"].as_str().unwrap()).await;
    assert_eq!(env.op("recommendations", json!({})).await, json!([]));
    // Enough runs: B is clearly better for `tests`.
    let c = env.op("compare_models", json!({"task": "Write tests into out.txt", "cwd": dir, "models": [a, b], "check": CHECK, "repeat": 8, "kind": "tests"})).await;
    env.report(c["id"].as_str().unwrap()).await;
    // Refreshed from several places at the same moment (a finished
    // comparison, the app, the assistant): never twice. (The race seen in CI
    // – the refresh at the end of a comparison against the app's request –
    // cannot be timed deterministically here; this guards the invariant.)
    let comparer = env.d().comparer.clone();
    let start = std::sync::Barrier::new(16);
    std::thread::scope(|s| {
        for _ in 0..16 {
            s.spawn(|| {
                start.wait();
                comparer.refresh_recommendations().unwrap();
            });
        }
    });
    let recs = env.op("recommendations", json!({"all": true})).await;
    assert_eq!(recs.as_array().unwrap().len(), 1, "{recs}");
    let rec = &recs[0];
    assert_eq!(
        rec["action"],
        json!({"type": "set_route", "kind": "tests", "model": b})
    );
    assert!(
        rec["rationale"].as_str().unwrap().contains("n = 11"),
        "{rec}"
    );
    // Nothing changed by itself.
    assert_eq!(env.op("list_routes", json!({})).await, json!([]));
    assert_eq!(
        env.op("explain_route", explain(None, Some("tests"), "delegation"))
            .await["model"],
        a.as_str()
    );
    // Applying needs a confirmation …
    let rid = rec["id"].as_str().unwrap();
    let (ok, err) = env
        .call("apply_recommendation", json!({"id": rid}), false)
        .await;
    assert!(!ok, "{err}");
    assert_eq!(env.op("list_routes", json!({})).await, json!([]));
    // … and then does exactly what was recommended.
    env.op("apply_recommendation", json!({"id": rid})).await;
    assert_eq!(
        env.op("list_routes", json!({})).await,
        json!([{"kind": "tests", "model": b}])
    );
    assert_eq!(
        env.op("explain_route", explain(None, None, "delegation"))
            .await["model"],
        a.as_str()
    );
    assert_eq!(env.op("recommendations", json!({})).await, json!([]));

    // The results are on the leaderboard, with a Markdown export.
    let board = env.op("leaderboard", json!({"kind": "tests"})).await;
    let entries = board["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["model"], b.as_str());
    assert_eq!(entries[0]["success"]["n"], 11);
    assert!(
        board["markdown"]
            .as_str()
            .unwrap()
            .contains(&format!("| tests | {b} | 100 %")),
        "{board}"
    );
    env.stop().await;
}

// covers: M4-AC-10
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blind_comparisons_stay_blind_until_rated() {
    let (env, a, b) = two_models(&[("Alpha-Q8_0", GOOD), ("Beta-Q8_0", BAD)]).await;
    let dir = project(&env);
    let mut events = env.d().bus.subscribe();
    let c = env.op("compare_models", json!({"task": "Create out.txt", "cwd": dir, "models": [a, b], "check": CHECK, "blind": true})).await;
    let id = c["id"].as_str().unwrap().to_string();
    let r = env.report(&id).await;
    let leaks = |v: &Value| v.to_string().contains(&a) || v.to_string().contains(&b);
    assert_eq!(r["revealed"], false);
    assert!(!leaks(&r), "{r}");
    assert!(!leaks(
        &env.op("comparison_status", json!({"id": id})).await
    ));
    assert!(!leaks(&env.op("list_comparisons", json!({})).await));
    assert_eq!(env.op("leaderboard", json!({})).await["entries"], json!([]));
    while let Ok(e) = events.try_recv() {
        if e.kind.starts_with("compare.") || e.kind.starts_with("agent.") {
            assert!(!leaks(&e.data), "{} {}", e.kind, e.data);
        }
    }
    let (ok, _) = env
        .call("rate_comparison", json!({"id": id, "best": "Z"}), true)
        .await;
    assert!(!ok);
    let rated = env
        .op(
            "rate_comparison",
            json!({"id": id, "best": "A", "note": "cleaner"}),
        )
        .await;
    assert_eq!(rated["revealed"], true);
    let models: Vec<&str> = rated["ranking"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["model"].as_str().unwrap())
        .collect();
    assert!(models.contains(&a.as_str()) && models.contains(&b.as_str()));
    assert_eq!(rated["rating"]["best"], "A");
    assert_eq!(
        env.op("leaderboard", json!({})).await["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (ok, _) = env
        .call("rate_comparison", json!({"id": id, "best": "B"}), true)
        .await;
    assert!(!ok, "a rating is final");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saved_suites_run_on_several_models_and_a_judge_is_marked() {
    let judge = r#"
cycle: true
steps:
  - respond: { text: "{\"score\": 8, \"reason\": \"does what was asked\"}" }
"#;
    let env = Env::start(
        vec![
            repo("o/Alpha-GGUF"),
            repo("o/Beta-GGUF"),
            repo("o/Judge-GGUF"),
        ],
        &[
            ("Alpha-Q8_0", GOOD),
            ("Beta-Q8_0", BAD),
            ("Judge-Q8_0", judge),
        ],
        HardwareProfile::apple(64),
        &[],
    )
    .await;
    let a = env.add("o/Alpha-GGUF").await;
    let b = env.add("o/Beta-GGUF").await;
    let j = env.add("o/Judge-GGUF").await;
    let suite = r##"
name: mine
tasks:
  - id: create-ok
    kind: docs
    files: { "README.md": "# x\n" }
    task: "Create out.txt containing ok"
    checks:
      - file_contains: { path: out.txt, text: "ok" }
      - unchanged: [README.md]
"##;
    let (ok, err) = env
        .call(
            "save_suite",
            json!({"name": "bad name!", "content": suite}),
            true,
        )
        .await;
    assert!(!ok, "{err}");
    let (ok, err) = env
        .call(
            "save_suite",
            json!({"name": "broken", "content": "name: x\ntasks: []\n"}),
            true,
        )
        .await;
    assert!(!ok, "{err}");
    env.op("save_suite", json!({"name": "mine", "content": suite}))
        .await;
    let suites = env.op("list_suites", json!({})).await;
    assert!(
        suites
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "mine" && s["tasks"] == 1),
        "{suites}"
    );
    let run = env
        .op(
            "run_suite",
            json!({"suite": "mine", "models": [a, b], "repeat": 2}),
        )
        .await;
    let r = env.report(run["id"].as_str().unwrap()).await;
    assert_eq!(r["mode"], "suite");
    assert_eq!(r["ranking"][0]["model"], a.as_str());
    assert_eq!(r["ranking"][0]["success"]["n"], 2);
    assert_eq!(r["ranking"][1]["success"]["rate"], 0.0);
    assert_eq!(r["runs"][0]["suite_task"], "create-ok");
    assert_eq!(r["runs"][0]["kind"], "docs");

    // A judge rates each result – marked as model-based.
    let dir = project(&env);
    let c = env
        .op(
            "compare_models",
            json!({"task": "Create out.txt", "cwd": dir, "models": [a, b], "judge": j}),
        )
        .await;
    let r = env.report(c["id"].as_str().unwrap()).await;
    assert_eq!(r["judgement"]["judge_model"], j.as_str());
    assert!(
        r["judgement"]["note"]
            .as_str()
            .unwrap()
            .contains("not an objective")
    );
    assert_eq!(r["judgement"]["scores"][0]["score"], 8.0);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delegations_follow_kind_rules_and_ab_tests_with_shadow_runs() {
    let (env, a, b) = two_models(&[("Alpha-Q8_0", GOOD), ("Beta-Q8_0", GOOD)]).await;
    let dir = project(&env);
    env.op("set_route", json!({"kind": "docs", "model": b}))
        .await;
    let t = env
        .op(
            "delegate",
            json!({"task": "Create out.txt", "cwd": dir, "kind": "docs"}),
        )
        .await;
    assert_eq!(
        (
            t["model"].as_str().unwrap(),
            t["via"].as_str().unwrap(),
            t["kind"].as_str().unwrap()
        ),
        (b.as_str(), "kind", "docs")
    );
    env.op("remove_route", json!({"kind": "docs"})).await;
    std::fs::remove_file(dir.join("out.txt")).unwrap();

    // Shadow mode: A answers, B does the same task in a worktree alongside.
    env.op(
        "ab_start",
        json!({"role": "delegation", "b": b, "shadow": true, "seed": 1}),
    )
    .await;
    let t = env
        .op("delegate", json!({"task": "Create out.txt", "cwd": dir}))
        .await;
    assert_eq!(t["model"], a.as_str());
    assert_eq!(t["status"], "done");
    let mut report = Value::Null;
    for _ in 0..1200 {
        report = env.op("ab_report", json!({"test": "delegation"})).await;
        if report["b"]["completed"] == 1 && report["a"]["completed"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(report["a"]["success"]["n"], 1, "{report}");
    assert_eq!(report["b"]["success"]["successes"], 1, "{report}");
    assert_eq!(report["metric"], "success");
    let tasks = env.op("list_tasks", json!({})).await;
    let shadow = tasks
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["model"] == b.as_str() && t["via"] == "ab_test")
        .expect("shadow task");
    assert!(shadow["branch"].as_str().unwrap().starts_with("ancilo/"));
    env.stop().await;
}

// covers: M4-AC-13
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m4_operations_are_registered_for_every_surface() {
    let env = Env::start(vec![], &[], HardwareProfile::apple(16), &[]).await;
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
    for want in [
        "list_models",
        "assign_role",
        "set_route",
        "remove_route",
        "list_routes",
        "explain_route",
        "compare_models",
        "comparison_status",
        "comparison_report",
        "list_comparisons",
        "cancel_comparison",
        "rate_comparison",
        "save_suite",
        "list_suites",
        "run_suite",
        "ab_start",
        "ab_stop",
        "ab_status",
        "ab_report",
        "recommendations",
        "apply_recommendation",
        "dismiss_recommendation",
        "leaderboard",
    ] {
        assert!(names.contains(&want), "missing {want}");
    }
    // REST, CLI and MCP all dispatch through the same registry
    // (every_operation_is_reachable_via_rest_and_cli, mcp_over_stdio_…).
    let apply = ops
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "apply_recommendation")
        .unwrap();
    assert_eq!(apply["consequential"], true, "{apply}");
    env.stop().await;
}
