//! M9-AC-05: the README's quick start works exactly as written – every line
//! of its code block runs with the real `ancilo` binary in a fresh home
//! (fake Hugging Face, fake llama-server, fake Claude Code; a fake `open`
//! records the browser call).

use std::path::Path;

use ancilo_core::Config;
use ancilo_models::hardware::HardwareProfile;
use ancilo_testkit::{FakeFile, FakeHf, FakeRepo, TestHome, fake_llama_server_bin};
use serde_json::json;

fn repo(id: &str, arch: &str) -> FakeRepo {
    let name = id
        .rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches("-GGUF")
        .to_string();
    let file = format!("{name}-Q4_K_M.gguf");
    FakeRepo::new(id, vec![FakeFile::gguf(&file, arch, 4096, 200_000)])
        .with_gguf_meta(json!({"architecture": arch, "context_length": 4096}))
}

fn script(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Runs a program with a pseudo-terminal as its terminal (`script`; BSD and
/// util-linux spell it differently).
fn at_terminal(program: &Path, args: &[String]) -> std::process::Command {
    let mut c = std::process::Command::new("script");
    if cfg!(target_os = "macos") {
        c.arg("-q").arg("/dev/null").arg(program).args(args);
    } else {
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let line = std::iter::once(program.display().to_string())
            .chain(args.iter().cloned())
            .map(|a| quote(&a))
            .collect::<Vec<_>>()
            .join(" ");
        c.arg("-qec").arg(line).arg("/dev/null");
    }
    c
}

/// The commands of the first ```bash block after `heading`.
fn block(markdown: &str, heading: &str) -> Vec<String> {
    let rest = &markdown[markdown.find(heading).expect("heading")..];
    let start = rest.find("```bash").expect("code block") + "```bash".len();
    let end = start + rest[start..].find("```").unwrap();
    rest[start..end]
        .lines()
        .map(|l| l.split(" #").next().unwrap_or_default().trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
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

// covers: M9-AC-05
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_readme_quick_start_works_as_written() {
    let readme =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))
            .unwrap();
    let commands = block(&readme, "## Quick start");
    assert!(commands.len() >= 3, "{commands:?}");
    let home = TestHome::new();
    let hf = FakeHf::start(vec![
        repo("unsloth/Qwen3.5-4B-GGUF", "qwen3"),
        repo("unsloth/Qwen3-4B-GGUF", "qwen3"),
        repo("second-state/All-MiniLM-L6-v2-Embedding-GGUF", "bert"),
    ])
    .await;
    let bin = home.scratch("bin");
    script(
        &bin,
        "claude",
        r#"case "$*" in *" list"*) echo ancilo ;; esac; exit 0"#,
    );
    for opener in ["open", "xdg-open"] {
        script(
            &bin,
            opener,
            &format!("echo \"$@\" >> {}", bin.join("opened").display()),
        );
    }
    let hw = home.scratch("hw").join("hw.json");
    std::fs::write(
        &hw,
        serde_json::to_string(&HardwareProfile::apple(16)).unwrap(),
    )
    .unwrap();
    home.write_config(&Config {
        port: 0,
        hf_endpoint: hf.url(),
        llama_server_bin: Some(fake_llama_server_bin()),
        model_search_dirs: Some(vec![]),
        hardware_override: Some(hw),
        claude_bin: Some(bin.join("claude")),
        ..home.config()
    });
    let _stop = StopDaemon(home.path().to_path_buf());
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // Without a terminal nobody can answer "Install?": setup says so instead of
    // quietly doing nothing.
    let quiet = std::process::Command::new(assert_cmd::cargo::cargo_bin("ancilo"))
        .arg("setup")
        .env("ANCILO_HOME", home.path())
        .env_remove("ANCILO_PORT")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!quiet.status.success());
    assert!(String::from_utf8_lossy(&quiet.stderr).contains("ancilo setup --yes"));
    for line in &commands {
        let words: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(words[0], "ancilo", "{line}");
        let out = tokio::task::spawn_blocking({
            let (home, path, args) = (
                home.path().to_path_buf(),
                path.clone(),
                words[1..].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            );
            move || {
                use std::io::Write;
                // The reader answers questions ("Install?") with yes.
                // …at a terminal (`script` gives the command a pseudo-terminal).
                let mut child = at_terminal(&assert_cmd::cargo::cargo_bin("ancilo"), &args)
                    .env("ANCILO_HOME", home)
                    .env("PATH", path)
                    .env_remove("ANCILO_PORT")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                // Kept open until the command is done: an early end of input
                // would reach the question before the answer.
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(b"y\n").ok();
                let out = child.wait_with_output().unwrap();
                drop(stdin);
                out
            }
        })
        .await
        .unwrap();
        assert!(
            out.status.success(),
            "`{line}` failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // The outcome the quick start promises: models ready, Claude connected, the app opened.
    let list = std::process::Command::new(assert_cmd::cargo::cargo_bin("ancilo"))
        .args(["list", "--json"])
        .env("ANCILO_HOME", home.path())
        .output()
        .unwrap();
    let models: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let ids: Vec<&str> = models
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.iter().any(|i| i.starts_with("qwen3-4b")), "{ids:?}");
    assert!(ids.len() >= 3, "setup + added model: {ids:?}");
    let opened = std::fs::read_to_string(bin.join("opened")).unwrap_or_default();
    assert!(
        opened.contains("/app/#token="),
        "the app was opened: {opened}"
    );
}
