//! Sandbox for the `bash` tool.
//!
//! macOS: Seatbelt (`sandbox-exec`) – reading is allowed, writing only inside
//! the workspace and temporary directories, network off unless enabled.
//! Linux: Bubblewrap (`bwrap`) when available; otherwise commands are refused
//! rather than run unconfined.

use std::path::Path;

use tokio::process::Command;

fn seatbelt_profile(root: &Path, network: bool) -> String {
    let root = root.display().to_string().replace('"', "");
    let net = if network { "" } else { "(deny network*)\n" };
    format!(
        r#"(version 1)
(allow default)
{net}(deny file-write*)
(allow file-write*
  (subpath "{root}")
  (subpath "/private/tmp")
  (subpath "/private/var/folders")
  (literal "/dev/null")
  (literal "/dev/zero")
  (regex #"^/dev/tty")
  (regex #"^/dev/fd/"))
"#
    )
}

/// A command that runs `script` in the sandbox (the caller sets `current_dir`).
pub fn command(root: &Path, network: bool, script: &str) -> Result<Command, String> {
    if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-p")
            .arg(seatbelt_profile(root, network))
            .arg("/bin/sh")
            .arg("-c")
            .arg(script);
        return Ok(c);
    }
    if cfg!(target_os = "linux") && which("bwrap") {
        let mut c = Command::new("bwrap");
        c.args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
        ])
        .arg("--bind")
        .arg(root)
        .arg(root)
        .args(["--die-with-parent"]);
        if !network {
            c.arg("--unshare-net");
        }
        c.args(["/bin/sh", "-c", script]);
        return Ok(c);
    }
    Err(
        "no sandbox available on this system (install bubblewrap) – shell commands are disabled"
            .into(),
    )
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    async fn run(root: &Path, network: bool, script: &str) -> std::process::Output {
        command(root, network, script)
            .unwrap()
            .current_dir(root)
            .output()
            .await
            .unwrap()
    }

    // covers: M3-AC-06
    #[tokio::test]
    async fn sandbox_allows_the_workspace_and_blocks_outside_writes_and_network() {
        let ws = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(ws.path()).unwrap();
        // Writing inside the workspace works.
        let out = run(&root, false, "echo hi > inside.txt && cat inside.txt").await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Writing outside (home directory) is denied.
        let target = dirs_home().join(format!("ancilo-sandbox-test-{}", std::process::id()));
        let out = run(&root, false, &format!("echo x > '{}'", target.display())).await;
        assert!(!out.status.success());
        assert!(!target.exists());
        // Network is denied (even to localhost).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let out = run(
            &root,
            false,
            &format!("/usr/bin/nc -z -w 2 127.0.0.1 {port}"),
        )
        .await;
        assert!(!out.status.success(), "network must be blocked");
        let out = run(
            &root,
            true,
            &format!("/usr/bin/nc -z -w 2 127.0.0.1 {port}"),
        )
        .await;
        assert!(out.status.success(), "network allowed when enabled");
    }

    fn dirs_home() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var("HOME").unwrap())
    }
}
