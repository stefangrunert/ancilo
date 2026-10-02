//! Contract tests: the official OpenAI and Anthropic SDKs (Node) against the
//! model API. Requires `npm ci --prefix tests/contract` (done by `just verify`).

use std::path::PathBuf;
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::DaemonOptions;
use ancilo_models::ManagerOptions;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use serde_json::json;

/// Steps in the order of the calls in `tests/contract/contract.mjs`.
const SCRIPT: &str = r#"
steps:
  - respond: { text: "Hello from Ancilo" }
  - respond: { text: "streamed answer here" }
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "main.rs" } }] }
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "lib.rs" } }] }
  - respond: { http_error: { status: 500, message: "boom" } }
  - respond: { text: "Hi from the Anthropic path" }
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "main.rs" } }] }
  - expect: { any_message_contains: "fn main() {}" }
    respond: { text: "It has one function." }
  - respond: { text: "streaming works" }
  - respond: { tool_calls: [{ name: read_file, arguments: { path: "lib.rs" } }] }
  - respond: { http_error: { status: 500, message: "boom" } }
"#;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// covers: M2-AC-01, M2-AC-02
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn official_sdks_work_against_the_model_api() {
    let contract = repo_root().join("tests/contract");
    assert!(
        contract.join("node_modules/openai").is_dir()
            && contract.join("node_modules/@anthropic-ai/sdk").is_dir(),
        "SDKs missing – run `npm ci --prefix tests/contract` (part of `just verify`)"
    );
    let home = TestHome::new();
    let hf = FakeHf::start(vec![
        FakeRepo::new(
            "o/M-GGUF",
            vec![FakeFile::gguf("M-Q8_0.gguf", "qwen3", 32768, 100_000)],
        )
        .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 32768})),
    ])
    .await;
    let script = home.scratch("s").join("script.yaml");
    std::fs::write(&script, SCRIPT).unwrap();
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
        llama_server_env: [("FAKE_LLM_SCRIPT".to_string(), script.display().to_string())]
            .into_iter()
            .collect(),
        ..home.config()
    };
    let options = DaemonOptions {
        manager: ManagerOptions {
            measure_speed: false,
            restart_backoff: Duration::from_millis(50),
            ..Default::default()
        },
        llama_build: Some(None),
        ..Default::default()
    };
    let d = ancilo_daemon::start(home.paths.clone(), config, options)
        .await
        .unwrap();
    d.manager
        .add("o/M-GGUF", &Default::default(), true)
        .await
        .unwrap();
    let (url, token) = (d.url().to_string(), d.token.clone());
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("node")
            .arg("contract.mjs")
            .arg(&url)
            .arg(&token)
            .current_dir(&contract)
            .output()
            .expect("node")
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    d.stop().await;
    println!("{stdout}");
    assert!(
        stdout.matches("\nok ").count() + usize::from(stdout.starts_with("ok ")) >= 20,
        "too few checks ran:\n{stdout}"
    );
    assert!(
        out.status.success(),
        "contract checks failed:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("ALL CONTRACT CHECKS PASSED"), "{stdout}");
}
