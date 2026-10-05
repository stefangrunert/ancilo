//! Ancilo's access token never reaches a model: not a cloud assistant that
//! asks for the model API settings, not an MCP client (Claude Code, Codex –
//! their models run in the cloud). The app and the CLI still get it.
//! (Due diligence 2026-10-05, finding 1.)

use ancilo_daemon::{DaemonHandle, DaemonOptions};
use ancilo_testkit::{FakeLlm, Script, TestHome};
use serde_json::{Value, json};

async fn start(home: &TestHome) -> DaemonHandle {
    let config = ancilo_core::Config {
        port: 0,
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

async fn post(d: &DaemonHandle, path: &str, body: Value) -> Value {
    let r = reqwest::Client::new()
        .post(format!("{}{path}", d.url()))
        .bearer_auth(&d.token)
        .header("x-ancilo-confirm", "true")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{path}: {}", r.status());
    r.json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_access_token_never_reaches_a_model() {
    let home = TestHome::new();
    let cloud = FakeLlm::start(
        Script::from_yaml(
            r#"
model: some-cloud
steps:
  - respond: { tool_calls: [{ name: model_api_config, arguments: { client: openai } }] }
  - respond: { text: "Here is how to connect." }
fallback: { text: "Here is how to connect." }
"#,
        )
        .unwrap(),
    )
    .await;
    let d = start(&home).await;
    let added = post(
        &d,
        "/api/v1/ops/set_cloud_provider",
        json!({"base_url": format!("{}/v1", cloud.url()), "model": "some-cloud", "api_key": "PROVIDER_KEY"}),
    )
    .await;
    let model = added["model"]["id"].as_str().unwrap();

    // A cloud assistant asks for the settings: the answer it gets back has
    // no token in it.
    post(
        &d,
        "/api/v1/ops/ask",
        json!({"prompt": "How do I connect a tool to Ancilo?", "model": model, "kind": "setup"}),
    )
    .await;
    let requests = cloud.requests();
    assert!(
        requests.len() >= 2,
        "the tool result went back to the model"
    );
    for r in &requests {
        assert!(
            !r.body.to_string().contains(&d.token),
            "token sent to the cloud"
        );
    }
    let (_, result) = d
        .assistant
        .call_as_tool("model_api_config", json!({"client": "openai"}), true)
        .await
        .unwrap();
    assert!(!result.contains(&d.token), "{result}");

    // MCP clients get no token either.
    let mcp = post(
        &d,
        "/mcp",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "ancilo", "arguments": {"operation": "model_api_config", "input": {"client": "codex"}}}}),
    )
    .await;
    let text = mcp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("ANCILO_TOKEN"), "{text}");
    assert!(!text.contains(&d.token), "{text}");

    // The app and the CLI still get the real settings.
    let shown = post(
        &d,
        "/api/v1/ops/model_api_config",
        json!({"client": "codex"}),
    )
    .await;
    assert_eq!(shown["env"]["ANCILO_TOKEN"], d.token.as_str());
    d.stop().await;
}
