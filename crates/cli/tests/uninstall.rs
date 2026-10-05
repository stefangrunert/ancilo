//! `ancilo uninstall`: stops the running daemon, disconnects what this home
//! connected and deletes the home – and only after a clear yes.

use std::process::{Command, Stdio};

use ancilo_testkit::TestHome;

fn ancilo(home: &TestHome) -> Command {
    let mut c = Command::new(assert_cmd::cargo::cargo_bin("ancilo"));
    c.env("ANCILO_HOME", home.path())
        .env_remove("ANCILO_PORT")
        // Never the user's own Claude configuration.
        .env("CLAUDE_CONFIG_DIR", home.scratch("claude-config"));
    c
}

fn alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn uninstall_stops_ancilo_disconnects_claude_and_deletes_the_home() {
    let home = TestHome::new();
    // A fake `claude` that records what it is asked; Claude Code connected.
    let bin = home.scratch("bin");
    let log = bin.join("claude.log");
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", log.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    home.write_config(&ancilo_core::Config {
        claude_bin: Some(claude),
        ..home.config()
    });
    std::fs::create_dir_all(home.path().join("clients")).unwrap();
    std::fs::write(
        home.path().join("clients/connections.json"),
        r#"{"claude_code": {"client": "claude_code", "connected": true, "verified": true, "since": null}}"#,
    )
    .unwrap();
    assert!(
        ancilo(&home)
            .args(["daemon", "start"])
            .status()
            .unwrap()
            .success()
    );
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("daemon.json")).unwrap())
            .unwrap();
    let pid = info["pid"].as_u64().unwrap();
    assert!(alive(pid));

    // Without a terminal and without --yes nothing happens.
    let quiet = ancilo(&home)
        .arg("uninstall")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!quiet.status.success());
    assert!(String::from_utf8_lossy(&quiet.stderr).contains("ancilo uninstall --yes"));
    assert!(home.path().join("token").exists() && alive(pid));

    let out = ancilo(&home)
        .args(["uninstall", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let removed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(removed["disconnected"], serde_json::json!(["claude_code"]));
    assert!(!home.path().exists(), "the home is deleted");
    assert!(!alive(pid), "the daemon is stopped");
    let asked = std::fs::read_to_string(&log).unwrap();
    assert!(
        asked.contains("plugin uninstall ancilo@ancilo-local"),
        "{asked}"
    );
}

#[test]
fn uninstall_keeping_the_data_keeps_the_home() {
    let home = TestHome::new();
    home.write_config(&home.config());
    let out = ancilo(&home)
        .args(["uninstall", "--yes", "--keep-data"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(home.path().join("config.toml").exists());
}
