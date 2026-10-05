//! A small Hugging Face client: model info, file lists with SHA-256, file URLs.

use std::sync::Arc;
use std::time::Duration;

use ancilo_core::{Error, Result};
use ancilo_net::{By, Note, Purpose};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::planner::{ModelShape, RepoFile};

#[derive(Clone)]
pub struct HfClient {
    base: String,
    http: ancilo_net::Net,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RepoInfo {
    pub id: String,
    pub pipeline_tag: Option<String>,
    pub architecture: Option<String>,
    pub context_length: Option<u64>,
    pub gated: bool,
}

impl RepoInfo {
    pub fn shape(&self) -> ModelShape {
        ModelShape {
            context_length: self.context_length,
            ..Default::default()
        }
    }
}

pub fn user_agent() -> String {
    format!("ancilo/{}", ancilo_core::VERSION)
}

/// The user's Hugging Face token (`HF_TOKEN`), for gated and private models.
/// Only ever sent to the Hugging Face endpoint.
pub fn token() -> Option<String> {
    ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// A repository found by a search.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub repo: String,
    /// For `plan_model` / `add_model`.
    pub address: String,
    pub downloads: u64,
    pub likes: u64,
    /// `YYYY-MM-DD`
    pub updated: Option<String>,
    pub pipeline_tag: Option<String>,
}

impl HfClient {
    /// `log`: the user's log of what left this computer; `hosts`: names
    /// reached at these addresses (tests).
    pub fn new(
        endpoint: &str,
        log: Option<Arc<dyn ancilo_net::Recorder>>,
        hosts: &std::collections::BTreeMap<String, std::net::IpAddr>,
    ) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(t) = token()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {t}"))
        {
            let mut v = v;
            v.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, v);
        }
        let http = ancilo_net::with_hosts(reqwest::Client::builder(), hosts)
            .default_headers(headers)
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Self {
            base: endpoint.trim_end_matches('/').to_string(),
            http: ancilo_net::Net::new(http, log),
        }
    }

    async fn get_json(&self, url: &str, what: &str, note: Note) -> Result<Value> {
        let resp = self
            .http
            .send(self.http.get(url), note)
            .await
            .map_err(|e| Error::unavailable(ancilo_core::msg("hf.unreachable", &[("why", &e)])))?;
        match resp.status().as_u16() {
            200 => resp
                .json()
                .await
                .map_err(|e| Error::unavailable(format!("invalid answer from Hugging Face: {e}"))),
            401 | 403 => Err(Error::PermissionDenied(format!(
                "{what} is private or gated on Hugging Face – accept its license on huggingface.co and set HF_TOKEN{}",
                if token().is_some() {
                    " (the token in HF_TOKEN has no access)"
                } else {
                    ""
                }
            ))),
            404 => Err(Error::not_found(ancilo_core::msg(
                "hf.not_found",
                &[("what", &what)],
            ))),
            s => Err(Error::unavailable(format!(
                "Hugging Face answered HTTP {s} for {what}"
            ))),
        }
    }

    /// GGUF repositories matching `query`, most downloaded first.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}/api/models", self.base),
            &[
                ("search", query),
                ("filter", "gguf"),
                ("sort", "downloads"),
                ("direction", "-1"),
                ("limit", &limit.to_string()),
            ],
        )
        .map_err(|e| Error::invalid(format!("search: {e}")))?;
        let v = self
            .get_json(
                url.as_str(),
                "the search",
                Note::new(Purpose::ModelSearch, query, By::You),
            )
            .await?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let repo = m["id"].as_str().or(m["modelId"].as_str())?.to_string();
                Some(SearchHit {
                    address: format!("hf.co/{repo}"),
                    downloads: m["downloads"].as_u64().unwrap_or(0),
                    likes: m["likes"].as_u64().unwrap_or(0),
                    updated: m["lastModified"]
                        .as_str()
                        .or(m["createdAt"].as_str())
                        .map(|s| s.chars().take(10).collect()),
                    pipeline_tag: m["pipeline_tag"].as_str().map(String::from),
                    repo,
                })
            })
            .collect())
    }

    pub async fn model_info(&self, repo: &str) -> Result<RepoInfo> {
        let v = self
            .get_json(
                &format!("{}/api/models/{repo}", self.base),
                &format!("repository '{repo}'"),
                Note::new(Purpose::ModelInfo, repo, By::You),
            )
            .await?;
        Ok(RepoInfo {
            id: v["id"].as_str().unwrap_or(repo).to_string(),
            pipeline_tag: v["pipeline_tag"].as_str().map(str::to_string),
            architecture: v["gguf"]["architecture"].as_str().map(str::to_string),
            context_length: v["gguf"]["context_length"].as_u64(),
            gated: v["gated"].as_bool().unwrap_or(false) || v["gated"].is_string(),
        })
    }

    /// All files (recursively) with size and, for LFS files, SHA-256.
    pub async fn files(&self, repo: &str, revision: &str) -> Result<Vec<RepoFile>> {
        let v = self
            .get_json(
                &format!(
                    "{}/api/models/{repo}/tree/{revision}?recursive=1",
                    self.base
                ),
                &format!("repository '{repo}'"),
                Note::new(Purpose::ModelInfo, repo, By::You),
            )
            .await?;
        Ok(v.as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| e["type"] == "file")
                    .map(|e| RepoFile {
                        path: e["path"].as_str().unwrap_or_default().to_string(),
                        size: e["lfs"]["size"]
                            .as_u64()
                            .or(e["size"].as_u64())
                            .unwrap_or(0),
                        sha256: e["lfs"]["oid"].as_str().map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A small text file of a repository (e.g. the model card `README.md`);
    /// `None` if it does not exist.
    pub async fn text_file(
        &self,
        repo: &str,
        revision: &str,
        path: &str,
    ) -> Result<Option<String>> {
        // Fetched by Ancilo itself when a model is added (its description).
        let resp = self
            .http
            .send(
                self.http.get(self.file_url(repo, revision, path)),
                Note::new(Purpose::ModelCard, format!("{repo}/{path}"), By::Ancilo),
            )
            .await
            .map_err(|e| Error::unavailable(ancilo_core::msg("hf.unreachable", &[("why", &e)])))?;
        match resp.status().as_u16() {
            200 => Ok(Some(resp.text().await.map_err(|e| {
                Error::unavailable(format!("invalid answer from Hugging Face: {e}"))
            })?)),
            404 => Ok(None),
            s => Err(Error::unavailable(format!(
                "Hugging Face answered HTTP {s} for {repo}/{path}"
            ))),
        }
    }

    pub fn file_url(&self, repo: &str, revision: &str, path: &str) -> String {
        format!("{}/{repo}/resolve/{revision}/{path}", self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ancilo_testkit::{FakeFile, FakeHf, FakeRepo};
    use serde_json::json;

    #[tokio::test]
    async fn reads_info_and_files() {
        let hf = FakeHf::start(vec![
            FakeRepo::new(
                "org/M-GGUF",
                vec![
                    FakeFile::gguf("M-Q4_K_M.gguf", "qwen3", 40960, 1000),
                    FakeFile::new("README.md", vec![1]),
                ],
            )
            .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 40960})),
        ])
        .await;
        let c = HfClient::new(&hf.url(), None, &Default::default());
        let info = c.model_info("org/M-GGUF").await.unwrap();
        assert_eq!(info.context_length, Some(40960));
        let files = c.files("org/M-GGUF", "main").await.unwrap();
        let gguf = files.iter().find(|f| f.path == "M-Q4_K_M.gguf").unwrap();
        assert_eq!(gguf.size, 1000);
        assert_eq!(gguf.sha256.as_ref().unwrap().len(), 64);
        assert_eq!(c.model_info("no/pe").await.unwrap_err().code(), "not_found");
    }

    #[tokio::test]
    async fn unreachable_endpoint_is_unavailable() {
        let c = HfClient::new("http://127.0.0.1:9", None, &Default::default());
        assert_eq!(c.model_info("a/b").await.unwrap_err().code(), "unavailable");
    }
}
