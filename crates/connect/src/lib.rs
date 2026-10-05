//! Connects Claude Code and Codex to Ancilo – and disconnects them without
//! leaving anything behind.
//!
//! - Claude Code: a local plugin marketplace with the `ancilo` plugin (MCP
//!   server, a skill on when to delegate, a worker subagent)
//! - Codex: an MCP server entry plus a marked block in its global AGENTS.md
//!
//! The foreign programs are driven through their own CLIs (`claude plugin …`,
//! `codex mcp …`), never by editing their internal files. After connecting,
//! a real MCP handshake through `ancilo mcp` verifies the setup.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ancilo_core::{Error, NoInput, OpBuilder, Registry, Result};

pub mod locate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MARKETPLACE: &str = "ancilo-local";
pub const PLUGIN: &str = "ancilo";
const AGENTS_START: &str =
    "<!-- ancilo:start – managed by Ancilo, removed by `ancilo disconnect codex` -->";
const AGENTS_END: &str = "<!-- ancilo:end -->";

const SKILL: &str =
    include_str!("../../../plugins/claude-code/ancilo/skills/delegate-to-ancilo/SKILL.md");
const WORKER: &str = include_str!("../../../plugins/claude-code/ancilo/agents/ancilo-worker.md");
const CODEX_SNIPPET: &str = include_str!("../../../plugins/codex/AGENTS.snippet.md");

/// Where the clients and Ancilo itself live.
#[derive(Debug, Clone)]
pub struct Clients {
    pub claude_bin: PathBuf,
    pub codex_bin: PathBuf,
    /// The `ancilo` binary that MCP clients start (`ancilo mcp`).
    pub ancilo_bin: PathBuf,
    /// Ancilo's directory for client files (`<home>/clients`).
    pub dir: PathBuf,
    /// Codex home (`$CODEX_HOME` or `~/.codex`) for its global AGENTS.md.
    pub codex_home: PathBuf,
    /// Extra environment for the client CLIs (tests: isolated config dirs).
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConnectReport {
    pub client: String,
    pub connected: bool,
    /// What was done.
    pub actions: Vec<String>,
    /// A real MCP handshake through `ancilo mcp` succeeded.
    pub verified: bool,
    pub notes: String,
}

impl Clients {
    fn run(&self, bin: &Path, args: &[&str]) -> Result<String> {
        // Found where the user's terminal finds it – the app's own PATH is
        // bare – and started with that PATH (npm CLIs need `node`).
        let (program, path) =
            locate::locate(bin).ok_or_else(|| Error::unavailable(locate::not_installed(bin)))?;
        let out = Command::new(&program)
            .args(args)
            .env("PATH", path)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::unavailable(format!("cannot run {}: {e}", bin.display())))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            Ok(text)
        } else {
            Err(Error::Conflict(format!(
                "{} {}: {}",
                bin.display(),
                args.join(" "),
                text.trim()
            )))
        }
    }

    /// Like `run`, but some failures are expected (already added, not installed).
    fn run_tolerant(&self, bin: &Path, args: &[&str], ok_if: &[&str]) -> Result<String> {
        match self.run(bin, args) {
            Ok(t) => Ok(t),
            Err(e) if ok_if.iter().any(|s| e.message().to_lowercase().contains(s)) => {
                Ok(e.message())
            }
            Err(e) => Err(e),
        }
    }

    fn marketplace_dir(&self) -> PathBuf {
        self.dir.join("claude-plugins")
    }

    /// Writes the plugin package (with the absolute path of `ancilo`) and
    /// returns the marketplace directory; the plugin is in `<dir>/ancilo`.
    pub fn write_plugin(&self) -> Result<PathBuf> {
        let root = self.marketplace_dir();
        let plugin = root.join(PLUGIN);
        std::fs::create_dir_all(root.join(".claude-plugin"))?;
        std::fs::create_dir_all(plugin.join(".claude-plugin"))?;
        std::fs::create_dir_all(plugin.join("skills/delegate-to-ancilo"))?;
        std::fs::create_dir_all(plugin.join("agents"))?;
        let version = ancilo_core::VERSION;
        std::fs::write(
            root.join(".claude-plugin/marketplace.json"),
            serde_json::to_string_pretty(&json!({
                "name": MARKETPLACE,
                "owner": {"name": "Ancilo"},
                "metadata": {"description": "Ancilo on this machine"},
                "plugins": [{"name": PLUGIN, "source": "./ancilo", "version": version,
                             "description": "Delegate well-defined coding tasks to a local model"}]
            }))?,
        )?;
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            serde_json::to_string_pretty(&json!({
                "name": PLUGIN, "version": version,
                "description": "Delegate well-defined coding tasks to a local model via Ancilo",
                "author": {"name": "Ancilo"}, "license": "Apache-2.0"
            }))?,
        )?;
        std::fs::write(
            plugin.join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {"ancilo": {
                    "command": self.ancilo_bin,
                    "args": ["mcp"],
                    // The MCP server must reach this Ancilo (its home), not the default one.
                    "env": self.env.iter().cloned().collect::<std::collections::BTreeMap<String, String>>(),
                }}
            }))?,
        )?;
        std::fs::write(plugin.join("skills/delegate-to-ancilo/SKILL.md"), SKILL)?;
        std::fs::write(plugin.join("agents/ancilo-worker.md"), WORKER)?;
        Ok(root)
    }

    pub fn connect_claude_code(&self) -> Result<ConnectReport> {
        let root = self.write_plugin()?;
        let mut actions = vec![format!("wrote the plugin package to {}", root.display())];
        let dir = root.display().to_string();
        self.run_tolerant(
            &self.claude_bin,
            &["plugin", "marketplace", "add", &dir],
            &["already"],
        )?;
        // Refresh in case an older version of the package was added before.
        self.run_tolerant(
            &self.claude_bin,
            &["plugin", "marketplace", "update", MARKETPLACE],
            &["not found", "no marketplace"],
        )?;
        actions.push(format!("added the marketplace '{MARKETPLACE}'"));
        let id = format!("{PLUGIN}@{MARKETPLACE}");
        self.run_tolerant(&self.claude_bin, &["plugin", "install", &id], &["already"])?;
        actions.push(format!(
            "installed the plugin '{id}' (MCP server, delegation skill, worker subagent)"
        ));
        let listed = self.run(&self.claude_bin, &["plugin", "list"])?;
        let connected = listed.contains(PLUGIN);
        let verified = connected && mcp_handshake(&self.ancilo_bin, &self.env).is_ok();
        Ok(ConnectReport {
            client: "claude_code".into(),
            connected,
            actions,
            verified,
            notes: "Restart running Claude Code sessions to load the plugin. Claude now knows when to hand tasks to Ancilo.".into(),
        })
    }

    pub fn disconnect_claude_code(&self) -> Result<ConnectReport> {
        let id = format!("{PLUGIN}@{MARKETPLACE}");
        let tolerate = [
            "not installed",
            "not found",
            "no plugin",
            "no marketplace",
            "does not exist",
        ];
        self.run_tolerant(&self.claude_bin, &["plugin", "uninstall", &id], &tolerate)?;
        self.run_tolerant(
            &self.claude_bin,
            &["plugin", "marketplace", "remove", MARKETPLACE],
            &tolerate,
        )?;
        let root = self.marketplace_dir();
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        Ok(ConnectReport {
            client: "claude_code".into(),
            connected: false,
            actions: vec![
                format!("uninstalled '{id}'"),
                format!("removed the marketplace '{MARKETPLACE}'"),
                format!("deleted {}", root.display()),
            ],
            verified: false,
            notes: String::new(),
        })
    }

    fn agents_file(&self) -> PathBuf {
        self.codex_home.join("AGENTS.md")
    }

    fn remove_agents_block(&self) -> Result<bool> {
        let file = self.agents_file();
        let Ok(text) = std::fs::read_to_string(&file) else {
            return Ok(false);
        };
        let (Some(start), Some(end)) = (text.find(AGENTS_START), text.find(AGENTS_END)) else {
            return Ok(false);
        };
        let mut rest = String::new();
        rest.push_str(text[..start].trim_end_matches('\n'));
        let after = &text[end + AGENTS_END.len()..];
        if !rest.is_empty() && !after.trim().is_empty() {
            rest.push_str("\n\n");
        }
        rest.push_str(after.trim_start_matches('\n'));
        if rest.trim().is_empty() {
            std::fs::remove_file(&file)?;
        } else {
            let mut rest = rest.trim_end().to_string();
            rest.push('\n');
            std::fs::write(&file, rest)?;
        }
        Ok(true)
    }

    pub fn connect_codex(&self) -> Result<ConnectReport> {
        let mut actions = Vec::new();
        // Re-adding keeps the entry current (path of `ancilo` may have changed).
        self.run_tolerant(
            &self.codex_bin,
            &["mcp", "remove", PLUGIN],
            &["not found", "no mcp server", "does not exist"],
        )?;
        let bin = self.ancilo_bin.display().to_string();
        let envs: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut args: Vec<&str> = vec!["mcp", "add", PLUGIN];
        for e in &envs {
            args.push("--env");
            args.push(e);
        }
        args.extend(["--", &bin, "mcp"]);
        self.run(&self.codex_bin, &args)?;
        actions.push(format!("added the MCP server 'ancilo' ({bin} mcp)"));
        self.remove_agents_block()?;
        std::fs::create_dir_all(&self.codex_home)?;
        let file = self.agents_file();
        let existing = std::fs::read_to_string(&file).unwrap_or_default();
        let mut text = existing.trim_end().to_string();
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!(
            "{AGENTS_START}\n{}\n{AGENTS_END}\n",
            CODEX_SNIPPET.trim()
        ));
        std::fs::write(&file, text)?;
        actions.push(format!(
            "added delegation instructions to {}",
            file.display()
        ));
        let listed = self.run(&self.codex_bin, &["mcp", "list"])?;
        let connected = listed.contains(PLUGIN);
        let verified = connected && mcp_handshake(&self.ancilo_bin, &self.env).is_ok();
        Ok(ConnectReport {
            client: "codex".into(),
            connected,
            actions,
            verified,
            notes: "New Codex sessions can now delegate to Ancilo.".into(),
        })
    }

    pub fn disconnect_codex(&self) -> Result<ConnectReport> {
        self.run_tolerant(
            &self.codex_bin,
            &["mcp", "remove", PLUGIN],
            &["not found", "no mcp server", "does not exist"],
        )?;
        let removed = self.remove_agents_block()?;
        Ok(ConnectReport {
            client: "codex".into(),
            connected: false,
            actions: vec![
                "removed the MCP server 'ancilo'".into(),
                if removed {
                    format!(
                        "removed the delegation instructions from {}",
                        self.agents_file().display()
                    )
                } else {
                    "no instructions to remove".into()
                },
            ],
            verified: false,
            notes: String::new(),
        })
    }
}

/// Starts `ancilo mcp`, performs `initialize` + `tools/list` and checks that
/// `delegate` is offered.
pub fn mcp_handshake(ancilo_bin: &Path, env: &[(String, String)]) -> Result<Vec<String>> {
    let mut child = Command::new(ancilo_bin)
        .arg("mcp")
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            Error::unavailable(format!("cannot start {} mcp: {e}", ancilo_bin.display()))
        })?;
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "ancilo-connect", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    for m in &msgs {
        writeln!(stdin, "{m}").map_err(Error::internal)?;
    }
    stdin.flush().map_err(Error::internal)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut tools = None;
    while Instant::now() < deadline {
        let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["id"] == 2 {
            tools = v["result"]["tools"].as_array().map(|t| {
                t.iter()
                    .filter_map(|x| x["name"].as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            });
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    match tools {
        Some(t) if t.iter().any(|n| n == "delegate") => Ok(t),
        Some(t) => Err(Error::internal(format!(
            "MCP server does not offer 'delegate' ({t:?})"
        ))),
        None => Err(Error::unavailable("no answer from `ancilo mcp`")),
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PluginDir {
    /// Load it for one session: `claude --plugin-dir <path>`.
    pub path: PathBuf,
}

fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

pub fn register(registry: &mut Registry, clients: Clients) {
    let c = clients.clone();
    registry.register(
        OpBuilder::new("claude_plugin_dir")
            .summary("Write the Ancilo plugin for Claude Code without installing it (for `claude --plugin-dir`)")
            .manage()
            .handler(move |_ctx, _i: NoInput| {
                let c = c.clone();
                async move {
                    let root = c.write_plugin()?;
                    Ok(PluginDir { path: root.join(PLUGIN) })
                }
            }),
    );
    type Action = fn(&Clients) -> Result<ConnectReport>;
    let ops: [(&'static str, &'static str, Action); 4] = [
        (
            "connect_claude_code",
            "Let Claude Code delegate to Ancilo (installs the Ancilo plugin)",
            Clients::connect_claude_code,
        ),
        (
            "disconnect_claude_code",
            "Remove the Ancilo plugin from Claude Code",
            Clients::disconnect_claude_code,
        ),
        (
            "connect_codex",
            "Let Codex delegate to Ancilo (MCP server and instructions)",
            Clients::connect_codex,
        ),
        (
            "disconnect_codex",
            "Remove Ancilo from Codex",
            Clients::disconnect_codex,
        ),
    ];
    for (name, summary, action) in ops {
        let c = clients.clone();
        registry.register(
            OpBuilder::new(name)
                .summary(summary)
                .description("Changes the configuration of another program (through its own CLI); fully reversible with the matching disconnect/connect operation.")
                .manage()
                .consequential()
                .handler(move |_ctx, _i: NoInput| {
                    let c = c.clone();
                    async move {
                        let c2 = c.clone();
                        let report = tokio::task::spawn_blocking(move || action(&c2))
                            .await
                            .map_err(Error::internal)?;
                        if let Ok(r) = &report {
                            c.remember(r);
                        }
                        report
                    }
                }),
        );
    }
    let c = clients;
    registry.register(
        OpBuilder::new("connections")
            .summary("Whether Claude Code and Codex are connected to Ancilo")
            .handler(move |_ctx, _i: NoInput| {
                let c = c.clone();
                async move { Ok(c.connections()) }
            }),
    );
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionState {
    /// `claude_code` or `codex`
    pub client: String,
    pub connected: bool,
    /// The last connect was verified with a real MCP handshake.
    pub verified: bool,
    pub since: Option<String>,
}

impl Clients {
    fn state_file(&self) -> PathBuf {
        self.dir.join("connections.json")
    }

    /// Records the outcome of a connect/disconnect for the status display.
    fn remember(&self, r: &ConnectReport) {
        let mut all: std::collections::BTreeMap<String, ConnectionState> =
            std::fs::read_to_string(self.state_file())
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
        all.insert(
            r.client.clone(),
            ConnectionState {
                client: r.client.clone(),
                connected: r.connected,
                verified: r.verified,
                since: Some(chrono::Utc::now().to_rfc3339()),
            },
        );
        std::fs::create_dir_all(&self.dir).ok();
        std::fs::write(
            self.state_file(),
            serde_json::to_string_pretty(&all).unwrap_or_default(),
        )
        .ok();
    }

    /// Disconnects every client this home connected (removing Ancilo):
    /// each with its outcome; a failure does not stop the others.
    ///
    /// Every Ancilo connects under the same names: a connection another
    /// Ancilo made since (the installed app beside Ancilo Dev) is not this
    /// home's – it stays.
    pub fn disconnect_all(&self) -> Vec<(String, Result<ConnectReport>)> {
        self.connections()
            .into_iter()
            .filter(|c| c.connected)
            .filter_map(|c| {
                let ours = match c.client.as_str() {
                    "claude_code" => self.claude_is_ours(),
                    _ => self.codex_is_ours(),
                };
                let report = match (ours, c.client.as_str()) {
                    (Some(false), client) => {
                        self.remember(&ConnectReport {
                            client: client.into(),
                            connected: false,
                            actions: Vec::new(),
                            verified: false,
                            notes: "connected to another Ancilo".into(),
                        });
                        return None;
                    }
                    (_, "claude_code") => self.disconnect_claude_code(),
                    _ => self.disconnect_codex(),
                };
                if let Ok(r) = &report {
                    self.remember(r);
                }
                Some((c.client, report))
            })
            .collect()
    }

    /// Whether Claude Code's Ancilo plugin comes from this home: Claude's
    /// list of marketplaces names the directory. `None`: cannot tell.
    fn claude_is_ours(&self) -> Option<bool> {
        let config = self
            .env
            .iter()
            .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
            .map(|(_, v)| PathBuf::from(v))
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
            .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
        let text = std::fs::read_to_string(config.join("plugins/known_marketplaces.json")).ok()?;
        let all: serde_json::Value = serde_json::from_str(&text).ok()?;
        let Some(m) = all.get(MARKETPLACE) else {
            return Some(false);
        };
        let path = m["source"]["path"]
            .as_str()
            .or(m["installLocation"].as_str())?;
        Some(same_path(Path::new(path), &self.marketplace_dir()))
    }

    /// Whether Codex's `ancilo` MCP server is this home's (its `ANCILO_HOME`).
    /// `None`: cannot tell.
    fn codex_is_ours(&self) -> Option<bool> {
        let text = std::fs::read_to_string(self.codex_home.join("config.toml")).ok()?;
        let config: toml::Value = toml::from_str(&text).ok()?;
        let Some(server) = config.get("mcp_servers").and_then(|s| s.get("ancilo")) else {
            return Some(false);
        };
        let home = server.get("env")?.get("ANCILO_HOME")?.as_str()?;
        let ours = self.env.iter().find(|(k, _)| k == "ANCILO_HOME")?;
        Some(same_path(Path::new(home), Path::new(&ours.1)))
    }

    pub fn connections(&self) -> Vec<ConnectionState> {
        let all: std::collections::BTreeMap<String, ConnectionState> =
            std::fs::read_to_string(self.state_file())
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
        ["claude_code", "codex"]
            .iter()
            .map(|c| {
                all.get(*c).cloned().unwrap_or(ConnectionState {
                    client: c.to_string(),
                    connected: false,
                    verified: false,
                    since: None,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake `claude` and `codex` CLIs that keep their state in files.
    fn fakes(dir: &Path) -> (PathBuf, PathBuf) {
        let claude = dir.join("claude");
        std::fs::write(
            &claude,
            r#"#!/bin/sh
S="$FAKE_STATE"
echo "$@" >> "$S/claude.log"
case "$1 $2" in
  "plugin marketplace")
    case "$3" in
      add) grep -qx "ancilo-local" "$S/mkt" 2>/dev/null && { echo "marketplace already added" >&2; exit 1; }; echo ancilo-local >> "$S/mkt";;
      update) grep -qx "ancilo-local" "$S/mkt" 2>/dev/null || { echo "marketplace not found" >&2; exit 1; };;
      remove) grep -qx "ancilo-local" "$S/mkt" 2>/dev/null || { echo "marketplace not found" >&2; exit 1; }; : > "$S/mkt";;
    esac;;
  "plugin install") grep -qx "$3" "$S/plugins" 2>/dev/null && { echo "already installed" >&2; exit 1; }; echo "$3" >> "$S/plugins";;
  "plugin uninstall") grep -qx "$3" "$S/plugins" 2>/dev/null || { echo "plugin not installed" >&2; exit 1; }; : > "$S/plugins";;
  "plugin list") cat "$S/plugins" 2>/dev/null;;
esac
"#,
        )
        .unwrap();
        let codex = dir.join("codex");
        std::fs::write(
            &codex,
            r#"#!/bin/sh
S="$FAKE_STATE"
echo "$@" >> "$S/codex.log"
case "$1 $2" in
  "mcp add") echo "$*" >> "$S/mcp";;
  "mcp remove") grep -q "add $3 " "$S/mcp" 2>/dev/null || { echo "No MCP server named '$3' found" >&2; exit 1; }; : > "$S/mcp";;
  "mcp list") cat "$S/mcp" 2>/dev/null;;
esac
"#,
        )
        .unwrap();
        for f in [&claude, &codex] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (claude, codex)
    }

    fn clients(dir: &Path) -> Clients {
        let (claude_bin, codex_bin) = fakes(dir);
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        Clients {
            claude_bin,
            codex_bin,
            ancilo_bin: PathBuf::from("/nonexistent/ancilo"),
            dir: dir.join("clients"),
            codex_home: dir.join("codex-home"),
            env: vec![
                ("FAKE_STATE".into(), state.display().to_string()),
                // Never the user's own Claude configuration.
                (
                    "CLAUDE_CONFIG_DIR".into(),
                    dir.join("claude-config").display().to_string(),
                ),
            ],
        }
    }

    // covers: M3-AC-09
    #[test]
    fn claude_code_connect_is_idempotent_and_disconnect_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let c = clients(dir.path());
        let r1 = c.connect_claude_code().unwrap();
        assert!(r1.connected);
        assert!(
            !r1.verified,
            "no ancilo binary here → handshake fails honestly"
        );
        let r2 = c.connect_claude_code().unwrap();
        assert!(r2.connected);
        let plugins = std::fs::read_to_string(dir.path().join("state/plugins")).unwrap();
        assert_eq!(plugins.lines().count(), 1, "installed once");
        let mcp =
            std::fs::read_to_string(dir.path().join("clients/claude-plugins/ancilo/.mcp.json"))
                .unwrap();
        assert!(
            mcp.contains("/nonexistent/ancilo"),
            "absolute path of ancilo"
        );
        assert!(
            dir.path()
                .join("clients/claude-plugins/ancilo/skills/delegate-to-ancilo/SKILL.md")
                .exists()
        );
        c.disconnect_claude_code().unwrap();
        assert!(!dir.path().join("clients/claude-plugins").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("state/plugins"))
                .unwrap()
                .trim(),
            ""
        );
        // Disconnecting again is fine.
        c.disconnect_claude_code().unwrap();
    }

    // covers: M3-AC-09
    #[test]
    fn codex_connect_keeps_user_instructions_and_disconnect_restores_them_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let c = clients(dir.path());
        std::fs::create_dir_all(&c.codex_home).unwrap();
        let original = "# My rules\n\nAlways write tests.\n";
        std::fs::write(c.agents_file(), original).unwrap();
        c.connect_codex().unwrap();
        c.connect_codex().unwrap();
        let text = std::fs::read_to_string(c.agents_file()).unwrap();
        assert!(text.starts_with(original.trim_end()));
        assert_eq!(text.matches(AGENTS_START).count(), 1, "block added once");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("state/mcp"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        c.disconnect_codex().unwrap();
        assert_eq!(std::fs::read_to_string(c.agents_file()).unwrap(), original);
        // Without prior AGENTS.md, disconnect removes the file again.
        std::fs::remove_file(c.agents_file()).unwrap();
        c.connect_codex().unwrap();
        c.disconnect_codex().unwrap();
        assert!(!c.agents_file().exists());
    }

    /// Removing Ancilo disconnects what it connected – and touches nothing
    /// it did not connect.
    #[test]
    fn disconnect_all_undoes_only_what_this_home_connected() {
        let dir = tempfile::tempdir().unwrap();
        let c = clients(dir.path());
        assert!(
            c.disconnect_all().is_empty(),
            "nothing connected, nothing run"
        );
        assert!(!dir.path().join("state/claude.log").exists());
        let r = c.connect_claude_code().unwrap();
        c.remember(&r);
        let done = c.disconnect_all();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, "claude_code");
        assert!(done[0].1.is_ok());
        assert!(c.connections().iter().all(|s| !s.connected));
        assert!(
            !dir.path().join("state/codex.log").exists(),
            "Codex was never connected"
        );
    }

    /// Ancilo Dev beside the installed app: both connect under the same
    /// names. Removing one never disconnects the other.
    #[test]
    fn disconnect_all_leaves_another_ancilos_connection() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = clients(dir.path());
        c.env.push(("ANCILO_HOME".into(), "/homes/dev".into()));
        for client in ["claude_code", "codex"] {
            c.remember(&ConnectReport {
                client: client.into(),
                connected: true,
                actions: Vec::new(),
                verified: true,
                notes: String::new(),
            });
        }
        // Since then the installed app connected both.
        let plugins = dir.path().join("claude-config/plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(
            plugins.join("known_marketplaces.json"),
            r#"{"ancilo-local": {"source": {"source": "directory", "path": "/homes/app/clients/claude-plugins"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&c.codex_home).unwrap();
        std::fs::write(
            c.codex_home.join("config.toml"),
            "[mcp_servers.ancilo]\ncommand = \"ancilo\"\n\n[mcp_servers.ancilo.env]\nANCILO_HOME = \"/homes/app\"\n",
        )
        .unwrap();
        assert!(c.disconnect_all().is_empty());
        assert!(!dir.path().join("state/claude.log").exists());
        assert!(!dir.path().join("state/codex.log").exists());
        assert!(c.connections().iter().all(|s| !s.connected));
        // This home's own connection is undone.
        std::fs::write(
            c.codex_home.join("config.toml"),
            "[mcp_servers.ancilo.env]\nANCILO_HOME = \"/homes/dev\"\n",
        )
        .unwrap();
        c.remember(&ConnectReport {
            client: "codex".into(),
            connected: true,
            actions: Vec::new(),
            verified: true,
            notes: String::new(),
        });
        let done = c.disconnect_all();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, "codex");
    }
}
