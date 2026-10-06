//! A computer too small for any local model (a 4 GB laptop): Ancilo says so
//! with the numbers and offers no download that would not run – also when
//! someone just starts chatting.

use ancilo_core::Config;
use ancilo_daemon::DaemonOptions;
use ancilo_models::hardware::{GIB, HardwareProfile};
use ancilo_testkit::TestHome;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_computer_too_small_for_any_model_is_told_so() {
    let home = TestHome::new();
    let mut hw = HardwareProfile::apple(2);
    hw.total_ram_bytes = 2 * GIB;
    let file = home.scratch("hw").join("hw.json");
    std::fs::write(&file, serde_json::to_string(&hw).unwrap()).unwrap();
    let config = Config {
        port: 0,
        hardware_override: Some(file),
        ..home.config()
    };
    let d = ancilo_daemon::start(
        home.paths.clone(),
        config,
        DaemonOptions {
            llama_build: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let op = |name: &'static str, input: Value| {
        let (url, token) = (d.url(), d.token.clone());
        async move {
            reqwest::Client::new()
                .post(format!("{url}/api/v1/ops/{name}"))
                .bearer_auth(token)
                .json(&input)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let rec = op("recommend_models", json!({"purposes": ["chat"]})).await;
    assert!(rec["best"].is_null(), "{rec}");
    let need = rec["smallest_need_bytes"].as_u64().unwrap();
    assert!(need > rec["memory"]["for_models_bytes"].as_u64().unwrap());
    // Just chatting: no offer of a download that answers nothing.
    let r = op("ask", json!({"prompt": "Hallo", "kind": "chat"})).await;
    assert!(
        r["answer"]
            .as_str()
            .unwrap()
            .contains("does not have enough memory"),
        "{r}"
    );
    assert_eq!(r["pending"], json!([]), "{r}");
    d.stop().await;
}
