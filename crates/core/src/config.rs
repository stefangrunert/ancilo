use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Paths, Result};

/// Default port of the daemon (loopback only).
pub const DEFAULT_PORT: u16 = 7424;

/// Ancilo configuration (`config.toml`). Deliberately small: users should never
/// need to edit it. Every field has a sensible default; tests override
/// endpoints and binaries to point at fakes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Loopback port of the daemon. `0` picks a free port (tests).
    pub port: u16,
    /// Hugging Face endpoint.
    pub hf_endpoint: String,
    /// Base URL for llama.cpp release downloads.
    pub github_endpoint: String,
    /// Where a newer model catalog is fetched from – only when the user opens
    /// the model choice.
    pub catalog_url: String,
    /// Use this `llama-server` binary instead of the one shipped with Ancilo.
    pub llama_server_bin: Option<PathBuf>,
    /// Download the pinned llama.cpp build when none is installed.
    /// `None`: only in development builds – packages ship llama.cpp, and
    /// otherwise `ancilo llama install` does it explicitly.
    pub llama_auto_install: Option<bool>,
    /// RAM kept free for the operating system and other apps, in GiB.
    /// `None`: automatic (20 % of RAM, at most 8 GiB).
    pub ram_reserve_gib: Option<f64>,
    /// Directories scanned for existing GGUF files (LM Studio, Ollama, HF
    /// cache). `None` = platform defaults.
    pub model_search_dirs: Option<Vec<PathBuf>>,
    /// Override the detected hardware (tests only).
    pub hardware_override: Option<PathBuf>,
    /// Where "new project" creates its folders (default: `~/Ancilo`).
    pub projects_dir: Option<PathBuf>,
    /// Read the computer's live state (free memory, pressure, heat) from
    /// this JSON file instead (tests only).
    pub system_probe_override: Option<PathBuf>,
    /// Extra environment for model processes (tuning flags, test fakes).
    pub llama_server_env: std::collections::BTreeMap<String, String>,
    /// Model names used by clients → model id or role (e.g. a Claude Code
    /// "haiku" model → a small local model). Unknown names use the default model.
    pub model_aliases: std::collections::BTreeMap<String, String>,
    /// `claude` executable (default: from PATH).
    pub claude_bin: Option<PathBuf>,
    /// `codex` executable (default: from PATH).
    pub codex_bin: Option<PathBuf>,
    /// The `ancilo` executable MCP clients start (default: this program).
    pub ancilo_bin: Option<PathBuf>,
    /// Codex home for its global AGENTS.md (default: `$CODEX_HOME` or `~/.codex`).
    pub codex_home: Option<PathBuf>,
    /// Wikipedia for the web search; `{lang}` becomes the language code.
    pub wikipedia_endpoint: String,
    /// Serper (Google results) for the web search.
    pub serper_endpoint: String,
    /// Host names the web search may reach at these addresses although they
    /// are not public (tests only – the one way past its address check).
    pub web_hosts: std::collections::BTreeMap<String, std::net::IpAddr>,
}

/// The model catalog in the Ancilo repository.
pub const DEFAULT_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/stefangrunert/ancilo/main/knowledge/catalog.json";

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            hf_endpoint: "https://huggingface.co".into(),
            github_endpoint: "https://github.com".into(),
            catalog_url: DEFAULT_CATALOG_URL.into(),
            llama_server_bin: None,
            llama_auto_install: None,
            ram_reserve_gib: None,
            model_search_dirs: None,
            hardware_override: None,
            system_probe_override: None,
            projects_dir: None,
            llama_server_env: Default::default(),
            model_aliases: Default::default(),
            claude_bin: None,
            codex_bin: None,
            ancilo_bin: None,
            codex_home: None,
            wikipedia_endpoint: "https://{lang}.wikipedia.org".into(),
            serper_endpoint: "https://google.serper.dev".into(),
            web_hosts: Default::default(),
        }
    }
}

impl Config {
    /// Loads `config.toml` (missing file = defaults), then applies environment
    /// overrides (`ANCILO_PORT`, `ANCILO_HF_ENDPOINT`, `ANCILO_LLAMA_SERVER`).
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        let mut config = match std::fs::read_to_string(&file) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| Error::invalid(format!("{}: {e}", file.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        if let Ok(port) = std::env::var("ANCILO_PORT") {
            config.port = port
                .parse()
                .map_err(|_| Error::invalid(format!("ANCILO_PORT is not a port: {port}")))?;
        }
        if let Ok(v) = std::env::var("ANCILO_HF_ENDPOINT") {
            config.hf_endpoint = v;
        }
        if let Ok(v) = std::env::var("ANCILO_CATALOG_URL") {
            config.catalog_url = v;
        }
        if let Ok(v) = std::env::var("ANCILO_LLAMA_SERVER") {
            config.llama_server_bin = Some(PathBuf::from(v));
        }
        Ok(config)
    }

    /// Bytes of RAM to keep free, given the machine's total RAM.
    pub fn ram_reserve_bytes(&self, total_ram: u64) -> u64 {
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        match self.ram_reserve_gib {
            Some(gib) => (gib.max(0.0) * GIB) as u64,
            None => (total_ram / 5).min(8 * 1024 * 1024 * 1024),
        }
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(Error::internal)?;
        std::fs::write(paths.config_file(), text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(dir.path());
        let c = Config::load(&paths).unwrap();
        assert_eq!(c.hf_endpoint, "https://huggingface.co");
    }

    #[test]
    fn roundtrips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(dir.path());
        let c = Config {
            port: 1234,
            ..Default::default()
        };
        c.save(&paths).unwrap();
        assert_eq!(Config::load(&paths).unwrap().port, 1234);
    }

    #[test]
    fn automatic_reserve_scales_with_ram() {
        let c = Config::default();
        let gib = 1024 * 1024 * 1024;
        assert_eq!(c.ram_reserve_bytes(128 * gib), 8 * gib);
        assert_eq!(c.ram_reserve_bytes(10 * gib), 2 * gib);
    }

    #[test]
    fn rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(dir.path());
        std::fs::write(paths.config_file(), "bogus = 1\n").unwrap();
        assert_eq!(Config::load(&paths).unwrap_err().code(), "invalid_input");
    }
}
