//! Daemon lifecycle: one daemon per home; stopping ends open event streams.

use std::time::Duration;

use ancilo_core::Config;
use ancilo_daemon::DaemonOptions;
use ancilo_testkit::TestHome;

fn options() -> DaemonOptions {
    DaemonOptions {
        llama_build: Some(None),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_daemon_per_home_and_stop_does_not_hang_on_open_streams() {
    let home = TestHome::new();
    let config = Config {
        port: 0,
        ..home.config()
    };
    let d = ancilo_daemon::start(home.paths.clone(), config.clone(), options())
        .await
        .unwrap();
    // A second daemon on the same home is refused – it would manage the same
    // models and database.
    let Err(err) = ancilo_daemon::start(home.paths.clone(), config.clone(), options()).await else {
        panic!("second daemon started");
    };
    assert!(
        err.message().contains("another Ancilo daemon"),
        "{}",
        err.message()
    );
    // An open event stream (like the app's) must not block the shutdown.
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/events", d.url()))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let stopped = tokio::time::timeout(Duration::from_secs(8), d.stop()).await;
    assert!(stopped.is_ok(), "stop hung on an open event stream");
    drop(resp);
    // Afterwards the home is free again.
    let d = ancilo_daemon::start(home.paths.clone(), config, options())
        .await
        .unwrap();
    d.stop().await;
}
