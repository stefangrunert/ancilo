//! System › Remove Ancilo: everything `ancilo uninstall` removes, and what
//! only the app set up – the login item, the `ancilo` command a local
//! install linked, the window's own data at macOS and the app itself.
//!
//! The page calls it (IPC `remove_ancilo`) after the user confirmed in a
//! dialog; the app quits afterwards.

use std::path::{Path, PathBuf};
use std::process::Command;

use ancilo_core::Paths;
use ancilo_core::messages::msg;

/// What the page shows afterwards: the app is gone, or what is left to do.
#[derive(Debug, serde::Serialize)]
pub struct Outcome {
    /// Steps that did not work – the user does them by hand.
    pub problems: Vec<String>,
}

pub fn remove(paths: &Paths, bundle_id: &str, keep_data: bool) -> Result<Outcome, String> {
    let mut problems = Vec::new();
    // First the login item: launchd would start the daemon again.
    remove_login_item();
    let mut uninstall = Command::new(crate::daemon::ancilo_bin());
    uninstall
        .args(["uninstall", "--yes", "--json"])
        .envs(crate::variant::daemon_env(paths));
    if keep_data {
        uninstall.arg("--keep-data");
    }
    let out = uninstall
        .output()
        .map_err(|e| format!("cannot run `ancilo uninstall`: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
        problems.extend(
            v["problems"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_str().map(String::from)),
        );
    }
    let bundle = std::env::current_exe().ok().and_then(|e| bundle_of(&e));
    if let Some(bundle) = &bundle {
        remove_commands(bundle);
        if installed(bundle) {
            if let Err(e) = std::fs::remove_dir_all(bundle) {
                problems.push(msg(
                    "remove.app_left",
                    &[("path", &bundle.display()), ("why", &e)],
                ));
            }
        } else {
            problems.push(msg("remove.app_elsewhere", &[("path", &bundle.display())]));
        }
    }
    if !keep_data {
        forget_window_data(bundle_id);
    }
    Ok(Outcome { problems })
}

fn remove_login_item() {
    let label = crate::variant::LAUNCH_AGENT;
    if let Some(uid) = uid() {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{label}")])
            .output();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::fs::remove_file(
            PathBuf::from(home).join(format!("Library/LaunchAgents/{label}.plist")),
        );
    }
}

fn uid() -> Option<String> {
    let out = Command::new("id").arg("-u").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|u| !u.is_empty())
}

/// The `.app` a binary belongs to.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// Installed where it stays: not the disk image it was opened from, not a
/// copy macOS runs from a hidden place (App Translocation).
pub fn installed(bundle: &Path) -> bool {
    let s = bundle.to_string_lossy();
    !s.starts_with("/Volumes/") && !s.contains("/AppTranslocation/")
}

/// `ancilo` / `ancilo-dev` in `~/.local/bin` – only if they lead into this app.
fn remove_commands(bundle: &Path) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let dir = PathBuf::from(home).join(".local/bin");
    let inside = bundle.to_string_lossy().to_string();
    for name in ["ancilo", "ancilo-dev"] {
        let p = dir.join(name);
        let leads_here = match std::fs::read_link(&p) {
            Ok(target) => target.starts_with(bundle),
            // The development command is a small script naming the app.
            Err(_) => std::fs::read_to_string(&p).is_ok_and(|t| t.contains(&inside)),
        };
        if leads_here {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// What macOS keeps for the window (web storage, caches, preferences) – once
/// the app has quit; WebKit writes until then.
fn forget_window_data(bundle_id: &str) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let lib = PathBuf::from(home).join("Library");
    let quote = |p: PathBuf| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let dirs: Vec<String> = [
        lib.join("WebKit").join(bundle_id),
        lib.join("Caches").join(bundle_id),
        lib.join("HTTPStorages").join(bundle_id),
        lib.join("Saved Application State")
            .join(format!("{bundle_id}.savedState")),
    ]
    .into_iter()
    .map(quote)
    .collect();
    let script = format!(
        "sleep 3; rm -rf {}; defaults delete {} 2>/dev/null",
        dirs.join(" "),
        quote(PathBuf::from(bundle_id))
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = Command::new("/bin/sh")
            .args(["-c", &script])
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_is_found_from_its_binary() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Ancilo.app/Contents/MacOS/ancilo-app"
            )),
            Some(PathBuf::from("/Applications/Ancilo.app"))
        );
        assert_eq!(bundle_of(Path::new("/usr/local/bin/ancilo")), None);
    }

    #[test]
    fn an_app_on_its_disk_image_is_left_alone() {
        assert!(installed(Path::new("/Applications/Ancilo.app")));
        assert!(!installed(Path::new("/Volumes/Ancilo/Ancilo.app")));
        assert!(!installed(Path::new(
            "/private/var/folders/x/AppTranslocation/1/d/Ancilo.app"
        )));
    }
}
