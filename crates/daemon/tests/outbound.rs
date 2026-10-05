//! The user's log of what left this computer is complete: every request the
//! servers out there received – Hugging Face, the catalog, Wikipedia, a
//! cloud model – stands in it, with what was sent and never a key. What
//! stays on this computer (the local model) does not.
//!
//! The fakes answer under names (`hf.test`, `wiki.test`, `cloud.test`), as
//! servers out there would; `web_hosts` maps them to this computer.

use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_testkit::{
    FakeFile, FakeHf, FakeLlm, FakeRepo, FakeWeb, Script, TestHome, fake_llama_server_bin,
};
use serde_json::{Value, json};

const REPO: &str = "someone/Tiny-GGUF";

async fn op(d: &DaemonHandle, name: &str, input: Value) -> Value {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v1/ops/{name}", d.url()))
        .bearer_auth(&d.token)
        .header("x-ancilo-confirm", "true")
        .json(&input)
        .send()
        .await
        .unwrap();
    let ok = r.status().is_success();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    assert!(ok, "{name}: {v}");
    v
}

fn named(url: &str, name: &str) -> String {
    url.replace("127.0.0.1", name)
}

// covers: M11-AC-01
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn everything_that_left_this_computer_is_in_the_log() {
    let home = TestHome::new();
    let hf = FakeHf::start(vec![FakeRepo::new(
        REPO,
        vec![
            FakeFile::gguf("Tiny-Q4_K_M.gguf", "qwen3", 4096, 100_000),
            FakeFile::new("README.md", b"# Tiny\n".to_vec()),
        ],
    )])
    .await;
    let web = FakeWeb::start().await;
    web.article("de", "Oslo", "Oslo ist die Hauptstadt Norwegens.");
    let cloud = FakeLlm::start(
        Script::from_yaml("model: some-cloud\nsteps: []\nfallback: { text: \"Hallo!\" }").unwrap(),
    )
    .await;
    let here: std::net::IpAddr = [127, 0, 0, 1].into();
    let config = Config {
        port: 0,
        hf_endpoint: named(&hf.url(), "hf.test"),
        catalog_url: format!("{}/ancilo/catalog.json", named(&hf.url(), "hf.test")),
        wikipedia_endpoint: named(&web.wikipedia(), "wiki.test"),
        llama_server_bin: Some(fake_llama_server_bin()),
        model_search_dirs: Some(vec![]),
        web_hosts: [("hf.test", here), ("wiki.test", here), ("cloud.test", here)]
            .into_iter()
            .map(|(h, ip)| (h.to_string(), ip))
            .collect(),
        ..home.config()
    };
    let options = DaemonOptions {
        llama_build: Some(None),
        ..Default::default()
    };
    let d = ancilo_daemon::start(home.paths.clone(), config, options)
        .await
        .unwrap();

    // Models: a search, a download (its facts, its file, its description).
    op(&d, "search_models", json!({"query": "tiny"})).await;
    let added = op(
        &d,
        "add_model",
        json!({"address": format!("hf.co/{REPO}:Q4_K_M"), "start": true}),
    )
    .await;
    let id = added["id"].as_str().unwrap().to_string();
    for _ in 0..400 {
        if op(&d, "model_status", json!({"model": id})).await["status"] == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // The catalog of recommended models.
    op(
        &d,
        "recommend_models",
        json!({"purposes": ["chat"], "refresh": true}),
    )
    .await;
    // A web search.
    op(
        &d,
        "set_web_search",
        json!({"provider": "wikipedia", "mode": "auto"}),
    )
    .await;
    op(
        &d,
        "web_search",
        json!({"query": "Oslo Einwohner", "topic": "Oslo", "lang": "de"}),
    )
    .await;
    // A cloud model: set up, then a message to it; a local chat stays here.
    let c = op(
        &d,
        "set_cloud_provider",
        json!({"base_url": named(&format!("{}/v1", cloud.url()), "cloud.test"), "model": "some-cloud", "api_key": "SECRET-PROVIDER-KEY"}),
    )
    .await;
    let cloud_id = c["model"]["id"].as_str().unwrap().to_string();
    op(
        &d,
        "ask",
        json!({"prompt": "Sag Hallo", "model": cloud_id, "kind": "chat"}),
    )
    .await;
    op(
        &d,
        "ask",
        json!({"prompt": "Sag Hallo", "model": id, "kind": "chat"}),
    )
    .await;
    // The model's description is fetched in the background.
    for _ in 0..200 {
        if hf.requests().iter().any(|r| r.path.ends_with("README.md")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let log = op(&d, "outbound_log", json!({"limit": 200})).await;
    let entries = log["entries"].as_array().unwrap();
    let to = |host: &str| entries.iter().filter(|e| e["host"] == host).count();
    // Every request a server out there got stands in the log …
    assert_eq!(to("hf.test"), hf.requests().len(), "{:#?}", hf.requests());
    assert_eq!(to("wiki.test"), web.requests().len());
    let chats = cloud.requests().len();
    let cloud_entries: Vec<&Value> = entries
        .iter()
        .filter(|e| e["host"] == "cloud.test")
        .collect();
    assert_eq!(
        cloud_entries
            .iter()
            .filter(|e| e["purpose"] == "cloud_model")
            .count(),
        chats
    );
    assert!(chats >= 1);
    assert!(cloud_entries.iter().any(|e| e["purpose"] == "cloud_setup"));
    // … and nothing else: the local model's requests stayed here.
    assert_eq!(
        entries.len(),
        to("hf.test") + to("wiki.test") + cloud_entries.len(),
        "{entries:#?}"
    );
    // Each why, in the log.
    let purposes: std::collections::BTreeSet<&str> = entries
        .iter()
        .filter_map(|e| e["purpose"].as_str())
        .collect();
    for p in [
        "model_search",
        "model_info",
        "model_download",
        "model_card",
        "catalog",
        "web_search",
        "cloud_setup",
        "cloud_model",
    ] {
        assert!(purposes.contains(p), "{p} missing: {purposes:?}");
    }
    // What went to the cloud model is there to see – its key never is.
    let message = cloud_entries
        .iter()
        .find(|e| e["purpose"] == "cloud_model")
        .unwrap();
    assert!(message["sent"].as_str().unwrap().contains("Sag Hallo"));
    assert!(!log.to_string().contains("SECRET-PROVIDER-KEY"));
    let search = entries
        .iter()
        .find(|e| e["purpose"] == "web_search")
        .unwrap();
    assert_eq!(search["subject"], "Oslo Einwohner");
    assert_eq!(search["by"], "you");
    // The description was fetched by Ancilo itself.
    assert_eq!(
        entries
            .iter()
            .find(|e| e["purpose"] == "model_card")
            .unwrap()["by"],
        "ancilo"
    );
    let summary = op(&d, "outbound_summary", json!({})).await;
    assert_eq!(summary["today"]["web_search"], to("wiki.test"));

    // The user empties it.
    op(&d, "clear_outbound_log", json!({})).await;
    let log = op(&d, "outbound_log", json!({})).await;
    assert_eq!(log["entries"], json!([]));
    d.stop().await;
}
