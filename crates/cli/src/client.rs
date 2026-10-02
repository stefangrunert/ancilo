//! Talks to the daemon; starts it when needed.

use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

use ancilo_core::version::Compat;
use ancilo_core::{Config, Error, Event, Paths, Result};
use ancilo_daemon::DaemonInfo;
use futures::{Stream, StreamExt};
use serde_json::Value;

pub struct Client {
    http: reqwest::Client,
    pub url: String,
    token: String,
}

async fn health(http: &reqwest::Client, url: &str) -> Option<Value> {
    let r = http
        .get(format!("{url}/api/v1/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

async fn healthy(http: &reqwest::Client, url: &str) -> bool {
    health(http, url).await.is_some()
}

fn spawn_daemon(paths: &Paths) -> Result<()> {
    let exe = std::env::current_exe()?;
    paths.ensure()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.logs_dir().join("daemon.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: closing the terminal does not stop the daemon.
        cmd.process_group(0);
    }
    cmd.spawn()
        .map_err(|e| Error::internal(format!("cannot start the daemon: {e}")))?;
    Ok(())
}

impl Client {
    /// Connects to the running daemon of this home, starting it if needed.
    pub async fn connect(paths: &Paths, autostart: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(Error::internal)?;
        let find = || -> Option<String> { DaemonInfo::read(paths).map(|i| i.url) };
        if let Some(url) = find()
            && let Some(h) = health(&http, &url).await
        {
            let client = Self::with_token(http.clone(), url, paths)?;
            if !autostart {
                return Ok(client);
            }
            // After an update the daemon of the previous version may still run.
            let own = env!("CARGO_PKG_VERSION");
            let running = h["version"].as_str().unwrap_or("0.0.0");
            let protocol = h["protocol"].as_u64().map(|p| p as u32);
            match ancilo_core::version::compat(own, running, protocol) {
                Compat::Same | Compat::UseNewer => return Ok(client),
                Compat::TooOld => {
                    return Err(Error::Conflict(format!(
                        "this `ancilo` ({own}) is older than the running Ancilo ({running}) – update it (e.g. `brew upgrade ancilo`)"
                    )));
                }
                Compat::Replace => {
                    eprintln!(
                        "Ancilo was updated ({running} → {own}) – restarting the background service …"
                    );
                    client.stop_and_wait(paths).await?;
                }
            }
        }
        if !autostart {
            return Err(Error::unavailable(
                "the Ancilo daemon is not running – start it with `ancilo daemon start`",
            ));
        }
        // A stale daemon.json from a crashed daemon must not confuse us.
        std::fs::remove_file(paths.daemon_file()).ok();
        let config = Config::load(paths)?;
        spawn_daemon(paths)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(url) = find()
                && healthy(&http, &url).await
            {
                return Self::with_token(http, url, paths);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(Error::unavailable(format!(
            "the Ancilo daemon did not start (port {}); see {}",
            config.port,
            paths.logs_dir().join("daemon.log").display()
        )))
    }

    /// Asks the daemon to shut down and returns once its process is gone – a
    /// new daemon must not start while the old one still stops its models.
    pub async fn stop_and_wait(&self, paths: &Paths) -> Result<()> {
        let pid = DaemonInfo::read(paths).map(|i| i.pid);
        self.call("daemon_shutdown", serde_json::json!({}), true)
            .await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline
            && pid.is_some_and(|p| {
                std::process::Command::new("kill")
                    .args(["-0", &p.to_string()])
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    fn with_token(http: reqwest::Client, url: String, paths: &Paths) -> Result<Self> {
        let token = std::fs::read_to_string(paths.token_file())
            .map_err(|e| Error::internal(format!("cannot read the access token: {e}")))?
            .trim()
            .to_string();
        Ok(Self { http, url, token })
    }

    /// Calls an operation. `confirm`: the user's command itself expresses the
    /// intent, so consequential operations run without a second question.
    pub async fn call(&self, op: &str, input: Value, confirm: bool) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/api/v1/ops/{op}", self.url))
            .bearer_auth(&self.token)
            .header("x-ancilo-surface", "cli")
            .header("x-ancilo-confirm", if confirm { "true" } else { "false" })
            .json(&input)
            .send()
            .await
            .map_err(|e| Error::unavailable(format!("cannot reach the Ancilo daemon: {e}")))?;
        let ok = resp.status().is_success();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| Error::internal(format!("invalid answer: {e}")))?;
        if ok {
            Ok(body)
        } else {
            Err(Error::from_code(
                body["error"]["code"].as_str().unwrap_or("internal"),
                body["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string(),
            ))
        }
    }

    /// Forwards one MCP message; `None` for notifications (HTTP 202).
    pub async fn mcp(&self, message: &str) -> Result<Option<String>> {
        let resp = self
            .http
            .post(format!("{}/mcp", self.url))
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .body(message.to_string())
            .send()
            .await
            .map_err(|e| Error::unavailable(format!("cannot reach the Ancilo daemon: {e}")))?;
        if resp.status().as_u16() == 202 {
            return Ok(None);
        }
        let text = resp
            .text()
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        Ok((!text.trim().is_empty()).then_some(text))
    }

    /// GET a JSON resource of the control API.
    pub async fn get(&self, path: &str) -> Result<Value> {
        self.http
            .get(format!("{}{path}", self.url))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Error::unavailable(format!("cannot reach the Ancilo daemon: {e}")))?
            .json()
            .await
            .map_err(|e| Error::internal(format!("invalid answer: {e}")))
    }

    /// Live events (optionally only about `subject`).
    pub async fn events(&self, subject: Option<&str>) -> Result<impl Stream<Item = Event> + use<>> {
        let mut url = format!("{}/api/v1/events", self.url);
        if let Some(s) = subject {
            url.push_str(&format!("?subject={s}"));
        }
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Error::unavailable(format!("cannot reach the Ancilo daemon: {e}")))?;
        let bytes = resp.bytes_stream();
        let stream =
            futures::stream::unfold((bytes, String::new()), |(mut bytes, mut buf)| async move {
                loop {
                    if let Some(end) = buf.find("\n\n") {
                        let block: String = buf.drain(..end + 2).collect();
                        let data: String = block
                            .lines()
                            .filter_map(|l| l.strip_prefix("data:"))
                            .map(str::trim_start)
                            .collect::<Vec<_>>()
                            .join("\n");
                        if let Ok(e) = serde_json::from_str::<Event>(&data) {
                            return Some((e, (bytes, buf)));
                        }
                        continue;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => buf.push_str(&String::from_utf8_lossy(&chunk)),
                        _ => return None,
                    }
                }
            });
        Ok(stream)
    }
}

/// Writes a status line; on a terminal the line is updated in place.
pub struct StatusLine {
    tty: bool,
    last: String,
}

impl StatusLine {
    pub fn new() -> Self {
        use std::io::IsTerminal;
        Self {
            tty: std::io::stderr().is_terminal(),
            last: String::new(),
        }
    }

    pub fn update(&mut self, text: &str) {
        if text == self.last {
            return;
        }
        let mut err = std::io::stderr();
        if self.tty {
            let _ = write!(err, "\r\x1b[2K{text}");
        } else {
            let _ = writeln!(err, "{text}");
        }
        let _ = err.flush();
        self.last = text.to_string();
    }

    pub fn finish(&mut self) {
        if self.tty && !self.last.is_empty() {
            eprintln!();
        }
        self.last.clear();
    }
}
