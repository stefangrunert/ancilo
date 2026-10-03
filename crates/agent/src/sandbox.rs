//! Sandbox for the `bash` tool.
//!
//! macOS: Seatbelt (`sandbox-exec`). Writing only inside the workspace and the
//! command's own temporary directory (plus the user's temp directory, which
//! `mktemp` uses whatever `TMPDIR` says) – never `/tmp`, shared by everyone.
//! Reading is allowed except where secrets and personal data live (SSH and
//! cloud keys, the keychains, browsers, mail, Claude/Codex, Ancilo's own
//! home with other sessions and its key file). Network off unless enabled.
//! Linux: Bubblewrap (`bwrap`) when available – the same rules; otherwise
//! commands are refused rather than run unconfined.
//!
//! The environment is not inherited: only what tools need to be found
//! (`PATH`, `HOME`, locale, toolchain homes) – no keys or tokens of the daemon.
//! Commands run at lower priority, so a build never makes the computer
//! unusable.

use std::path::{Path, PathBuf};

use tokio::process::Command;

/// Places under the home directory no command may read.
const SECRET_PLACES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".config/gh",
    ".kube",
    ".docker",
    ".netrc",
    ".git-credentials",
    ".password-store",
    ".claude",
    ".claude.json",
    ".codex",
    "Library/Keychains",
    "Library/Cookies",
    "Library/Safari",
    "Library/Mail",
    "Library/Messages",
    "Library/Application Support/Google",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Arc",
    "Library/Application Support/1Password",
    "Library/Application Support/Claude",
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".local/share/keyrings",
];

/// Variables a command keeps; everything else of the daemon stays outside.
const KEPT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "NVM_DIR",
    "NVM_BIN",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "PYENV_ROOT",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "HOMEBREW_PREFIX",
    "DEVELOPER_DIR",
    "SDKROOT",
];

/// What a sandboxed command may touch.
#[derive(Debug, Clone)]
pub struct Bounds<'a> {
    /// The workspace: readable and writable.
    pub root: &'a Path,
    /// Further places no command may read (Ancilo's home); the workspace stays
    /// readable even inside one of them.
    pub hidden: &'a [PathBuf],
    pub network: bool,
}

/// The command's own temporary directory, one per workspace, in the user's
/// temp area – never next to the project (removed with the workspace).
pub fn temp_dir(root: &Path) -> PathBuf {
    // FNV-1a: the same workspace finds the same directory after a restart.
    let hash = root
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        });
    std::env::temp_dir()
        .join("ancilo-sandbox")
        .join(format!("{hash:016x}"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The paths no command may read, canonical where they exist.
fn unreadable(b: &Bounds) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = home()
        .map(|h| SECRET_PLACES.iter().map(|p| h.join(p)).collect())
        .unwrap_or_default();
    v.extend(b.hidden.iter().cloned());
    v.into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

/// The user's own temp directory on macOS (`/var/folders/…/T`), where
/// `mktemp` writes regardless of `TMPDIR`.
#[cfg(target_os = "macos")]
fn user_temp() -> Option<&'static Path> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let out = std::process::Command::new("/usr/bin/getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .output()
            .ok()?;
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!dir.is_empty()).then(|| std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.into()))
    })
    .as_deref()
}

fn quoted(p: &Path) -> String {
    p.display().to_string().replace(['"', '\\'], "")
}

#[cfg(target_os = "macos")]
fn seatbelt_profile(b: &Bounds, tmp: &Path) -> String {
    let root = quoted(b.root);
    let net = if b.network { "" } else { "(deny network*)\n" };
    let user_temp = user_temp()
        .map(|t| format!("  (subpath \"{}\")\n", quoted(t)))
        .unwrap_or_default();
    let secret: String = unreadable(b)
        .iter()
        .map(|p| format!("  (subpath \"{}\")\n", quoted(p)))
        .collect();
    format!(
        r#"(version 1)
(allow default)
{net}(deny file-write*)
(allow file-write*
  (subpath "{root}")
  (subpath "{tmp}")
{user_temp}  (literal "/dev/null")
  (literal "/dev/zero")
  (regex #"^/dev/tty")
  (regex #"^/dev/fd/"))
(deny file-read*
{secret})
(allow file-read* (subpath "{root}") (subpath "{tmp}"))
"#,
        tmp = quoted(tmp),
    )
}

/// A command that runs `script` in the sandbox (the caller sets `current_dir`).
pub fn command(b: &Bounds, script: &str) -> Result<Command, String> {
    let tmp = temp_dir(b.root);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot prepare the sandbox: {e}"))?;
    let tmp = std::fs::canonicalize(&tmp).unwrap_or(tmp);
    let mut c = sandboxed(b, &tmp, script)?;
    c.env_clear();
    for k in KEPT_ENV {
        if let Some(v) = std::env::var_os(k) {
            c.env(k, v);
        }
    }
    c.env("TMPDIR", &tmp);
    Ok(c)
}

#[cfg(target_os = "macos")]
fn sandboxed(b: &Bounds, tmp: &Path, script: &str) -> Result<Command, String> {
    let mut c = Command::new("/usr/bin/sandbox-exec");
    c.arg("-p")
        .arg(seatbelt_profile(b, tmp))
        .args(["/usr/bin/nice", "-n", "10", "/bin/sh", "-c"])
        .arg(script);
    Ok(c)
}

#[cfg(not(target_os = "macos"))]
fn sandboxed(b: &Bounds, tmp: &Path, script: &str) -> Result<Command, String> {
    if !(cfg!(target_os = "linux") && which("bwrap")) {
        return Err(
            "no sandbox available on this system (install bubblewrap) – shell commands are disabled"
                .into(),
        );
    }
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
    ]);
    // Secrets disappear behind an empty directory (or file); the workspace
    // and its temp directory are mounted again afterwards.
    for p in unreadable(b) {
        if p.is_dir() {
            c.arg("--tmpfs").arg(&p);
        } else if p.exists() {
            c.arg("--ro-bind").arg("/dev/null").arg(&p);
        }
    }
    c.arg("--bind").arg(b.root).arg(b.root);
    c.arg("--bind").arg(tmp).arg(tmp);
    c.arg("--die-with-parent");
    if !b.network {
        c.arg("--unshare-net");
    }
    c.args(["nice", "-n", "10", "/bin/sh", "-c", script]);
    Ok(c)
}

#[cfg(not(target_os = "macos"))]
fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    async fn run_in(
        root: &Path,
        hidden: &[PathBuf],
        network: bool,
        script: &str,
    ) -> std::process::Output {
        let b = Bounds {
            root,
            hidden,
            network,
        };
        command(&b, script)
            .unwrap()
            .current_dir(root)
            .output()
            .await
            .unwrap()
    }

    async fn run(root: &Path, network: bool, script: &str) -> std::process::Output {
        run_in(root, &[], network, script).await
    }

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let ws = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(ws.path()).unwrap().join("work");
        std::fs::create_dir(&root).unwrap();
        (ws, root)
    }

    // covers: M3-AC-06
    #[tokio::test]
    async fn sandbox_allows_the_workspace_and_blocks_outside_writes_and_network() {
        let (_ws, root) = workspace();
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

    // covers: M8-AC-13
    #[tokio::test]
    async fn a_command_has_its_own_temp_no_shared_tmp_no_secrets_and_no_inherited_keys() {
        let (ws, root) = workspace();
        // Its own temp directory – not /tmp, not next to the project.
        let out = run(&root, false, "echo x > \"$TMPDIR/t\" && echo $TMPDIR").await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tmp = std::fs::canonicalize(temp_dir(&root)).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            tmp.display().to_string()
        );
        assert!(!tmp.starts_with(ws.path().canonicalize().unwrap()));
        std::fs::remove_dir_all(&tmp).ok();
        let shared = format!("/tmp/ancilo-sandbox-{}", std::process::id());
        let out = run(&root, false, &format!("echo x > {shared}")).await;
        assert!(
            !out.status.success() && !Path::new(&shared).exists(),
            "/tmp is shared by everyone"
        );
        // mktemp (which ignores TMPDIR on macOS) still works.
        let out = run(&root, false, "f=$(mktemp) && rm \"$f\"").await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // A hidden place (Ancilo's home) cannot be read – the workspace inside it can.
        let home = ws.path().canonicalize().unwrap();
        std::fs::write(home.join("secret.key"), "s3cret").unwrap();
        std::fs::write(root.join("mine.txt"), "mine").unwrap();
        let hidden = [home.clone()];
        let out = run_in(
            &root,
            &hidden,
            false,
            &format!("cat '{}'", home.join("secret.key").display()),
        )
        .await;
        assert!(!out.status.success(), "a hidden file is readable");
        let out = run_in(&root, &hidden, false, "cat mine.txt").await;
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "mine",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // SSH keys are never readable.
        let ssh = dirs_home().join(".ssh");
        if ssh.is_dir() {
            let out = run(&root, false, &format!("ls '{}'", ssh.display())).await;
            assert!(!out.status.success(), "~/.ssh is readable");
        }
        // The daemon's environment stays outside (cargo sets this one for
        // the test); PATH and HOME come along.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let out = run(
            &root,
            false,
            "echo \"[$CARGO_MANIFEST_DIR]$HOME\"; command -v git",
        )
        .await;
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("[]/"), "{text}");
        assert!(text.contains("git"), "tools are found: {text}");
    }

    fn dirs_home() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var("HOME").unwrap())
    }
}
