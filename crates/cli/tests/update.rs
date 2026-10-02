//! M9-AC-03: after an update the daemon of the previous version may still be
//! running. The CLI replaces an older one in an orderly way and never
//! downgrades a newer one.

use std::sync::{Arc, Mutex};

use ancilo_testkit::TestHome;
use axum::Router;
use axum::routing::{get, post};
use serde_json::{Value, json};

/// A daemon of another version: answers health, records shutdown requests.
async fn fake_daemon(home: &TestHome, version: &str, protocol: u32) -> Arc<Mutex<Vec<String>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (c1, c2) = (calls.clone(), calls.clone());
    let v = version.to_string();
    let app = Router::new()
        .route(
            "/api/v1/health",
            get(move || {
                let v = v.clone();
                async move { axum::Json(json!({"status": "ok", "product": "Ancilo", "version": v, "protocol": protocol})) }
            }),
        )
        .route(
            "/api/v1/ops/{name}",
            post(move |axum::extract::Path(name): axum::extract::Path<String>| {
                c1.lock().unwrap().push(name);
                async { axum::Json(json!({})) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    home.paths.ensure().unwrap();
    std::fs::write(home.paths.token_file(), "t".repeat(64)).unwrap();
    std::fs::write(
        home.paths.daemon_file(),
        serde_json::to_string(&json!({
            // A pid that does not exist: the "old daemon" is gone once asked to stop.
            "pid": 999_999, "port": port, "url": format!("http://127.0.0.1:{port}"),
            "version": version, "home": home.path(), "started_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    drop(c2);
    calls
}

fn cli(home: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("ancilo"))
        .args(args)
        .env("ANCILO_HOME", home)
        .env_remove("ANCILO_PORT")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Stops the daemon a test started through the CLI – also when the test fails.
struct StopDaemon(std::path::PathBuf);

impl Drop for StopDaemon {
    fn drop(&mut self) {
        let _ = std::process::Command::new(assert_cmd::cargo::cargo_bin("ancilo"))
            .args(["daemon", "stop"])
            .env("ANCILO_HOME", &self.0)
            .env_remove("ANCILO_PORT")
            .output();
    }
}

// covers: M9-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_daemon_is_replaced_by_the_updated_version() {
    let home = TestHome::new();
    home.write_config(&ancilo_core::Config {
        model_search_dirs: Some(vec![]),
        ..home.config()
    });
    let calls = fake_daemon(&home, "0.0.1", 1).await;
    let h = home.path().to_path_buf();
    let _stop = StopDaemon(h.clone());
    let h2 = h.clone();
    let (code, out, err) = tokio::task::spawn_blocking(move || cli(&h2, &["list", "--json"]))
        .await
        .unwrap();
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("0.0.1 → "), "{err}");
    assert_eq!(*calls.lock().unwrap(), ["daemon_shutdown"]);
    let _: Value = serde_json::from_str(&out).unwrap();
    // The new daemon runs the version of this CLI.
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(home.paths.daemon_file()).unwrap()).unwrap();
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    let (code, _, err) = tokio::task::spawn_blocking(move || cli(&h, &["daemon", "stop"]))
        .await
        .unwrap();
    assert_eq!(code, 0, "{err}");
}

// covers: M9-AC-03
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_daemon_is_never_downgraded() {
    let home = TestHome::new();
    let calls = fake_daemon(&home, "99.0.0", 99).await;
    let h = home.path().to_path_buf();
    let (code, _, err) = tokio::task::spawn_blocking(move || cli(&h, &["list"]))
        .await
        .unwrap();
    assert_ne!(code, 0);
    assert!(
        err.contains("older than the running Ancilo (99.0.0)"),
        "{err}"
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "the newer daemon was left alone"
    );
}
