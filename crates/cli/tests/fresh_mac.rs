//! M9-AC-01: a fresh Mac is ready in minutes – the finished package in a
//! clean macOS VM (`packaging/vm-test.sh`, needs `tart`).
//!
//! Run: `ANCILO_ARCHIVE=… ANCILO_TEST_MODEL=… cargo nextest run -p ancilo --test fresh_mac --run-ignored only`

use std::path::Path;
use std::process::Command;

/// Threshold for installation → delegation done (empirical; M9 spec).
const READY_WITHIN_S: u64 = 600;

// covers: M9-AC-01
#[test]
#[ignore = "release check: needs tart and a macOS VM image"]
fn a_fresh_mac_is_ready_in_minutes() {
    let archive = std::env::var("ANCILO_ARCHIVE").expect("ANCILO_ARCHIVE");
    let model = std::env::var("ANCILO_TEST_MODEL").expect("ANCILO_TEST_MODEL");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/vm-test.sh");
    let out = Command::new(script)
        .args([&archive, &model])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(stdout.lines().last().unwrap_or("{}")).unwrap();
    let ready = report["ready_s"].as_u64().unwrap();
    println!("fresh Mac ready in {ready} s");
    assert!(ready <= READY_WITHIN_S, "{ready} s");
}
