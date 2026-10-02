//! Finds GGUF files that are already on this machine (LM Studio, Ollama,
//! Hugging Face cache), so Ancilo never downloads a model twice.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FoundIn {
    LmStudio,
    Ollama,
    HfCache,
    Folder,
}

impl FoundIn {
    pub fn label(self) -> &'static str {
        match self {
            FoundIn::LmStudio => "LM Studio",
            FoundIn::Ollama => "Ollama",
            FoundIn::HfCache => "Hugging Face cache",
            FoundIn::Folder => "folder",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FoundFile {
    pub path: PathBuf,
    /// File name as published (for Ollama blobs: the model name).
    pub name: String,
    pub size: u64,
    pub found_in: FoundIn,
    /// `org/repo` if the layout tells us.
    pub repo: Option<String>,
    /// Known without hashing (Ollama stores blobs by SHA-256).
    pub sha256: Option<String>,
}

/// Default locations of other tools' model stores.
pub fn default_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut dirs = vec![
        home.join(".lmstudio/models"),
        home.join(".cache/lm-studio/models"),
        home.join(".ollama/models"),
        home.join(".cache/huggingface/hub"),
    ];
    if let Ok(hf) = std::env::var("HF_HUB_CACHE") {
        dirs.push(PathBuf::from(hf));
    }
    dirs
}

fn walk_gguf(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // Follow symlinks (HF cache snapshots are symlinks into blobs).
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() => walk_gguf(&p, depth + 1, out),
            Ok(m) if m.is_file() && name.to_ascii_lowercase().ends_with(".gguf") => out.push(p),
            _ => {}
        }
    }
}

fn size_of(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn scan_ollama(root: &Path, out: &mut Vec<FoundFile>) {
    let manifests = root.join("manifests");
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    walk(&manifests, &mut files);
    for manifest in files {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(layer) = v["layers"].as_array().and_then(|l| {
            l.iter()
                .find(|x| x["mediaType"] == "application/vnd.ollama.image.model")
        }) else {
            continue;
        };
        let Some(digest) = layer["digest"]
            .as_str()
            .and_then(|d| d.strip_prefix("sha256:"))
        else {
            continue;
        };
        let blob = root.join("blobs").join(format!("sha256-{digest}"));
        if !blob.is_file() {
            continue;
        }
        // manifests/<registry>/<namespace…>/<model>/<tag>
        let rel: Vec<String> = manifest
            .strip_prefix(&manifests)
            .unwrap_or(&manifest)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let (name, repo) = match rel.as_slice() {
            [registry, rest @ .., tag] if registry == "hf.co" && rest.len() == 2 => {
                (format!("{}:{tag}", rest.join("/")), Some(rest.join("/")))
            }
            [_, rest @ .., tag] => (
                format!("{}:{tag}", rest.join("/").trim_start_matches("library/")),
                None,
            ),
            _ => continue,
        };
        out.push(FoundFile {
            size: size_of(&blob),
            path: blob,
            name,
            found_in: FoundIn::Ollama,
            repo,
            sha256: Some(digest.to_string()),
        });
    }
}

fn scan_hf_cache(root: &Path, out: &mut Vec<FoundFile>) {
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("models--") else {
            continue;
        };
        let repo = rest.replacen("--", "/", 1);
        let snapshots = e.path().join("snapshots");
        let mut files = Vec::new();
        walk_gguf(&snapshots, 0, &mut files);
        for f in files {
            let rel = f
                .strip_prefix(&snapshots)
                .ok()
                .and_then(|r| {
                    r.components()
                        .skip(1)
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .reduce(|a, b| format!("{a}/{b}"))
                })
                .unwrap_or_default();
            out.push(FoundFile {
                size: size_of(&f),
                path: f,
                name: rel,
                found_in: FoundIn::HfCache,
                repo: Some(repo.clone()),
                sha256: None,
            });
        }
    }
}

fn scan_folder(root: &Path, found_in: FoundIn, out: &mut Vec<FoundFile>) {
    let mut files = Vec::new();
    walk_gguf(root, 0, &mut files);
    for f in files {
        let rel: Vec<String> = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        // LM Studio: <publisher>/<repo>/<file>
        let repo = (rel.len() >= 3).then(|| format!("{}/{}", rel[0], rel[1]));
        out.push(FoundFile {
            size: size_of(&f),
            name: rel.last().cloned().unwrap_or_default(),
            path: f,
            found_in,
            repo,
            sha256: None,
        });
    }
}

/// Scans the given directories; the layout of each is detected automatically.
pub fn scan(dirs: &[PathBuf]) -> Vec<FoundFile> {
    let mut out = Vec::new();
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        if dir.join("manifests").is_dir() && dir.join("blobs").is_dir() {
            scan_ollama(dir, &mut out);
        } else if std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("models--"))
        {
            scan_hf_cache(dir, &mut out);
        } else {
            let kind = if dir.to_string_lossy().contains("lmstudio")
                || dir.to_string_lossy().contains("lm-studio")
            {
                FoundIn::LmStudio
            } else {
                FoundIn::Folder
            };
            scan_folder(dir, kind, &mut out);
        }
    }
    out
}

/// A local copy of `name` with exactly `size` bytes (and the same SHA-256 if
/// both are known). Name matching ignores directories.
pub fn find<'a>(
    found: &'a [FoundFile],
    name: &str,
    size: u64,
    sha256: Option<&str>,
) -> Option<&'a FoundFile> {
    let base = name.rsplit('/').next().unwrap_or(name);
    found.iter().find(|f| {
        if let (Some(a), Some(b)) = (sha256, f.sha256.as_deref()) {
            return a.eq_ignore_ascii_case(b);
        }
        f.size == size
            && (f.name == name
                || f.name.rsplit('/').next() == Some(base)
                || f.path.file_name().is_some_and(|n| n == base))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn finds_files_in_all_three_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let lm = dir.path().join("lmstudio");
        write(&lm.join("unsloth/Q-GGUF/Q-Q8_0.gguf"), &[1; 100]);
        let hf = dir.path().join("hub");
        write(&hf.join("models--org--M-GGUF/blobs/abc"), &[2; 50]);
        std::fs::create_dir_all(hf.join("models--org--M-GGUF/snapshots/rev1")).unwrap();
        std::os::unix::fs::symlink(
            hf.join("models--org--M-GGUF/blobs/abc"),
            hf.join("models--org--M-GGUF/snapshots/rev1/M-Q4_K_M.gguf"),
        )
        .unwrap();
        let ol = dir.path().join("ollama");
        let digest = "d".repeat(64);
        write(&ol.join(format!("blobs/sha256-{digest}")), &[3; 70]);
        write(
            &ol.join("manifests/hf.co/org/X-GGUF/Q4_K_M"),
            format!(r#"{{"layers":[{{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:{digest}"}}]}}"#).as_bytes(),
        );
        write(
            &ol.join("manifests/registry.ollama.ai/library/llama3.2/latest"),
            format!(r#"{{"layers":[{{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:{digest}"}}]}}"#).as_bytes(),
        );

        let found = scan(&[lm, hf, ol, dir.path().join("missing")]);
        let lm = found
            .iter()
            .find(|f| f.found_in == FoundIn::LmStudio)
            .unwrap();
        assert_eq!(lm.repo.as_deref(), Some("unsloth/Q-GGUF"));
        assert_eq!(lm.name, "Q-Q8_0.gguf");
        let hf = found
            .iter()
            .find(|f| f.found_in == FoundIn::HfCache)
            .unwrap();
        assert_eq!(
            (hf.repo.as_deref(), hf.name.as_str(), hf.size),
            (Some("org/M-GGUF"), "M-Q4_K_M.gguf", 50)
        );
        let ol: Vec<_> = found
            .iter()
            .filter(|f| f.found_in == FoundIn::Ollama)
            .collect();
        assert_eq!(ol.len(), 2);
        assert!(ol.iter().any(|f| f.repo.as_deref() == Some("org/X-GGUF")));
        assert!(ol.iter().any(|f| f.name == "llama3.2:latest"));

        assert!(find(&found, "Q-Q8_0.gguf", 100, None).is_some());
        assert!(find(&found, "Q-Q8_0.gguf", 99, None).is_none());
        assert!(find(&found, "anything", 0, Some(&digest)).is_some());
    }
}
