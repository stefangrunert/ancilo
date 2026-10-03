//! M9-AC-06: nothing leaves the machine unasked – with the shipped package.
//!
//! The package (`ANCILO_PACKAGE`: an unpacked release archive, see
//! `packaging/package.sh`) runs in a fresh home under a macOS sandbox profile
//! that allows network connections only to the loopback interface and
//! **kills** any process that tries anything else (IPv4, IPv6, or a DNS
//! lookup). A program cannot ignore such an attempt: the daemon or
//! llama.cpp would die and the flow fail. Positive controls in the same test
//! prove the profile catches each kind of attempt on this macOS version.
//!
//! Run: `ANCILO_PACKAGE=dist/ancilo-<version>-aarch64-apple-darwin cargo nextest run -p ancilo --test network --run-ignored only`

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome};
use serde_json::{Value, json};

/// Loopback only; every other outbound connection – and DNS through
/// mDNSResponder – kills the process that tries it.
const PROFILE: &str = r#"(version 1)
(allow default)
(deny network-outbound (with send-signal SIGKILL))
(allow network-outbound (remote ip "localhost:*"))
(allow network-outbound (remote unix-socket))
(deny network-outbound (remote unix-socket (path-literal "/private/var/run/mDNSResponder")) (with send-signal SIGKILL))
"#;

const KILLED: i32 = 137;

/// Stops the package's daemon – also when the test fails.
struct StopDaemon(PathBuf, PathBuf);

impl Drop for StopDaemon {
    fn drop(&mut self) {
        let _ = Command::new(&self.0)
            .args(["daemon", "stop"])
            .env("ANCILO_HOME", &self.1)
            .output();
    }
}

fn sandboxed(program: &Path, args: &[&str], home: &Path) -> (i32, String, String) {
    let out = Command::new("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(PROFILE)
        .arg(program)
        .args(args)
        .env("ANCILO_HOME", home)
        .env_remove("ANCILO_PORT")
        .env_remove("ANCILO_LLAMA_SERVER")
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + out.status.signal().unwrap_or(0)
    });
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn sh(script: &str, home: &Path) -> i32 {
    sandboxed(Path::new("/bin/sh"), &["-c", script], home).0
}

/// A small real model for inference (the test home of `just test-real` has one).
fn real_model() -> PathBuf {
    std::env::var_os("ANCILO_TEST_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("ANCILO_REAL_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/real-model-home")
                })
                .join("models/unsloth/Qwen3-0.6B-GGUF/Qwen3-0.6B-Q4_0.gguf")
        })
}

// covers: M9-AC-06
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release check: needs the package (ANCILO_PACKAGE) and a small real model"]
async fn the_package_talks_only_to_this_machine() {
    let package = PathBuf::from(std::env::var("ANCILO_PACKAGE").expect("ANCILO_PACKAGE"));
    let ancilo = package.join("bin/ancilo");
    assert!(ancilo.is_file(), "{} missing", ancilo.display());
    assert!(
        package.join("libexec/ancilo/llama-server").is_file(),
        "llama.cpp must be in the package"
    );
    assert!(
        package.join("libexec/ancilo/ancilo-ocr").is_file(),
        "text recognition must be in the package"
    );
    let model = real_model();
    assert!(
        model.is_file(),
        "{} missing (run `just test-real` once or set ANCILO_TEST_MODEL)",
        model.display()
    );
    let home = TestHome::new();
    let h = home.path().to_path_buf();
    let _stop = StopDaemon(ancilo.clone(), h.clone());

    // Positive controls: the profile catches every kind of attempt here.
    assert_eq!(
        sh("/usr/bin/curl -s -m 5 -o /dev/null https://1.1.1.1", &h),
        KILLED,
        "IPv4"
    );
    assert_eq!(
        sh(
            "/usr/bin/curl -s -m 5 -o /dev/null 'https://[2606:4700:4700::1111]'",
            &h
        ),
        KILLED,
        "IPv6"
    );
    assert_eq!(
        sh(
            "/usr/bin/python3 -c 'import socket; socket.getaddrinfo(\"example.com\", 443)'",
            &h
        ),
        KILLED,
        "DNS"
    );
    // A program that ignores the error is stopped all the same.
    assert_eq!(
        sh(
            "/usr/bin/python3 -c 'import socket\ntry: socket.create_connection((\"1.1.1.1\", 443), 3)\nexcept Exception: pass'",
            &h
        ),
        KILLED,
        "ignored error"
    );
    assert_eq!(
        sh(
            "/usr/bin/curl -s -m 5 -o /dev/null http://127.0.0.1:9 || true",
            &h
        ),
        0,
        "loopback is allowed"
    );

    // The flow: start, UI, a download the user asked for, a real model, a
    // delegated task, stop – all through the shipped binaries.
    let hf = FakeHf::start(vec![FakeRepo::new(
        "demo/Tiny-GGUF",
        vec![FakeFile::gguf("Tiny-Q8_0.gguf", "qwen3", 4096, 50_000)],
    )])
    .await;
    let config = ancilo_core::Config {
        port: 0,
        hf_endpoint: hf.url(),
        model_search_dirs: Some(vec![]),
        ..ancilo_core::Config::default()
    };
    home.write_config(&config);
    let run = |args: &[&str]| {
        let (code, out, err) = sandboxed(&ancilo, args, &h);
        assert_eq!(code, 0, "ancilo {args:?} failed ({code}): {err}");
        out
    };
    let list: Value = serde_json::from_str(&run(&["list", "--json"])).unwrap();
    assert_eq!(list, json!([]));
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(home.paths.daemon_file()).unwrap()).unwrap();
    let url = info["url"].as_str().unwrap().to_string();
    let ui = reqwest::get(format!("{url}/app/")).await.unwrap();
    assert!(ui.status().is_success());
    // Nothing was fetched before the user asked for a download.
    assert!(
        hf.requests().is_empty(),
        "requests before any user action: {:?}",
        hf.requests()
    );
    run(&["add", "demo/Tiny-GGUF", "--no-start"]);
    assert!(!hf.requests().is_empty());

    // A real model on the shipped llama.cpp, and a delegated task.
    run(&["add", model.to_str().unwrap()]);
    let project = home.scratch("project");
    ancilo_testkit::home::git_repo(&project, &[("README.md", "# Demo\n")]);
    let (code, out, err) = sandboxed(
        &ancilo,
        &[
            "run",
            "--json",
            "--cwd",
            project.to_str().unwrap(),
            "Create hello.txt containing the word hello.",
        ],
        &h,
    );
    assert_eq!(code, 0, "delegation failed ({code}): {err}");
    let task: Value = serde_json::from_str(&out).unwrap();
    assert!(task["status"].is_string(), "{task}");
    run(&["daemon", "stop"]);
}
