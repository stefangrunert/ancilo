//! Understands whatever the user types as a "model address".
//!
//! Pure function, no I/O: classification only. Existence is checked later.

use std::path::PathBuf;

use ancilo_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Address {
    /// A Hugging Face repository, optionally a specific file or quantization.
    HuggingFace {
        repo: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        /// Quantization hint, e.g. `Q4_K_M` from `repo:Q4_K_M`.
        #[serde(skip_serializing_if = "Option::is_none")]
        quant: Option<String>,
        revision: String,
    },
    /// A GGUF file on this machine.
    LocalFile { path: PathBuf },
    /// A running OpenAI-compatible server (Ollama, LM Studio, llama-server).
    ServerUrl { url: String },
}

fn valid_repo_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !s.starts_with('.')
}

fn expand_home(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(s)
}

/// Parses `org/repo[/path/to/file.gguf][:QUANT]` (after the host prefix).
fn parse_repo_path(rest: &str, input: &str) -> Result<Address> {
    let rest = rest.trim_matches('/');
    let (rest, quant) = match rest.rsplit_once(':') {
        Some((r, q)) if !q.is_empty() && !q.contains('/') => (r, Some(q.to_string())),
        _ => (rest, None),
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 2 || !valid_repo_part(parts[0]) || !valid_repo_part(parts[1]) {
        return Err(Error::invalid(format!(
            "'{input}' is not a Hugging Face repository (expected org/repo)"
        )));
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    let mut revision = "main".to_string();
    let mut file_parts: &[&str] = &parts[2..];
    // Web URLs: /blob/<rev>/<file>, /resolve/<rev>/<file>, /tree/<rev>
    if let Some(kind) = file_parts.first()
        && matches!(*kind, "blob" | "resolve" | "tree")
    {
        if let Some(rev) = file_parts.get(1) {
            revision = rev.to_string();
        }
        file_parts = if file_parts.len() > 2 {
            &file_parts[2..]
        } else {
            &[]
        };
    }
    let file = (!file_parts.is_empty()).then(|| file_parts.join("/"));
    if let Some(f) = &file
        && !f.to_ascii_lowercase().ends_with(".gguf")
    {
        return Err(Error::invalid(format!(
            "'{f}' is not a GGUF file – Ancilo runs GGUF models"
        )));
    }
    Ok(Address::HuggingFace {
        repo,
        file,
        quant: quant.map(|q| q.to_ascii_uppercase()),
        revision,
    })
}

/// Classifies user input as a model address.
pub fn resolve(input: &str) -> Result<Address> {
    let s = input.trim();
    if s.is_empty() {
        return Err(Error::invalid("please enter a model address"));
    }
    let lower = s.to_ascii_lowercase();

    // Local files.
    let looks_local =
        s.starts_with('/') || s.starts_with("~/") || s.starts_with("./") || s.starts_with("../");
    if looks_local
        || (lower.ends_with(".gguf") && !s.contains("://") && s.matches('/').count() == 0)
    {
        if !lower.ends_with(".gguf") {
            return Err(Error::invalid(format!(
                "'{s}' is not a GGUF file – Ancilo runs GGUF models"
            )));
        }
        return Ok(Address::LocalFile {
            path: expand_home(s),
        });
    }
    if let Some(rest) = lower.strip_prefix("file://") {
        let _ = rest;
        return resolve(&s["file://".len()..]);
    }

    // Hugging Face in its various spellings.
    for prefix in [
        "https://huggingface.co/",
        "http://huggingface.co/",
        "https://www.huggingface.co/",
        "huggingface.co/",
        "https://hf.co/",
        "hf.co/",
        "hf://",
    ] {
        if lower.starts_with(prefix) {
            return parse_repo_path(&s[prefix.len()..], s);
        }
    }

    // Other URLs and host:port are servers.
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(Address::ServerUrl {
            url: s.trim_end_matches('/').to_string(),
        });
    }
    if let Some((host, port)) = s.split_once(':')
        && !host.contains('/')
        && port.chars().all(|c| c.is_ascii_digit())
        && !port.is_empty()
    {
        return Ok(Address::ServerUrl {
            url: format!("http://{host}:{port}"),
        });
    }

    // Bare org/repo[...].
    if s.contains('/') {
        return parse_repo_path(s, s);
    }
    Err(Error::invalid(format!(
        "'{s}' is not a model address. Examples: hf.co/unsloth/Qwen3.6-35B-A3B-GGUF, ~/models/model.gguf, http://localhost:11434"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hf(repo: &str, file: Option<&str>, quant: Option<&str>, rev: &str) -> Address {
        Address::HuggingFace {
            repo: repo.into(),
            file: file.map(Into::into),
            quant: quant.map(Into::into),
            revision: rev.into(),
        }
    }

    // covers: M1-AC-01
    #[test]
    fn recognises_every_common_spelling() {
        let q = "unsloth/Qwen3.6-35B-A3B-GGUF";
        let cases: Vec<(&str, Address)> = vec![
            (
                "hf.co/unsloth/Qwen3.6-35B-A3B-GGUF",
                hf(q, None, None, "main"),
            ),
            (
                "huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF",
                hf(q, None, None, "main"),
            ),
            (
                "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF",
                hf(q, None, None, "main"),
            ),
            (
                "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/",
                hf(q, None, None, "main"),
            ),
            (
                "https://hf.co/unsloth/Qwen3.6-35B-A3B-GGUF",
                hf(q, None, None, "main"),
            ),
            (
                "hf://unsloth/Qwen3.6-35B-A3B-GGUF",
                hf(q, None, None, "main"),
            ),
            ("unsloth/Qwen3.6-35B-A3B-GGUF", hf(q, None, None, "main")),
            (
                "  unsloth/Qwen3.6-35B-A3B-GGUF  ",
                hf(q, None, None, "main"),
            ),
            (
                "hf.co/unsloth/Qwen3.6-35B-A3B-GGUF:Q4_K_M",
                hf(q, None, Some("Q4_K_M"), "main"),
            ),
            (
                "hf.co/unsloth/Qwen3.6-35B-A3B-GGUF:q8_0",
                hf(q, None, Some("Q8_0"), "main"),
            ),
            (
                "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/blob/main/Qwen3.6-35B-A3B-Q8_0.gguf",
                hf(q, Some("Qwen3.6-35B-A3B-Q8_0.gguf"), None, "main"),
            ),
            (
                "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/resolve/abc123/Qwen3.6-35B-A3B-Q8_0.gguf",
                hf(q, Some("Qwen3.6-35B-A3B-Q8_0.gguf"), None, "abc123"),
            ),
            (
                "https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/tree/main",
                hf(q, None, None, "main"),
            ),
            (
                "unsloth/Qwen3.6-35B-A3B-GGUF/Q8_0/part-00001-of-00002.gguf",
                hf(q, Some("Q8_0/part-00001-of-00002.gguf"), None, "main"),
            ),
            (
                "/models/qwen.gguf",
                Address::LocalFile {
                    path: "/models/qwen.gguf".into(),
                },
            ),
            (
                "./qwen.gguf",
                Address::LocalFile {
                    path: "./qwen.gguf".into(),
                },
            ),
            (
                "qwen.gguf",
                Address::LocalFile {
                    path: "qwen.gguf".into(),
                },
            ),
            (
                "file:///models/q.gguf",
                Address::LocalFile {
                    path: "/models/q.gguf".into(),
                },
            ),
            (
                "http://localhost:11434",
                Address::ServerUrl {
                    url: "http://localhost:11434".into(),
                },
            ),
            (
                "http://127.0.0.1:1234/",
                Address::ServerUrl {
                    url: "http://127.0.0.1:1234".into(),
                },
            ),
            (
                "localhost:8080",
                Address::ServerUrl {
                    url: "http://localhost:8080".into(),
                },
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(resolve(input).unwrap(), expected, "input: {input}");
        }
        let home = resolve("~/models/x.gguf").unwrap();
        assert!(matches!(home, Address::LocalFile { path } if !path.starts_with("~")));
    }

    // covers: M1-AC-01
    #[test]
    fn rejects_nonsense_with_helpful_messages() {
        for bad in [
            "",
            "   ",
            "qwen",
            "/models/readme.md",
            "hf.co/onlyorg",
            "hf.co/org/repo/blob/main/config.json",
            "hf.co/../etc/passwd",
            "org /repo",
        ] {
            let err = resolve(bad).unwrap_err();
            assert_eq!(err.code(), "invalid_input", "input: {bad:?}");
            assert!(!err.message().is_empty());
        }
    }
}
