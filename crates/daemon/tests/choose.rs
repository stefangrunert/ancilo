//! Choosing a model without knowing anything about models: recommendations
//! for this machine from Ancilo's list, a newer list fetched on request,
//! and the search on Hugging Face.

use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use serde_json::{Value, json};

struct Env {
    _home: TestHome,
    hf: FakeHf,
    d: Option<DaemonHandle>,
}

impl Env {
    async fn start(ram_gib: u64, catalog: Option<String>) -> Self {
        let home = TestHome::new();
        let repos = vec![
            FakeRepo::new(
                "unsloth/Qwen3.5-4B-GGUF",
                vec![FakeFile::gguf(
                    "Qwen3.5-4B-Q4_K_M.gguf",
                    "qwen35",
                    4096,
                    200_000,
                )],
            ),
            FakeRepo::new(
                "unsloth/Qwen3.5-9B-GGUF",
                vec![FakeFile::gguf(
                    "Qwen3.5-9B-Q4_K_M.gguf",
                    "qwen35",
                    4096,
                    200_000,
                )],
            ),
            FakeRepo::new(
                "someone/Tiny-Coder-GGUF",
                vec![FakeFile::gguf(
                    "Tiny-Coder-Q4_K_M.gguf",
                    "qwen3",
                    4096,
                    100_000,
                )],
            ),
        ];
        let raw = catalog
            .map(|c| vec![FakeFile::new("ancilo/catalog.json", c.into_bytes())])
            .unwrap_or_default();
        let hf = FakeHf::start_with_raw(repos, raw).await;
        let hw = home.scratch("hw").join("hw.json");
        std::fs::write(
            &hw,
            serde_json::to_string(&HardwareProfile::apple(ram_gib)).unwrap(),
        )
        .unwrap();
        let config = Config {
            port: 0,
            hf_endpoint: hf.url(),
            catalog_url: format!("{}/ancilo/catalog.json", hf.url()),
            llama_server_bin: Some(fake_llama_server_bin()),
            model_search_dirs: Some(vec![]),
            hardware_override: Some(hw),
            ..home.config()
        };
        let options = DaemonOptions {
            llama_build: Some(None),
            ..Default::default()
        };
        let d = ancilo_daemon::start(home.paths.clone(), config, options)
            .await
            .unwrap();
        Self {
            _home: home,
            hf,
            d: Some(d),
        }
    }

    async fn call(&self, name: &str, input: Value, confirm: bool) -> (bool, Value) {
        let d = self.d.as_ref().unwrap();
        let mut r = reqwest::Client::new()
            .post(format!("{}/api/v1/ops/{name}", d.url()))
            .bearer_auth(&d.token)
            .json(&input);
        if confirm {
            r = r.header("x-ancilo-confirm", "true");
        }
        let r = r.send().await.unwrap();
        (
            r.status().is_success(),
            r.json().await.unwrap_or(Value::Null),
        )
    }

    async fn op(&self, name: &str, input: Value) -> Value {
        let (ok, v) = self.call(name, input, true).await;
        assert!(ok, "{name}: {v}");
        v
    }

    async fn stop(mut self) {
        self.d.take().unwrap().stop().await;
    }
}

// covers: M1-AC-13
/// What fits this machine, for what the user wants – without a word about
/// quantization; and nothing goes over the network unless asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recommendations_fit_the_machine_and_the_purpose() {
    let env = Env::start(16, None).await;
    let r = env.op("recommend_models", json!({})).await;
    assert_eq!(r["purposes"], json!(["chat"]));
    let best = &r["best"];
    assert_eq!(best["room"], "comfortable", "{r}");
    assert_ne!(best["speed"], "slow", "{r}");
    assert!(best["address"].as_str().unwrap().starts_with("hf.co/"));
    assert!(best["download_minutes"].as_u64().unwrap() >= 1);
    assert!(best["summary"]["de"].as_str().unwrap().len() > 10);
    assert!(
        r["too_big"].as_u64().unwrap() > 0,
        "16 GB cannot run everything"
    );
    assert!(r["alternatives"].as_array().unwrap().len() <= 3);
    assert_eq!(r["catalog"]["source"], "bundled");
    assert_eq!(r["memory"]["total_bytes"], 16u64 << 30);
    // The list was not fetched: no request left without being asked.
    assert!(env.hf.requests().is_empty(), "{:?}", env.hf.requests());
    // Documents bring the embedding model along.
    let docs = env
        .op("recommend_models", json!({"purposes": ["documents"]}))
        .await;
    assert!(
        docs["embedding"]["address"]
            .as_str()
            .unwrap()
            .contains("All-MiniLM")
    );
    // Unknown purposes are refused, not guessed.
    let (ok, _) = env
        .call("recommend_models", json!({"purposes": ["games"]}), false)
        .await;
    assert!(!ok);
    // The first-start setup uses the same choice (here: the first
    // recommendation the fake Hugging Face has).
    let plan = env.op("setup", json!({"dry_run": true})).await;
    let chat = plan["chat"]["repo"]["id"].as_str().unwrap();
    assert!(chat.starts_with("unsloth/Qwen3.5-"), "{plan}");
    env.stop().await;
}

/// A suggestion's address adds exactly that model; afterwards it is
/// recommended as installed (nothing to download).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_suggestion_installs_with_one_call() {
    let env = Env::start(8, None).await;
    let r = env.op("recommend_models", json!({})).await;
    // 8 GB: the 4B model in Q4.
    assert_eq!(r["best"]["id"], "qwen3.5-4b", "{r}");
    let address = r["best"]["address"].as_str().unwrap().to_string();
    assert!(address.ends_with(":Q4_K_M"), "{address}");
    let added = env
        .op("add_model", json!({"address": address, "start": false}))
        .await;
    let id = added["id"].as_str().unwrap().to_string();
    for _ in 0..400 {
        if env.op("model_status", json!({"model": id})).await["status"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let r = env.op("recommend_models", json!({})).await;
    assert_eq!(r["best"]["installed"], id.as_str(), "{r}");
    assert_eq!(r["best"]["download_bytes"], 0);
    env.stop().await;
}

/// A newer list from the Ancilo repository is used – only when asked for,
/// kept for later, and a broken or missing one changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newer_model_list_is_fetched_on_request() {
    let mut catalog: Value = serde_json::from_str(ancilo_models::catalog::BUNDLED).unwrap();
    catalog["updated"] = "2099-01-01".into();
    catalog["models"] = json!([{
        "id": "tiny-coder", "name": "Tiny Coder", "address": "hf.co/someone/Tiny-Coder-GGUF",
        "maker": "Someone", "kind": "chat", "purposes": ["chat", "code"], "params_b": 1.0,
        "active_params_b": 1.0, "quality": {"chat": 4, "code": 5}, "sizes": {"Q4_K_M": 100000},
        "license": "mit", "released": "2099-01", "summary": {"de": "Klein.", "en": "Small."}
    }]);
    let env = Env::start(16, Some(catalog.to_string())).await;
    let before = env.op("recommend_models", json!({})).await;
    assert_eq!(before["catalog"]["source"], "bundled");
    let after = env.op("recommend_models", json!({"refresh": true})).await;
    assert_eq!(after["catalog"]["source"], "online", "{after}");
    assert_eq!(after["catalog"]["updated"], "2099-01-01");
    assert_eq!(after["best"]["id"], "tiny-coder");
    // Kept: the next call needs no network.
    let n = env.hf.requests().len();
    let again = env.op("recommend_models", json!({})).await;
    assert_eq!(again["best"]["id"], "tiny-coder");
    assert_eq!(env.hf.requests().len(), n);
    env.stop().await;

    // A broken list is refused; the bundled one stays.
    let env = Env::start(16, Some("{\"version\": 1}".into())).await;
    let r = env.op("recommend_models", json!({"refresh": true})).await;
    assert_eq!(r["catalog"]["source"], "bundled");
    assert!(r["catalog"]["error"].as_str().is_some(), "{r}");
    assert!(r["best"].is_object());
    env.stop().await;
}

/// Models outside the list: the search on Hugging Face, and a hit planned
/// like any other address.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hugging_face_can_be_searched() {
    let env = Env::start(16, None).await;
    let hits = env
        .op("search_models", json!({"query": "tiny coder"}))
        .await;
    let hits = hits.as_array().unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["address"], "hf.co/someone/Tiny-Coder-GGUF");
    let plan = env
        .op("plan_model", json!({"address": hits[0]["address"]}))
        .await;
    assert_eq!(plan["plan"]["fit"], "fits");
    let qwen = env.op("search_models", json!({"query": "qwen3.5"})).await;
    assert_eq!(qwen.as_array().unwrap().len(), 2);
    let (ok, _) = env
        .call("search_models", json!({"query": " "}), false)
        .await;
    assert!(!ok);
    env.stop().await;
}
