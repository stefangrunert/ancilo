//! Smoke test of the native shell (M7-AC-08/09): the app starts, finds or
//! starts the daemon, opens its window and serves the menu's "open" path.
//! Run with `just app-smoke` (needs a desktop session and `target/debug/ancilo`).

use std::process::Command;
use std::time::{Duration, Instant};

// covers: M7-AC-08, M7-AC-09
#[test]
fn the_app_starts_the_daemon_and_opens_its_window() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ancilo = root.join("target/debug/ancilo");
    assert!(
        ancilo.exists(),
        "build `ancilo` first (cargo build -p ancilo)"
    );
    let home = std::env::temp_dir().join(format!("ancilo-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let started = Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_ancilo-app"))
        .env("ANCILO_HOME", &home)
        .env("ANCILO_PORT", "0")
        .env("ANCILO_BIN", &ancilo)
        .env("ANCILO_SMOKE", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Memory of the app process while its window is open.
    std::thread::sleep(Duration::from_millis(1000));
    let rss_kb: u64 = String::from_utf8_lossy(
        &Command::new("ps")
            .args(["-o", "rss=", "-p", &child.id().to_string()])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .parse()
    .unwrap_or(0);
    let out = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    let report: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .last()
            .unwrap_or("{}"),
    )
    .unwrap();
    // The daemon keeps running without the app – stop it.
    Command::new(&ancilo)
        .args(["daemon", "stop"])
        .env("ANCILO_HOME", &home)
        .status()
        .ok();
    println!(
        "app: {report} · {} ms · {} MB",
        elapsed.as_millis(),
        rss_kb / 1024
    );
    assert!(
        report["daemon"]
            .as_str()
            .is_some_and(|u| u.starts_with("http://127.0.0.1:")),
        "{report}"
    );
    assert_eq!(report["window"], true);
    assert_eq!(report["menu_open"], true);
    // Thresholds from the first measurement (M7 spec, "Erkenntnisse").
    assert!(elapsed < Duration::from_secs(15), "start took {elapsed:?}");
    assert!(
        rss_kb > 0 && rss_kb < 400 * 1024,
        "app uses {} MB",
        rss_kb / 1024
    );
    std::fs::remove_dir_all(&home).ok();
}
