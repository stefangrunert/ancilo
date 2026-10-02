//! Guardrail "local first": with a Hugging Face model installed, starting
//! the daemon contacts nobody. Model cards are fetched only when the user
//! acts – adding the model, or `refresh_model_knowledge`.

use std::path::PathBuf;
use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use serde_json::{Value, json};

const REPO: &str = "someone/Tiny-Coder-GGUF";

async fn start(home: &TestHome, hf: &FakeHf) -> DaemonHandle {
    let config = Config {
        port: 0,
        hf_endpoint: hf.url(),
        catalog_url: format!("{}/ancilo/catalog.json", hf.url()),
        llama_server_bin: Some(fake_llama_server_bin()),
        model_search_dirs: Some(vec![]),
        ..home.config()
    };
    let options = DaemonOptions {
        llama_build: Some(None),
        ..Default::default()
    };
    ancilo_daemon::start(home.paths.clone(), config, options)
        .await
        .unwrap()
}

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

fn card_requests(hf: &FakeHf) -> usize {
    hf.requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path.starts_with(REPO) && r.path.ends_with("/README.md"))
        .count()
}

/// Waits until a knowledge search for `word` finds the model card.
async fn card_indexed(d: &DaemonHandle, word: &str) -> bool {
    for _ in 0..1200 {
        let r = op(d, "search", json!({"query": word, "scope": "knowledge"})).await;
        let found = r["hits"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|h| h["path"] == format!("hf:{REPO}"));
        if found {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

// covers: M9-AC-06
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_cards_are_fetched_only_when_the_user_acts() {
    let home = TestHome::new();
    let hf = FakeHf::start(vec![FakeRepo::new(
        REPO,
        vec![
            FakeFile::gguf("Tiny-Coder-Q4_K_M.gguf", "qwen3", 4096, 100_000),
            FakeFile::new(
                "README.md",
                b"# Tiny Coder\n\nThe card mentions a quokka.\n".to_vec(),
            ),
        ],
    )])
    .await;

    // Adding the model: the user asked for the network – the card comes along.
    let d = start(&home, &hf).await;
    let added = op(
        &d,
        "add_model",
        json!({"address": format!("hf.co/{REPO}:Q4_K_M"), "start": false}),
    )
    .await;
    let id = added["id"].as_str().unwrap().to_string();
    for _ in 0..400 {
        if op(&d, "model_status", json!({"model": id})).await["status"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        card_indexed(&d, "quokka").await,
        "card not indexed after add_model"
    );
    assert_eq!(card_requests(&hf), 1, "{:?}", hf.requests());
    d.stop().await;

    // A changed card on disk shows that the next start re-indexes it from
    // there; a fetch would have overwritten it.
    let card: PathBuf = home
        .paths
        .home()
        .join("knowledge/model-cards")
        .join(format!("{id}.md"));
    assert!(card.exists(), "{}", card.display());
    std::fs::write(&card, "# Tiny Coder\n\nA local note about a wombat.\n").unwrap();
    let before = hf.requests().len();

    let d = start(&home, &hf).await;
    assert!(
        card_indexed(&d, "wombat").await,
        "card on disk not re-indexed at start"
    );
    assert_eq!(
        hf.requests().len(),
        before,
        "the start contacted Hugging Face: {:?}",
        &hf.requests()[before..]
    );

    // Asked for explicitly: the card is fetched again.
    let r = op(&d, "refresh_model_knowledge", json!({})).await;
    assert_eq!(r["model_cards_fetched"], 1, "{r}");
    assert_eq!(card_requests(&hf), 2, "{:?}", hf.requests());
    assert!(std::fs::read_to_string(&card).unwrap().contains("quokka"));
    d.stop().await;
}
