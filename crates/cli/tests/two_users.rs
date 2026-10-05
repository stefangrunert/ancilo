//! Two users on one Mac, each with an own Ancilo: where this home's daemon
//! once listened (a stale `daemon.json`), the other user's daemon may listen
//! now. The CLI must not take it for its own – it starts its own daemon.

use std::process::Command;

use ancilo_testkit::TestHome;

fn ancilo(home: &TestHome) -> Command {
    let mut c = Command::new(assert_cmd::cargo::cargo_bin("ancilo"));
    c.env("ANCILO_HOME", home.path()).env_remove("ANCILO_PORT");
    c
}

fn info(home: &TestHome) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(home.path().join("daemon.json")).unwrap())
        .unwrap()
}

#[test]
fn another_users_daemon_is_never_taken_for_ones_own() {
    let (a, b) = (TestHome::new(), TestHome::new());
    for h in [&a, &b] {
        h.write_config(&ancilo_core::Config {
            port: 0,
            ..h.config()
        });
    }
    assert!(
        ancilo(&a)
            .args(["daemon", "start"])
            .status()
            .unwrap()
            .success()
    );
    // B's stale daemon.json points where A's daemon listens now.
    std::fs::write(
        b.path().join("daemon.json"),
        std::fs::read_to_string(a.path().join("daemon.json")).unwrap(),
    )
    .unwrap();
    let started = ancilo(&b).args(["daemon", "start"]).output().unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let (ia, ib) = (info(&a), info(&b));
    assert_ne!(ia["url"], ib["url"], "B runs its own daemon");
    assert_eq!(ib["home"].as_str().unwrap(), b.path().to_str().unwrap());
    let status = ancilo(&b).arg("list").output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    for h in [&a, &b] {
        let _ = ancilo(h).args(["daemon", "stop"]).output();
    }
}
