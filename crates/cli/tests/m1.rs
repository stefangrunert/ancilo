//! M1 end-to-end: the real `ancilo` binary against an in-process daemon with
//! fake Hugging Face and a fake `llama-server`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::ManagerOptions;
use ancilo_models::download::DownloadOptions;
use ancilo_models::hardware::{GIB, Gpu, HardwareProfile};
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use serde_json::{Value, json};

struct Env {
    home: TestHome,
    hf: FakeHf,
    daemon: Option<DaemonHandle>,
}

impl Env {
    async fn start(repos: Vec<FakeRepo>, hw: HardwareProfile, search_dirs: Vec<PathBuf>) -> Self {
        let home = TestHome::new();
        let hf = FakeHf::start(repos).await;
        let hw_file = home.scratch("hw").join("hw.json");
        std::fs::write(&hw_file, serde_json::to_string(&hw).unwrap()).unwrap();
        let config = Config {
            hf_endpoint: hf.url(),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(search_dirs),
            hardware_override: Some(hw_file),
            ..home.config()
        };
        home.write_config(&config);
        let options = DaemonOptions {
            manager: ManagerOptions {
                download: DownloadOptions {
                    max_attempts: 3,
                    base_backoff: Duration::from_millis(20),
                    progress_interval: Duration::from_millis(10),
                },
                restart_backoff: Duration::from_millis(50),
                measure_speed: true,
                ..Default::default()
            },
            llama_build: Some(None),
            ..Default::default()
        };
        let daemon = ancilo_daemon::start(home.paths.clone(), config, options)
            .await
            .unwrap();
        Self {
            home,
            hf,
            daemon: Some(daemon),
        }
    }

    /// Runs the real CLI binary (on a blocking thread).
    async fn cli(&self, args: &[&str]) -> (i32, String, String) {
        let home = self.home.path().to_path_buf();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("ancilo"))
                .args(&args)
                .env("ANCILO_HOME", &home)
                .env_remove("ANCILO_PORT")
                .output()
                .unwrap();
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        })
        .await
        .unwrap()
    }

    async fn json(&self, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.push("--json");
        let (code, out, err) = self.cli(&a).await;
        assert_eq!(code, 0, "ancilo {args:?} failed: {err}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not JSON ({e}): {out}"))
    }

    async fn rest(&self, op: &str, input: Value) -> reqwest::Response {
        let d = self.daemon.as_ref().unwrap();
        reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{op}", d.url()))
            .bearer_auth(&d.token)
            .json(&input)
            .send()
            .await
            .unwrap()
    }

    async fn stop(mut self) {
        if let Some(d) = self.daemon.take() {
            d.stop().await;
        }
    }
}

fn repo(id: &str, quants: &[&str]) -> FakeRepo {
    let name = id
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches("-GGUF")
        .to_string();
    FakeRepo::new(
        id,
        quants
            .iter()
            .enumerate()
            .map(|(i, q)| {
                FakeFile::gguf(
                    &format!("{name}-{q}.gguf"),
                    "qwen3",
                    32768,
                    200_000 + i * 50_000,
                )
            })
            .chain(std::iter::once(FakeFile::new(
                "README.md",
                b"# model".to_vec(),
            )))
            .collect(),
    )
    .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 32768}))
}

fn qwen_test_repo() -> FakeRepo {
    repo("unsloth/Qwen-Test-GGUF", &["Q4_K_M", "Q8_0"])
}

// covers: M1-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_command_brings_a_model_from_nothing_to_running() {
    let env = Env::start(vec![qwen_test_repo()], HardwareProfile::apple(64), vec![]).await;
    let (code, _out, err) = env.cli(&["add", "hf.co/unsloth/Qwen-Test-GGUF"]).await;
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("Qwen-Test-Q8_0.gguf"), "plan shown: {err}");
    assert!(err.contains("is running"), "{err}");
    let list = env.json(&["list"]).await;
    let m = &list[0];
    assert_eq!(m["status"], "running");
    assert_eq!(m["quant"], "Q8_0");
    assert_eq!(m["roles"], json!(["default"]));
    assert!(m["instance"]["tokens_per_sec"].as_f64().unwrap() > 0.0);
    assert!(
        m["path"]
            .as_str()
            .unwrap()
            .starts_with(env.home.paths.models_dir().to_str().unwrap())
    );
    // Adding again is idempotent.
    let (code, _, _) = env.cli(&["add", "hf.co/unsloth/Qwen-Test-GGUF"]).await;
    assert_eq!(code, 0);
    assert_eq!(env.json(&["list"]).await.as_array().unwrap().len(), 1);
    env.stop().await;
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

// covers: M1-AC-06
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn existing_model_files_are_used_instead_of_downloading() {
    let lm_repo = repo("unsloth/Lm-GGUF", &["Q8_0"]);
    let hf_repo = repo("org/Cached-GGUF", &["Q8_0"]);
    let ol_repo = repo("org/Ollama-GGUF", &["Q8_0"]);
    let scratch = tempfile::tempdir().unwrap();
    // LM Studio layout.
    let lm = scratch.path().join("lmstudio");
    write(
        &lm.join("unsloth/Lm-GGUF/Lm-Q8_0.gguf"),
        &lm_repo.files[0].content,
    );
    // Hugging Face cache layout (snapshot symlink → blob).
    let hub = scratch.path().join("hub");
    let blob = hub.join("models--org--Cached-GGUF/blobs/b1");
    write(&blob, &hf_repo.files[0].content);
    std::fs::create_dir_all(hub.join("models--org--Cached-GGUF/snapshots/r1")).unwrap();
    std::os::unix::fs::symlink(
        &blob,
        hub.join("models--org--Cached-GGUF/snapshots/r1/Cached-Q8_0.gguf"),
    )
    .unwrap();
    // Ollama layout (blob named by SHA-256).
    let ol = scratch.path().join("ollama");
    let sha = ol_repo.files[0].sha256();
    write(
        &ol.join(format!("blobs/sha256-{sha}")),
        &ol_repo.files[0].content,
    );
    write(
        &ol.join("manifests/hf.co/org/Ollama-GGUF/Q8_0"),
        json!({"layers": [{"mediaType": "application/vnd.ollama.image.model", "digest": format!("sha256:{sha}")}]}).to_string().as_bytes(),
    );

    let env = Env::start(
        vec![lm_repo, hf_repo, ol_repo],
        HardwareProfile::apple(64),
        vec![lm, hub, ol],
    )
    .await;
    for (address, file, tool) in [
        ("hf.co/unsloth/Lm-GGUF", "Lm-Q8_0.gguf", "lm_studio"),
        ("hf.co/org/Cached-GGUF", "Cached-Q8_0.gguf", "hf_cache"),
        ("hf.co/org/Ollama-GGUF", "Ollama-Q8_0.gguf", "ollama"),
    ] {
        let plan = env.json(&["plan", address]).await;
        assert_eq!(plan["existing_in"], tool, "{address}");
        assert_eq!(plan["download_bytes"], 0);
        let (code, _, err) = env.cli(&["add", address, "--no-start"]).await;
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("existing file"), "{err}");
        assert_eq!(
            env.hf.downloads_of(file),
            0,
            "{file} must not be downloaded"
        );
    }
    let list = env.json(&["list"]).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|m| m["status"] == "ready")
    );
    // Removing never deletes files of other tools.
    let (code, out, _) = env.cli(&["remove", "lm-q8_0"]).await;
    assert_eq!(code, 0);
    assert!(!out.contains("deleted"));
    assert!(
        scratch
            .path()
            .join("lmstudio/unsloth/Lm-GGUF/Lm-Q8_0.gguf")
            .exists()
    );
    env.stop().await;
}

// covers: M1-AC-08
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_operation_is_reachable_via_rest_and_cli() {
    let env = Env::start(vec![], HardwareProfile::apple(16), vec![]).await;
    let d = env.daemon.as_ref().unwrap();
    let ops: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/ops", d.url()))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<String> = ops
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "add_model",
        "list_models",
        "start_model",
        "stop_model",
        "remove_model",
        "plan_model",
        "hardware_info",
        "resolve_address",
        "model_status",
        "assign_role",
        "daemon_info",
        "daemon_shutdown",
    ] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    for name in names.iter().filter(|n| *n != "daemon_shutdown") {
        let r = env.rest(name, json!({})).await;
        let body: Value = r.json().await.unwrap_or(Value::Null);
        assert_ne!(
            body["error"]["message"]
                .as_str()
                .map(|m| m.contains("unknown operation")),
            Some(true),
            "REST {name}"
        );
        let (_, _, err) = env.cli(&["op", name]).await;
        assert!(!err.contains("unknown operation"), "CLI {name}: {err}");
    }
    let (code, out, _) = env.cli(&["op", "daemon_shutdown"]).await;
    assert_eq!(code, 0, "{out}");
    env.stop().await;
}

// covers: M1-AC-11
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn several_models_can_be_installed_and_run_side_by_side() {
    let repos = vec![
        repo("o/Alpha-GGUF", &["Q8_0"]),
        repo("o/Beta-GGUF", &["Q8_0"]),
        repo("o/Gamma-GGUF", &["Q8_0"]),
    ];
    let env = Env::start(repos, HardwareProfile::apple(64), vec![]).await;
    for r in ["o/Alpha-GGUF", "o/Beta-GGUF", "o/Gamma-GGUF"] {
        let (code, _, err) = env.cli(&["add", r, "--no-start"]).await;
        assert_eq!(code, 0, "{err}");
    }
    assert_eq!(env.json(&["list"]).await.as_array().unwrap().len(), 3);
    for m in ["alpha", "beta"] {
        let (code, _, err) = env.cli(&["start", m]).await;
        assert_eq!(code, 0, "{err}");
    }
    let status = |list: &Value, id: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()["status"]
            .clone()
    };
    let list = env.json(&["list"]).await;
    assert_eq!(status(&list, "alpha-q8_0"), "running");
    assert_eq!(status(&list, "beta-q8_0"), "running");
    assert_eq!(status(&list, "gamma-q8_0"), "ready");
    let (code, _, _) = env.cli(&["stop", "alpha"]).await;
    assert_eq!(code, 0);
    let file = env.json(&["status", "beta"]).await["path"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, out, _) = env.cli(&["remove", "beta"]).await;
    assert_eq!(code, 0);
    assert!(out.contains("deleted"));
    assert!(!Path::new(&file).exists());
    let list = env.json(&["list"]).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(status(&list, "alpha-q8_0"), "ready");
    env.stop().await;
}

// covers: M1-AC-12
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_memory_budget_is_never_exceeded() {
    // 1 GiB for models: one small model fits, two do not.
    let hw = HardwareProfile {
        total_ram_bytes: 2 * GIB,
        gpu: Gpu::Metal,
        gpu_memory_bytes: Some(GIB),
        ..HardwareProfile::apple(2)
    };
    let env = Env::start(
        vec![repo("o/A-GGUF", &["Q8_0"]), repo("o/B-GGUF", &["Q8_0"])],
        hw,
        vec![],
    )
    .await;
    let (code, _, err) = env.cli(&["add", "o/A-GGUF", "--context", "small"]).await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = env
        .cli(&["add", "o/B-GGUF", "--context", "small", "--no-start"])
        .await;
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = env.cli(&["start", "b-q8_0"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("Stop another model first"), "{err}");
    assert!(err.contains("a-q8_0 loaded"), "{err}");
    let hw = env.json(&["hardware"]).await;
    assert!(hw["used_bytes"].as_u64().unwrap() <= hw["model_budget_bytes"].as_u64().unwrap());
    // After stopping A, B fits.
    env.cli(&["stop", "a-q8_0"]).await;
    let (code, _, err) = env.cli(&["start", "b-q8_0"]).await;
    assert_eq!(code, 0, "{err}");
    env.stop().await;
}

// covers: M1-AC-10
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typical_errors_are_explained() {
    let env = Env::start(
        vec![repo("o/Huge-GGUF", &["Q8_0"])],
        HardwareProfile {
            gpu_memory_bytes: Some(GIB / 4),
            ..HardwareProfile::apple(1)
        },
        vec![],
    )
    .await;
    let (code, _, err) = env.cli(&["add", "o/Huge-GGUF"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("does not fit"), "{err}");
    assert!(err.contains("hint:"), "{err}");
    let (code, _, err) = env.cli(&["add", "hf.co/o/Missing-GGUF"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("not found on Hugging Face"), "{err}");
    let (code, _, err) = env.cli(&["add", "qwen"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("not a model address"), "{err}");
    env.stop().await;
}
