//! Finding – or starting – the Ancilo daemon. The app is only a window onto
//! it: the daemon keeps running without the app, so Claude Code and Codex can
//! always reach Ancilo.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ancilo_core::Paths;
use ancilo_core::version::{Compat, compat};

#[derive(Debug, Clone)]
pub struct Daemon {
    pub url: String,
    pub token: String,
}

/// The daemon's health answer (version, protocol), if it answers.
fn health(url: &str) -> Option<serde_json::Value> {
    let host = url.strip_prefix("http://")?;
    let mut s = TcpStream::connect_timeout(&host.parse().ok()?, Duration::from_millis(500)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    write!(s, "GET /api/v1/health HTTP/1.0\r\nHost: {host}\r\n\r\n").ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    if !(buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200")) {
        return None;
    }
    serde_json::from_str(buf.split("\r\n\r\n").nth(1)?).ok()
}

/// The running daemon of this user, if any, with its health answer.
fn running(paths: &Paths) -> Option<(Daemon, serde_json::Value)> {
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(paths.daemon_file()).ok()?).ok()?;
    let url = info["url"].as_str()?.to_string();
    let h = health(&url)?;
    let token = std::fs::read_to_string(paths.token_file())
        .ok()?
        .trim()
        .to_string();
    let d = Daemon { url, token };
    // Where this home's daemon once listened, another one may listen now –
    // another user's Ancilo on this Mac: only ours accepts our token.
    op(&d, "daemon_info")?;
    Some((d, h))
}

/// Calls a daemon operation without input; its answer, if any.
pub fn op(d: &Daemon, name: &str) -> Option<serde_json::Value> {
    op_with(d, name, &serde_json::json!({}))
}

/// Calls a daemon operation with `input`; its answer, if any.
pub fn op_with(d: &Daemon, name: &str, input: &serde_json::Value) -> Option<serde_json::Value> {
    let host = d.url.strip_prefix("http://")?;
    let addr = host.parse().ok()?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let body = input.to_string();
    write!(
        s,
        "POST /api/v1/ops/{name} HTTP/1.0\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        d.token,
        body.len()
    )
    .ok()?;
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    if !(buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200")) {
        return None;
    }
    serde_json::from_str(buf.split("\r\n\r\n").nth(1)?).ok()
}

/// Whether the user allowed automatic update checks (`get_update_settings`).
pub fn auto_update_checks(d: &Daemon) -> bool {
    op(d, "get_update_settings").is_some_and(|v| v["auto_check"] == true)
}

/// The running daemon of this user, if any.
pub fn find(paths: &Paths) -> Option<Daemon> {
    running(paths).map(|(d, _)| d)
}

/// The `ancilo` binary: `ANCILO_BIN`, next to the app, or on the PATH.
pub fn ancilo_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("ANCILO_BIN") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && dir.join("ancilo").exists()
    {
        return dir.join("ancilo");
    }
    PathBuf::from("ancilo")
}

/// Finds the daemon or starts it (`ancilo daemon start`) and waits for it.
pub fn ensure(paths: &Paths) -> Result<Daemon, String> {
    if let Some((d, h)) = running(paths) {
        // After an app update the daemon of the previous version may still run.
        let own = env!("CARGO_PKG_VERSION");
        let version = h["version"].as_str().unwrap_or("0.0.0");
        let protocol = h["protocol"].as_u64().map(|p| p as u32);
        match compat(own, version, protocol) {
            Compat::Same | Compat::UseNewer => return Ok(d),
            Compat::TooOld => {
                return Err(format!(
                    "Ancilo {version} is running, this app is {own} – please update the app"
                ));
            }
            Compat::Replace => {
                // An orderly stop: tasks and models are shut down as usual.
                let stopped = Command::new(ancilo_bin())
                    .args(["daemon", "stop"])
                    .envs(crate::variant::daemon_env(paths))
                    .status()
                    .map_err(|e| format!("cannot run `ancilo`: {e}"))?;
                if !stopped.success() {
                    return Err("could not stop the previous Ancilo version".into());
                }
            }
        }
    }
    let status = Command::new(ancilo_bin())
        .args(["daemon", "start"])
        .envs(crate::variant::daemon_env(paths))
        .status()
        .map_err(|e| format!("cannot start `ancilo`: {e}"))?;
    if !status.success() {
        return Err("`ancilo daemon start` failed".into());
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(d) = find(paths) {
            return Ok(d);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("the daemon did not come up within 20 s".into())
}

/// macOS: keeps the daemon running across logins (LaunchAgent). Idempotent.
#[cfg(target_os = "macos")]
pub fn install_launch_agent(paths: &Paths) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    let dir = PathBuf::from(home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let label = crate::variant::LAUNCH_AGENT;
    let plist = dir.join(format!("{label}.plist"));
    let bin = ancilo_bin();
    let env: String = crate::variant::daemon_env(paths)
        .iter()
        .map(|(k, v)| format!("<key>{k}</key><string>{v}</string>"))
        .collect();
    let env = if env.is_empty() {
        String::new()
    } else {
        format!("\n  <key>EnvironmentVariables</key><dict>{env}</dict>")
    };
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>daemon</string><string>run</string></array>{env}
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Background</string>
</dict>
</plist>
"#,
        bin.display()
    );
    if std::fs::read_to_string(&plist).ok().as_deref() != Some(content.as_str()) {
        std::fs::write(&plist, content).map_err(|e| e.to_string())?;
    }
    Ok(plist)
}
