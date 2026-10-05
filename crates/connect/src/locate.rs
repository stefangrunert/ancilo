//! Finding the client CLIs (`claude`, `codex`) when Ancilo runs as an app:
//! macOS starts apps with a bare PATH (`/usr/bin:/bin:/usr/sbin:/sbin`),
//! while the CLIs live where the user installed them – Homebrew, npm,
//! `~/.local/bin` … And CLIs from npm start with `#!/usr/bin/env node`: they
//! need `node` on their PATH to run at all.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long the login shell may take to tell its PATH.
const SHELL_WAIT: Duration = Duration::from_secs(5);

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Where such CLIs are usually installed.
fn usual_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin"]
        .iter()
        .map(PathBuf::from)
        .collect();
    for rel in [
        ".local/bin",
        ".npm-global/bin",
        ".claude/local",
        ".bun/bin",
        ".volta/bin",
        ".cargo/bin",
        "Library/pnpm",
        ".yarn/bin",
        ".asdf/shims",
        ".local/share/mise/shims",
        ".nodenv/shims",
    ] {
        dirs.push(home.join(rel));
    }
    // nvm: the node versions installed, newest first.
    if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
        versions.sort();
        versions.reverse();
        dirs.extend(versions);
    }
    dirs
}

/// The PATH the user's login shell sets up – where `claude` is found in a
/// terminal. Asked once, waiting at most [`SHELL_WAIT`].
fn shell_path() -> Option<OsString> {
    static PATH: OnceLock<Option<OsString>> = OnceLock::new();
    PATH.get_or_init(|| {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
        let mut child = Command::new(shell)
            .args(["-ilc", "printf '\\n__ANCILO_PATH__%s\\n' \"$PATH\""])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < SHELL_WAIT => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    child.kill().ok();
                    child.wait().ok();
                    return None;
                }
            }
        }
        let mut out = String::new();
        child.stdout.take()?.read_to_string(&mut out).ok()?;
        out.lines()
            .find_map(|l| l.strip_prefix("__ANCILO_PATH__"))
            .filter(|p| !p.is_empty())
            .map(OsString::from)
    })
    .clone()
}

/// `first`, then the usual places, then Ancilo's own PATH – each folder once.
fn joined(first: Option<&OsStr>, home: &Path) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = first {
        dirs.extend(std::env::split_paths(p));
    }
    dirs.extend(usual_dirs(home));
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
    std::env::join_paths(dirs).unwrap_or_default()
}

/// The PATH the client CLIs get: the login shell's, the usual places, then
/// Ancilo's own.
pub fn user_path() -> OsString {
    joined(shell_path().as_deref(), &home())
}

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The program `bin` names – a path as it is, a bare name looked up in `path`.
pub fn locate_in(bin: &Path, path: &OsStr) -> Option<PathBuf> {
    if bin.components().count() > 1 {
        return executable(bin).then(|| bin.to_path_buf());
    }
    std::env::split_paths(path)
        .map(|dir| dir.join(bin))
        .find(|p| executable(p))
}

/// The program to start for `bin`, and the PATH to start it with (`None`:
/// not installed). A bare name is looked up where the user's terminal would
/// find it; a path keeps its own folder first on the PATH.
pub fn locate(bin: &Path) -> Option<(PathBuf, OsString)> {
    if bin.components().count() > 1 {
        let dir = bin.parent().map(Path::as_os_str);
        return executable(bin).then(|| (bin.to_path_buf(), joined(dir, &home())));
    }
    let path = user_path();
    locate_in(bin, &path).map(|p| (p, path))
}

/// What to tell when a client is not installed.
pub fn not_installed(bin: &Path) -> String {
    let name = bin
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (product, site) = match name.as_str() {
        "claude" => ("Claude Code", "https://claude.com/product/claude-code"),
        "codex" => ("Codex", "https://developers.openai.com/codex/cli"),
        _ => (name.as_str(), "its website"),
    };
    ancilo_core::msg(
        "connect.not_installed",
        &[("product", &product), ("command", &name), ("site", &site)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(dir: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[test]
    fn a_cli_is_found_where_the_terminal_would_find_it() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        tool(&a, "claude", 0o644); // not executable: passed over
        let real = tool(&b, "claude", 0o755);
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(locate_in(Path::new("claude"), &path), Some(real.clone()));
        assert_eq!(locate_in(Path::new("codex"), &path), None);
        // A path is taken as it is.
        assert_eq!(locate_in(&real, OsStr::new("")), Some(real.clone()));
        assert_eq!(locate_in(&a.join("claude"), OsStr::new("")), None);
        // A path's own folder comes first on its PATH (npm CLIs need `node` from there).
        let (found, path) = locate(&real).unwrap();
        assert_eq!(found, real);
        assert_eq!(std::env::split_paths(&path).next(), Some(b));
    }

    #[test]
    fn the_usual_places_follow_the_shells_path() {
        let home = Path::new("/Users/x");
        let path = joined(Some(OsStr::new("/shell/bin:/opt/homebrew/bin")), home);
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs[0], PathBuf::from("/shell/bin"));
        assert_eq!(dirs[1], PathBuf::from("/opt/homebrew/bin"));
        assert!(dirs.contains(&home.join(".npm-global/bin")));
        assert_eq!(
            dirs.iter()
                .filter(|d| *d == Path::new("/opt/homebrew/bin"))
                .count(),
            1,
            "each folder once"
        );
    }

    #[test]
    fn a_missing_client_says_what_to_do() {
        let m = not_installed(Path::new("claude"));
        assert!(m.starts_with("Claude Code is not installed"), "{m}");
        assert!(m.contains("claude.com"), "{m}");
    }

    /// By hand, on a machine with Claude Code and Codex installed, started
    /// with a bare PATH as macOS starts apps:
    /// `env -i HOME=$HOME SHELL=$SHELL PATH=/usr/bin:/bin <test binary> --ignored --exact locate::tests::the_installed_clients_are_found_from_an_apps_bare_path`
    #[test]
    #[ignore]
    fn the_installed_clients_are_found_from_an_apps_bare_path() {
        for name in ["claude", "codex"] {
            let (bin, path) = locate(Path::new(name)).expect(name);
            let out = Command::new(&bin)
                .arg("--version")
                .env("PATH", path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            println!(
                "{name}: {} – {}",
                bin.display(),
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
    }
}
