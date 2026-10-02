//! Against the real world: real Hugging Face, the pinned llama.cpp build and
//! tiny real models on this machine. Ignored by default; run with
//! `just test-real` (part of `just verify-full`).
//!
//! Uses a persistent home in `target/real-model-home`, so models and the
//! llama.cpp build are downloaded only once.

use std::path::PathBuf;
use std::time::Duration;

use ancilo_core::{Config, Paths};
use ancilo_daemon::DaemonOptions;
use serde_json::{Value, json};

/// The model library of the real tests: `ANCILO_REAL_HOME`, else
/// `target/real-model-home` (the release job keeps it outside the checkout,
/// so models are not downloaded again for every run).
fn home() -> Paths {
    let dir = std::env::var_os("ANCILO_REAL_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/real-model-home")
        });
    Paths::from_home(dir)
}

/// The models the real tests use – set up by whichever test runs first, so
/// no test depends on another one having run (a fresh machine has none).
const STANDARD_MODELS: &[&str] = &[
    "hf.co/unsloth/Qwen3-0.6B-GGUF:Q4_0",
    "hf.co/unsloth/Qwen3.5-4B-GGUF:Q4_K_M",
    "hf.co/second-state/All-MiniLM-L6-v2-Embedding-GGUF:Q4_K_M",
];

/// Models above this size are "large": only with `ANCILO_REAL_LARGE=1`.
const LARGE_BYTES: u64 = 16 << 30;

/// A large model already on the machine (LM Studio) – only on request
/// (`ANCILO_REAL_LARGE=1`): it takes most of the memory and the processor of
/// a working computer for a long time.
fn local_large_model() -> Option<PathBuf> {
    std::env::var_os("ANCILO_REAL_LARGE")?;
    let p = PathBuf::from(std::env::var_os("HOME")?)
        .join(".lmstudio/models/unsloth/Qwen3.6-35B-A3B-GGUF/Qwen3.6-35B-A3B-Q8_0.gguf");
    p.is_file().then_some(p)
}

async fn ensure_standard_models(url: &str, token: &str) {
    let list = call(url, token, "list_models", json!({})).await;
    let have: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["source"].as_str().map(str::to_lowercase))
        .chain(
            list.as_array()
                .unwrap()
                .iter()
                .filter_map(|m| m["path"].as_str().map(str::to_lowercase)),
        )
        .collect();
    let mut wanted: Vec<String> = STANDARD_MODELS.iter().map(|s| s.to_string()).collect();
    if let Some(p) = local_large_model() {
        wanted.push(p.display().to_string());
    }
    for address in wanted {
        let key = address
            .trim_start_matches("hf.co/")
            .split(':')
            .next()
            .unwrap()
            .to_lowercase();
        if have.iter().any(|h| h.contains(&key)) {
            continue;
        }
        let v = call(
            url,
            token,
            "add_model",
            json!({"address": address, "start": false, "context": "small"}),
        )
        .await;
        let id = v["id"].as_str().unwrap().to_string();
        // Downloads take minutes on a fresh machine.
        for _ in 0..3600 {
            let s = call(url, token, "model_status", json!({"model": id})).await;
            match s["status"].as_str() {
                Some("ready" | "running") => break,
                Some("download_failed" | "failed") => panic!("setting up {address} failed: {s}"),
                _ => tokio::time::sleep(Duration::from_secs(1)).await,
            }
        }
    }
}

async fn call(url: &str, token: &str, op: &str, input: Value) -> Value {
    let r = reqwest::Client::new()
        .post(format!("{url}/api/v1/ops/{op}"))
        .bearer_auth(token)
        .header("x-ancilo-confirm", "true")
        .json(&input)
        .send()
        .await
        .unwrap();
    let status = r.status();
    let body: Value = r.json().await.unwrap();
    assert!(status.is_success(), "{op} failed: {body}");
    body
}

/// Waits until the model runs – and, for chat models, until its speed was measured.
async fn wait_running(url: &str, token: &str, model: &str) -> Value {
    for _ in 0..1800 {
        let v = call(url, token, "model_status", json!({"model": model})).await;
        let measured = v["embedding"] == true || v["instance"]["tokens_per_sec"].is_number();
        match v["status"].as_str() {
            Some("running") if measured => return v,
            Some("failed" | "download_failed") => panic!("model failed: {v}"),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("model {model} did not start in time");
}

// covers: M1-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn real_llama_cpp_loads_answers_embeds_and_stops() {
    // A home of its own (one daemon per home); model files and the llama.cpp
    // build of the shared real-model home are reused, not downloaded again.
    let paths = Paths::from_home(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/real-model-home-m1"),
    );
    paths.ensure().unwrap();
    let shared = home();
    let _ = std::fs::remove_dir_all(paths.home().join("bin"));
    #[cfg(unix)]
    std::os::unix::fs::symlink(shared.home().join("bin"), paths.home().join("bin")).ok();
    let config = Config {
        port: 0,
        model_search_dirs: Some(vec![shared.home().join("models")]),
        ..Config::default()
    };
    let d = ancilo_daemon::start(paths, config, DaemonOptions::default())
        .await
        .unwrap();
    let (url, token) = (d.url().to_string(), d.token.clone());

    // A tiny Qwen chat model.
    let chat = call(
        &url,
        &token,
        "add_model",
        json!({"address": "hf.co/unsloth/Qwen3-0.6B-GGUF:Q4_0", "context": "small"}),
    )
    .await;
    let chat_id = chat["id"].as_str().unwrap().to_string();
    let running = wait_running(&url, &token, &chat_id).await;
    assert!(
        running["instance"]["tokens_per_sec"]
            .as_f64()
            .unwrap_or(0.0)
            > 0.0,
        "{running}"
    );
    let ep = d.manager.endpoint(&chat_id).await.expect("endpoint");
    let answer: Value = reqwest::Client::new()
        .post(format!("{}/chat/completions", ep.base))
        .bearer_auth(&ep.key)
        .json(&json!({"messages": [{"role": "user", "content": "Reply with the single word: hello"}], "max_tokens": 64}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = answer["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !text.is_empty() || answer["choices"][0]["message"]["reasoning_content"].is_string(),
        "{answer}"
    );
    // The model process refuses requests without its API key.
    let unauth = reqwest::Client::new()
        .post(format!("{}/chat/completions", ep.base))
        .json(&json!({"messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status().as_u16(), 401);

    // A tiny embedding model.
    let emb = call(&url, &token, "add_model", json!({"address": "hf.co/second-state/All-MiniLM-L6-v2-Embedding-GGUF:Q4_K_M", "context": "small"})).await;
    let emb_id = emb["id"].as_str().unwrap().to_string();
    let running = wait_running(&url, &token, &emb_id).await;
    assert_eq!(running["embedding"], true, "{running}");
    assert!(
        running["roles"]
            .as_array()
            .unwrap()
            .contains(&json!("embed")),
        "{running}"
    );
    let ep = d.manager.endpoint(&emb_id).await.expect("endpoint");
    let v: Value = reqwest::Client::new()
        .post(format!("{}/embeddings", ep.base))
        .bearer_auth(&ep.key)
        .json(&json!({"input": ["local models made simple"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        v["data"][0]["embedding"]
            .as_array()
            .is_some_and(|e| e.len() >= 64),
        "{v}"
    );

    for id in [&chat_id, &emb_id] {
        let v = call(&url, &token, "stop_model", json!({"model": id})).await;
        assert_eq!(v["status"], "ready");
    }
    d.stop().await;
}

// ---- M2: reliability gain and agent integration on real models -------------

/// Uses a daemon already running on the real-model home, or starts one.
async fn connect_or_start() -> (String, String, Option<ancilo_daemon::DaemonHandle>) {
    let paths = home();
    paths.ensure().unwrap();
    if let Some(info) = ancilo_daemon::DaemonInfo::read(&paths)
        && reqwest::get(format!("{}/api/v1/health", info.url))
            .await
            .is_ok_and(|r| r.status().is_success())
    {
        let token = std::fs::read_to_string(paths.token_file())
            .unwrap()
            .trim()
            .to_string();
        ensure_standard_models(&info.url, &token).await;
        standard_resources(&info.url, &token).await;
        return (info.url, token, None);
    }
    let config = Config {
        port: 0,
        // The daemon runs inside this test; MCP clients (the Claude plugin,
        // Codex) must start the real `ancilo`, not the test binary.
        ancilo_bin: Some(env!("CARGO_BIN_EXE_ancilo").into()),
        ..Config::default()
    };
    let d = ancilo_daemon::start(paths, config, DaemonOptions::default())
        .await
        .unwrap();
    ensure_standard_models(d.url(), &d.token).await;
    standard_resources(d.url(), &d.token).await;
    (d.url().to_string(), d.token.clone(), Some(d))
}

/// The standard level ("balanced"): models load only into free memory and
/// nothing is preloaded at start – the tests run on a working computer.
/// (Set explicitly: an earlier run may have left another level.)
async fn standard_resources(url: &str, token: &str) {
    call(url, token, "set_resources", json!({"level": "balanced"})).await;
}

/// Chat models that are downloaded, largest first (waits for models that
/// are still starting, e.g. right after a daemon restart).
async fn chat_models(url: &str, token: &str) -> Vec<(String, u64)> {
    let mut list = call(url, token, "list_models", json!({})).await;
    for _ in 0..300 {
        if !list
            .as_array()
            .unwrap()
            .iter()
            .any(|m| matches!(m["status"].as_str(), Some("starting" | "downloading")))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        list = call(url, token, "list_models", json!({})).await;
    }
    let mut models: Vec<(String, u64)> = list
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            m["embedding"] == false && matches!(m["status"].as_str(), Some("ready" | "running"))
        })
        // Large models only on request (see `local_large_model`).
        .filter(|m| {
            std::env::var_os("ANCILO_REAL_LARGE").is_some()
                || m["size_bytes"].as_u64().unwrap_or(0) <= LARGE_BYTES
        })
        .map(|m| {
            (
                m["id"].as_str().unwrap().to_string(),
                m["size_bytes"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    models.sort_by_key(|m| std::cmp::Reverse(m.1));
    models
}

// covers: M2-AC-04, M2-AC-05
/// The central bet: the reliability pipeline makes local models measurably
/// better at tool calling – without unwanted tool calls and without an
/// unreasonable latency cost. Protocol (decision 2026-09-30-m2-reliability,
/// after Codex's counter-review): tune on the development sets, then compare
/// the chosen pipeline with "off" on the untouched holdout sets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn reliability_pipeline_improves_real_models() {
    let (url, token, d) = connect_or_start().await;
    let models = chat_models(&url, &token).await;
    assert!(
        !models.is_empty(),
        "no downloaded chat model in the real-model home"
    );
    let mut summary = Vec::new();
    let mut failures = Vec::new();
    for (model, size) in &models {
        let tuned = call(&url, &token, "tune_reliability", json!({"model": model})).await;
        let chosen = tuned["chosen"].as_str().unwrap().to_string();
        let eval = |suite: &'static str, reliability: String| {
            let (url, token, model) = (url.clone(), token.clone(), model.clone());
            async move {
                call(&url, &token, "run_eval", json!({"suite": suite, "model": model, "reliability": reliability, "repeat": 2})).await
            }
        };
        let passed = |r: &Value| -> u64 {
            r["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|t| t["runs"].as_array().unwrap().iter())
                .filter(|x| x["passed"] == true)
                .count() as u64
        };
        let total = |r: &Value| -> u64 {
            r["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["runs"].as_array().unwrap().len() as u64)
                .sum()
        };
        let (tc_off, tc_on) = (
            eval("tool-calling-holdout", "off".into()).await,
            eval("tool-calling-holdout", chosen.clone()).await,
        );
        let (nt_off, nt_on) = (
            eval("no-tool-holdout", "off".into()).await,
            eval("no-tool-holdout", chosen.clone()).await,
        );
        for r in [&tc_off, &tc_on, &nt_off, &nt_on] {
            assert_eq!(r["errors"], 0, "{model}: measurement incomplete: {r}");
        }
        let mean = |r: &Value| r["latency_mean_ms"].as_f64().unwrap();
        summary.push(format!(
            "{model}: chosen `{chosen}` · holdout tool-calling {}/{} → {}/{} · no-tool {}/{} → {}/{} · mean {:.0} → {:.0} ms",
            passed(&tc_off), total(&tc_off), passed(&tc_on), total(&tc_on),
            passed(&nt_off), total(&nt_off), passed(&nt_on), total(&nt_on),
            mean(&tc_off), mean(&tc_on)
        ));
        // Never worse than without the pipeline – on actions and on non-actions.
        if passed(&tc_on) < passed(&tc_off) {
            failures.push(format!("{model}: tool calling got worse"));
        }
        if passed(&nt_on) < passed(&nt_off) {
            failures.push(format!("{model}: more unwanted tool calls"));
        }
        // Small models: a clear gain (M2-AC-04: +10 points or ≥ 95 %).
        if *size < 1_000_000_000 {
            let gain = passed(&tc_on) as i64 - passed(&tc_off) as i64;
            let ten_points = (total(&tc_on) as i64 + 9) / 10;
            if gain < ten_points && passed(&tc_on) * 100 < total(&tc_on) * 95 {
                failures.push(format!(
                    "{model}: gain {gain} of {} runs is below +10 points",
                    total(&tc_on)
                ));
            }
        }
        // Latency: mean at most 1.5 × off + 0.5 s on both sets.
        for (off, on, set) in [
            (&tc_off, &tc_on, "tool-calling"),
            (&nt_off, &nt_on, "no-tool"),
        ] {
            if mean(on) > mean(off) * 1.5 + 500.0 {
                failures.push(format!(
                    "{model}: {set} mean latency {} → {}",
                    mean(off),
                    mean(on)
                ));
            }
        }
    }
    println!("{}", summary.join("\n"));
    if let Some(d) = d {
        d.stop().await;
    }
    assert!(failures.is_empty(), "{failures:#?}\n{}", summary.join("\n"));
}

// covers: M2-AC-06
/// Codex and Claude Code do a reference task through Ancilo's model API.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models and agents: run with `just test-real`"]
async fn codex_and_claude_code_run_on_the_local_model() {
    let (url, token, d) = connect_or_start().await;
    let models = chat_models(&url, &token).await;
    let (model, size) = models.first().cloned().expect("no chat model");
    assert!(
        size >= 2_000_000_000,
        "needs a chat model of at least ~4B parameters in the real-model home (found {model})"
    );
    let work = tempfile::tempdir().unwrap();
    let task = "Reply with exactly this text and nothing else: hello from local";

    // Codex (Responses API).
    let (u, t, m, w) = (
        url.clone(),
        token.clone(),
        model.clone(),
        work.path().to_path_buf(),
    );
    let codex = tokio::task::spawn_blocking(move || {
        std::process::Command::new("codex")
            .args(["exec", "--skip-git-repo-check", "-s", "read-only", "-c", "model_provider=ancilo", "-c"])
            .arg(format!("model_providers.ancilo={{name=\"Ancilo\", base_url=\"{u}/v1\", env_key=\"ANCILO_TOKEN\", wire_api=\"responses\"}}"))
            .args(["-m", &m, task])
            .env("ANCILO_TOKEN", &t)
            .current_dir(&w)
            .output()
            .expect("codex")
    })
    .await
    .unwrap();
    let out = String::from_utf8_lossy(&codex.stdout).to_lowercase();
    assert!(
        out.contains("hello from local"),
        "codex: {}\n{}",
        out,
        String::from_utf8_lossy(&codex.stderr)
    );

    // Claude Code (Anthropic API), isolated from the user's login.
    let cfg = call(
        &url,
        &token,
        "model_api_config",
        json!({"client": "claude_code"}),
    )
    .await;
    let config_dir = tempfile::tempdir().unwrap();
    let (m, w) = (model.clone(), work.path().to_path_buf());
    let env: Vec<(String, String)> = cfg["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let cdir = config_dir.path().to_path_buf();
    let claude = tokio::task::spawn_blocking(move || {
        let mut c = std::process::Command::new("claude");
        for (k, v) in env {
            c.env(k, v);
        }
        c.env("CLAUDE_CONFIG_DIR", &cdir)
            .env("ANTHROPIC_MODEL", &m)
            .env("ANTHROPIC_DEFAULT_HAIKU_MODEL", &m)
            .env("CLAUDE_CODE_MAX_CONTEXT_TOKENS", "131072")
            .args(["-p", task])
            .stdin(std::process::Stdio::null())
            .current_dir(&w)
            .output()
            .expect("claude")
    })
    .await
    .unwrap();
    let out = String::from_utf8_lossy(&claude.stdout).to_lowercase();
    assert!(
        out.contains("hello from local"),
        "claude: {}\n{}",
        out,
        String::from_utf8_lossy(&claude.stderr)
    );
    if let Some(d) = d {
        d.stop().await;
    }
}

// ---- M3: delegation on real models and real agents -------------------------

// covers: M3-AC-10
/// Eval-Set II: 15 real delegated tasks, checked automatically.
/// Threshold from the baseline measurement (see M3 spec, "Erkenntnisse").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn delegation_eval_on_the_best_local_model() {
    let (url, token, d) = connect_or_start().await;
    let models = chat_models(&url, &token).await;
    let (model, _) = models.first().cloned().expect("no chat model");
    let report = call(
        &url,
        &token,
        "run_delegation_eval",
        json!({"model": model, "repeat": 1}),
    )
    .await;
    let rate = report["success_rate"].as_f64().unwrap();
    let failed: Vec<&str> = report["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["success_rate"] != 1.0)
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    println!(
        "{model}: delegation {:.0} % – failed: {failed:?}",
        rate * 100.0
    );
    assert!(
        rate >= DELEGATION_THRESHOLD,
        "{model}: {rate} < {DELEGATION_THRESHOLD}; failed: {failed:?}"
    );
    if let Some(d) = d {
        d.stop().await;
    }
}

/// Minimum success rate on Eval-Set II for the best available local model.
const DELEGATION_THRESHOLD: f64 = 0.6;

// covers: M8-AC-07
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn coding_eval_on_the_best_local_model() {
    let (url, token, d) = connect_or_start().await;
    let models = chat_models(&url, &token).await;
    let (model, _) = models.first().cloned().expect("no chat model");
    let report = call(
        &url,
        &token,
        "run_coding_eval",
        json!({"model": model, "repeat": 1}),
    )
    .await;
    let rate = report["success_rate"].as_f64().unwrap();
    let failed: Vec<String> = report["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["success_rate"] != 1.0)
        .map(|t| {
            format!(
                "{}: {}",
                t["id"].as_str().unwrap(),
                t["runs"][0]["failure"].as_str().unwrap_or("")
            )
        })
        .collect();
    println!(
        "{model}: coding {:.0} % – failed: {failed:#?}",
        rate * 100.0
    );
    assert!(
        rate >= CODING_THRESHOLD,
        "{model}: {rate} < {CODING_THRESHOLD}; failed: {failed:#?}"
    );
    if let Some(d) = d {
        d.stop().await;
    }
}

/// Minimum success rate on the coding eval (M8) for the best local model –
/// from the first measurement (Qwen3.6-35B: 8/8), with room for variance.
const CODING_THRESHOLD: f64 = 0.75;

/// Tasks created while `f` ran, as (task id, model).
async fn tasks_during<F: std::future::Future<Output = ()>>(
    url: &str,
    token: &str,
    f: F,
) -> Vec<String> {
    let before: Vec<String> = call(url, token, "list_tasks", json!({"limit": 1000}))
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["task_id"].as_str().unwrap().to_string())
        .collect();
    f.await;
    call(url, token, "list_tasks", json!({"limit": 1000}))
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["task_id"].as_str().unwrap().to_string())
        .filter(|id| !before.contains(id))
        .collect()
}

fn demo_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("calc.py"),
        "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap();
    }
    dir
}

/// The Ancilo plugin directory, loaded per session with `--plugin-dir`
/// (the user's Claude Code configuration is never modified).
fn plugin_dir(ancilo_home: &std::path::Path) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ancilo"))
        .arg("claude-plugin")
        .env("ANCILO_HOME", ancilo_home)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn run_claude(plugin: &str, dir: &std::path::Path, prompt: &str) -> String {
    let out = std::process::Command::new("claude")
        .args([
            "-p",
            prompt,
            "--plugin-dir",
            plugin,
            "--permission-mode",
            "bypassPermissions",
        ])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// covers: M3-AC-11, M3-AC-13
/// Claude Code (with the Ancilo plugin) delegates suitable work on its own –
/// and keeps unsuitable work. Proven through Ancilo's task log, not Claude's text.
/// Uses the logged-in Claude Code as it is; the plugin is loaded for the
/// session only (`--plugin-dir`), nothing is installed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real agents (costs Claude usage): run with `just test-real`"]
async fn claude_code_delegates_when_it_fits() {
    let (url, token, d) = connect_or_start().await;
    let project = demo_project();
    let plugin = plugin_dir(home().home());
    let p = project.path().to_path_buf();
    let c = plugin.clone();
    let said = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let s2 = said.clone();
    let fitting = tasks_during(&url, &token, async {
        let (p, c) = (p.clone(), c.clone());
        let out = tokio::task::spawn_blocking(move || run_claude(&c, &p, "Write unittest tests for calc.py in test_calc.py. This is routine work – hand it to the local model."))
            .await
            .unwrap();
        *s2.lock().unwrap() = out;
    })
    .await;
    assert!(
        !fitting.is_empty(),
        "Claude Code did not delegate a fitting task – it said: {}",
        said.lock().unwrap()
    );
    let unfitting = tasks_during(&url, &token, async {
        let (p, c) = (p.clone(), c.clone());
        tokio::task::spawn_blocking(move || run_claude(&c, &p, "Should this calculator module become a class hierarchy or stay functions? Give me your architectural judgement in three sentences; do not change files."))
            .await
            .unwrap();
    })
    .await;
    assert!(
        unfitting.is_empty(),
        "Claude Code delegated an architecture question: {unfitting:?}"
    );
    if let Some(d) = d {
        d.stop().await;
    }
}

// covers: M3-AC-12
/// Codex (with the Ancilo MCP server) delegates a fitting task.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real agents (costs Codex usage): run with `just test-real`"]
async fn codex_delegates_when_it_fits() {
    let (url, token, d) = connect_or_start().await;
    let project = demo_project();
    // MCP server for this run only (`-c`), the user's Codex config is untouched.
    let ancilo = env!("CARGO_BIN_EXE_ancilo").to_string();
    let ah = home().home().display().to_string();
    let p = project.path().to_path_buf();
    let created = tasks_during(&url, &token, async move {
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("codex")
                .args(["exec", "--skip-git-repo-check", "-s", "workspace-write", "-c"])
                // `codex exec` cannot ask: this run allows Ancilo's tools up front.
                .arg(format!("mcp_servers.ancilo={{command=\"{ancilo}\", args=[\"mcp\"], env={{ANCILO_HOME=\"{ah}\"}}, default_tools_approval_mode=\"approve\"}}"))
                .arg("Use the ancilo delegate tool to have the local model write unittest tests for calc.py in test_calc.py.")
                .current_dir(&p)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap()
        })
        .await
        .unwrap();
    })
    .await;
    assert!(!created.is_empty(), "Codex did not delegate");
    if let Some(d) = d {
        d.stop().await;
    }
}

// ---- M4: comparisons on real models ----------------------------------------

/// Largest allowed difference of a model's success rate between two identical
/// suite runs (3 of 15 delegation tasks) – see M4 spec, "Erkenntnisse".
const COMPARE_SPREAD: f64 = 0.2;

// covers: M4-AC-12
/// Two real models on the delegation suite, twice: the success rates must be
/// stable enough between runs to carry decisions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn real_comparisons_are_repeatable() {
    let (url, token, d) = connect_or_start().await;
    let models: Vec<String> = chat_models(&url, &token)
        .await
        .into_iter()
        .take(2)
        .map(|m| m.0)
        .collect();
    assert_eq!(models.len(), 2, "needs two downloaded chat models");
    let mut rates: Vec<Vec<(String, f64)>> = Vec::new();
    for _ in 0..2 {
        let started = call(
            &url,
            &token,
            "run_suite",
            json!({"suite": "delegation", "models": models, "repeat": 1}),
        )
        .await;
        let id = started["id"].as_str().unwrap().to_string();
        let report = loop {
            let r = call(
                &url,
                &token,
                "comparison_report",
                json!({"id": id, "wait_s": 30}),
            )
            .await;
            if !matches!(r["status"].as_str(), Some("queued" | "running")) {
                break r;
            }
        };
        assert_eq!(report["status"], "done", "{report}");
        rates.push(
            report["ranking"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| {
                    (
                        a["model"].as_str().unwrap().to_string(),
                        a["success"]["rate"].as_f64().unwrap(),
                    )
                })
                .collect(),
        );
    }
    for (model, first) in &rates[0] {
        let second = rates[1].iter().find(|(m, _)| m == model).unwrap().1;
        println!("{model}: {:.0} % / {:.0} %", first * 100.0, second * 100.0);
        assert!(
            (first - second).abs() <= COMPARE_SPREAD,
            "{model}: {first} vs {second}"
        );
    }
    if let Some(d) = d {
        d.stop().await;
    }
}

// ---- M5: search in real delegation -------------------------------------------

// covers: M5-AC-06
/// Delegation with and without the `search` tool on the best local model:
/// search must help – a better success rate, or the same with fewer tokens –
/// on bugs described in the user's words, hidden among unrelated modules
/// (`evals/delegation-search.yaml`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn search_helps_delegation() {
    let (url, token, d) = connect_or_start().await;
    let (model, _) = chat_models(&url, &token)
        .await
        .first()
        .cloned()
        .expect("no chat model");
    let run = |search: bool| {
        let (url, token, model) = (url.clone(), token.clone(), model.clone());
        async move {
            call(
                &url,
                &token,
                "run_delegation_eval",
                json!({"suite": "delegation-search", "model": model, "repeat": 1, "search": search}),
            )
            .await
        }
    };
    let (with, without) = (run(true).await, run(false).await);
    let tokens = |r: &Value| -> u64 {
        r["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|t| t["runs"].as_array().unwrap().iter())
            .filter_map(|x| x["tokens"].as_u64())
            .sum()
    };
    let rate = |r: &Value| r["success_rate"].as_f64().unwrap();
    println!(
        "{model}: with search {:.0} % ({} tokens), without {:.0} % ({} tokens)",
        rate(&with) * 100.0,
        tokens(&with),
        rate(&without) * 100.0,
        tokens(&without)
    );
    assert!(
        rate(&with) > rate(&without)
            || (rate(&with) >= rate(&without) && tokens(&with) <= tokens(&without)),
        "search did not help"
    );
    if let Some(d) = d {
        d.stop().await;
    }
}

// ---- M6: the assistant on a real model ----------------------------------------

/// Minimum share of Eval-Set III tasks the assistant model must solve – see
/// M6 spec, "Erkenntnisse".
const ASSISTANT_THRESHOLD: f64 = 0.8;

// covers: M6-AC-03, M6-AC-07, M6-AC-10
/// Eval-Set III with the assistant model: requests with checked target states
/// (roles, rules, A/B tests, proposals, knowledge, honesty about missing data).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models: run with `just test-real`"]
async fn the_assistant_operates_ancilo() {
    let (url, token, d) = connect_or_start().await;
    let models = chat_models(&url, &token).await;
    assert!(models.len() >= 2, "needs two chat models");
    // The smallest model that is not tiny: the realistic assistant.
    let assistant = models
        .iter()
        .rev()
        .find(|(_, size)| *size > 1_000_000_000)
        .or(models.last())
        .unwrap()
        .0
        .clone();
    let report = call(
        &url,
        &token,
        "run_assistant_eval",
        json!({"model": assistant}),
    )
    .await;
    let rate = report["success_rate"].as_f64().unwrap();
    let failed: Vec<String> = report["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["passed"] != true)
        .map(|t| format!("{}: {}", t["id"], t["failure"]))
        .collect();
    println!(
        "{assistant}: Eval-Set III {:.0} %\n{}",
        rate * 100.0,
        failed.join("\n")
    );
    assert!(
        rate >= ASSISTANT_THRESHOLD,
        "{rate} < {ASSISTANT_THRESHOLD}: {failed:#?}"
    );
    if let Some(d) = d {
        d.stop().await;
    }
}

// covers: M6-AC-14
/// Web search with real Wikipedia and a small real model: a fact is looked up
/// and answered with its source; writing is not searched (decision
/// `2026-10-02-websuche`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real models and Wikipedia: run with `just test-real`"]
async fn web_search_answers_from_wikipedia_on_a_small_model() {
    let (url, token, d) = connect_or_start().await;
    let list = call(&url, &token, "list_models", json!({})).await;
    let model = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str().is_some_and(|id| id.contains("qwen3.5-4b")))
        .and_then(|m| m["id"].as_str())
        .expect("Qwen3.5-4B in the real-model home")
        .to_string();
    let before = call(&url, &token, "get_web_search", json!({})).await;
    call(
        &url,
        &token,
        "set_web_search",
        json!({"provider": "wikipedia", "mode": "auto"}),
    )
    .await;
    let started = std::time::Instant::now();
    let r = call(
        &url,
        &token,
        "ask",
        json!({"prompt": "Wie hoch ist die Zugspitze?", "model": model, "kind": "chat"}),
    )
    .await;
    let took = started.elapsed();
    eprintln!("{} in {took:?}: {}", r["web"]["query"], r["answer"]);
    assert_eq!(r["web"]["state"], "searched", "{r}");
    assert!(r["answer"].as_str().unwrap().contains("2962"), "{r}");
    assert!(
        r["answer"].as_str().unwrap().contains("[1]"),
        "cites its source: {r}"
    );
    assert!(
        r["web"]["sources"][0]["url"]
            .as_str()
            .unwrap()
            .contains("wikipedia.org/wiki/Zugspitze"),
        "{r}"
    );
    let w = call(&url, &token, "ask", json!({"prompt": "Schreib mir ein kurzes Gedicht über den Herbst.", "model": model, "kind": "chat"})).await;
    assert!(w["web"].is_null(), "writing is not searched: {w}");
    call(
        &url,
        &token,
        "set_web_search",
        json!({"provider": before["provider"], "mode": before["mode"]}),
    )
    .await;
    if let Some(d) = d {
        d.stop().await;
    }
}
