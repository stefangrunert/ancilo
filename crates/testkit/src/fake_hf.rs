//! A Hugging Face stand-in: repository metadata, file trees with SHA-256, and
//! downloads with `Range` support – plus fault injection (dropped connections,
//! corrupted content, 404, slow transfer).
//!
//! It also serves arbitrary "raw" files, which covers other download sources
//! such as llama.cpp release archives.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A file in a fake repository.
#[derive(Clone)]
pub struct FakeFile {
    pub path: String,
    pub content: Arc<Vec<u8>>,
    /// The first `n` download attempts break off after this many bytes.
    pub fail_after_bytes: Option<u64>,
    pub fail_times: u32,
    /// Serve content that does not match the advertised SHA-256.
    pub corrupt: bool,
    /// Pause between 64 KiB chunks (for progress tests).
    pub chunk_delay: Option<Duration>,
}

impl FakeFile {
    pub fn new(path: &str, content: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            content: Arc::new(content),
            fail_after_bytes: None,
            fail_times: 0,
            corrupt: false,
            chunk_delay: None,
        }
    }

    /// A small but valid GGUF file padded to `size` bytes.
    pub fn gguf(path: &str, arch: &str, context_length: u64, size: usize) -> Self {
        let name = path.trim_end_matches(".gguf");
        Self::new(
            path,
            crate::gguf::fake_gguf(arch, name, context_length, size),
        )
    }

    pub fn failing_once_after(mut self, bytes: u64) -> Self {
        self.fail_after_bytes = Some(bytes);
        self.fail_times = 1;
        self
    }

    pub fn corrupt(mut self) -> Self {
        self.corrupt = true;
        self
    }

    pub fn slow(mut self, per_chunk: Duration) -> Self {
        self.chunk_delay = Some(per_chunk);
        self
    }

    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.content.as_slice()))
    }
}

/// A fake model repository, e.g. `unsloth/Qwen3-Test-GGUF`.
#[derive(Clone)]
pub struct FakeRepo {
    pub id: String,
    pub files: Vec<FakeFile>,
    /// Value of the `gguf` field in the model info (architecture, context, …).
    pub gguf: Option<Value>,
    pub pipeline_tag: Option<String>,
}

impl FakeRepo {
    pub fn new(id: &str, files: Vec<FakeFile>) -> Self {
        Self {
            id: id.into(),
            files,
            gguf: None,
            pipeline_tag: Some("text-generation".into()),
        }
    }

    pub fn with_gguf_meta(mut self, meta: Value) -> Self {
        self.gguf = Some(meta);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HfRequest {
    pub method: String,
    pub path: String,
    pub range: Option<String>,
}

struct Inner {
    repos: HashMap<String, FakeRepo>,
    /// Repository ids in the order given (search results).
    order: Vec<String>,
    raw: HashMap<String, FakeFile>,
    failures_served: HashMap<String, u32>,
    requests: Vec<HfRequest>,
}

type Shared = Arc<Mutex<Inner>>;

pub struct FakeHf {
    pub addr: SocketAddr,
    state: Shared,
    task: tokio::task::JoinHandle<()>,
}

impl FakeHf {
    pub async fn start(repos: Vec<FakeRepo>) -> Self {
        Self::start_with_raw(repos, Vec::new()).await
    }

    /// `raw` files are served at their path, e.g.
    /// `ggml-org/llama.cpp/releases/download/b1/llama.tar.gz`.
    pub async fn start_with_raw(repos: Vec<FakeRepo>, raw: Vec<FakeFile>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state: Shared = Arc::new(Mutex::new(Inner {
            order: repos.iter().map(|r| r.id.clone()).collect(),
            repos: repos.into_iter().map(|r| (r.id.clone(), r)).collect(),
            raw: raw.into_iter().map(|f| (f.path.clone(), f)).collect(),
            failures_served: HashMap::new(),
            requests: Vec::new(),
        }));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        Self { addr, state, task }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<HfRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    /// Number of requests that downloaded (part of) `path`.
    pub fn downloads_of(&self, path: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.method == "GET" && r.path.ends_with(path) && r.path.contains("resolve"))
            .count()
    }
}

impl Drop for FakeHf {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(s): State<Shared>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().trim_start_matches('/').to_string();
    let range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    s.lock().unwrap().requests.push(HfRequest {
        method: method.to_string(),
        path: path.clone(),
        range: range.clone(),
    });

    if path == "api/models" {
        return search(&s, req.uri().query().unwrap_or(""));
    }
    if let Some(rest) = path.strip_prefix("api/models/") {
        return api_models(&s, rest);
    }
    // <org>/<repo>/resolve/<rev>/<file>
    let parts: Vec<&str> = path.splitn(5, '/').collect();
    if parts.len() == 5 && parts[2] == "resolve" {
        let repo_id = format!("{}/{}", parts[0], parts[1]);
        let file = {
            let st = s.lock().unwrap();
            st.repos
                .get(&repo_id)
                .and_then(|r| r.files.iter().find(|f| f.path == parts[4]).cloned())
        };
        return match file {
            Some(f) => serve_file(&s, &method, req.headers(), f),
            None => not_found("Entry not found"),
        };
    }
    let raw = s.lock().unwrap().raw.get(&path).cloned();
    match raw {
        Some(f) => serve_file(&s, &method, req.headers(), f),
        None => not_found("not found"),
    }
}

fn not_found(msg: &str) -> Response {
    (StatusCode::NOT_FOUND, axum::Json(json!({"error": msg}))).into_response()
}

/// `GET /api/models?search=…&limit=…`: repositories whose id contains every
/// word, in the order they were added (stands in for "most downloaded").
fn search(s: &Shared, query: &str) -> Response {
    let params: std::collections::HashMap<String, String> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.replace('+', " ").replace("%20", " ")))
        .collect();
    let words: Vec<String> = params
        .get("search")
        .map(|q| {
            q.to_lowercase()
                .split_whitespace()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20);
    let st = s.lock().unwrap();
    let hits: Vec<Value> = st
        .order
        .iter()
        .filter_map(|id| st.repos.get(id))
        .filter(|r| {
            let id = r.id.to_lowercase();
            words.iter().all(|w| id.contains(w.as_str()))
        })
        .take(limit)
        .enumerate()
        .map(|(i, r)| {
            json!({"id": r.id, "modelId": r.id, "downloads": 1000 - i as u64, "likes": 10,
                   "pipeline_tag": r.pipeline_tag, "lastModified": "2026-09-01T00:00:00.000Z", "tags": ["gguf"]})
        })
        .collect();
    axum::Json(hits).into_response()
}

fn api_models(s: &Shared, rest: &str) -> Response {
    let st = s.lock().unwrap();
    let (repo_id, tree) = match rest.find("/tree/") {
        Some(i) => (&rest[..i], true),
        None => (rest, false),
    };
    let Some(repo) = st.repos.get(repo_id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "Repository not found"})),
        )
            .into_response();
    };
    if tree {
        let entries: Vec<Value> = repo
            .files
            .iter()
            .map(|f| {
                let size = f.content.len();
                if f.path.ends_with(".gguf") {
                    json!({"type": "file", "oid": "0", "size": size, "path": f.path,
                           "lfs": {"oid": f.sha256(), "size": size, "pointerSize": 134}})
                } else {
                    json!({"type": "file", "oid": "0", "size": size, "path": f.path})
                }
            })
            .collect();
        return axum::Json(entries).into_response();
    }
    let siblings: Vec<Value> = repo
        .files
        .iter()
        .map(|f| json!({"rfilename": f.path}))
        .collect();
    let mut info = json!({
        "id": repo.id,
        "modelId": repo.id,
        "author": repo.id.split('/').next(),
        "pipeline_tag": repo.pipeline_tag,
        "tags": ["gguf"],
        "siblings": siblings,
    });
    if let Some(g) = &repo.gguf {
        info["gguf"] = g.clone();
    }
    axum::Json(info).into_response()
}

fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let start: u64 = a.parse().ok()?;
    let end: u64 = if b.is_empty() {
        len.saturating_sub(1)
    } else {
        b.parse::<u64>().ok()?.min(len.saturating_sub(1))
    };
    (start <= end && start < len).then_some((start, end))
}

fn serve_file(s: &Shared, method: &Method, headers: &HeaderMap, f: FakeFile) -> Response {
    let mut content: Vec<u8> = f.content.as_ref().clone();
    if f.corrupt && content.len() > 16 {
        let i = content.len() / 2;
        content[i] ^= 0xff;
    }
    let len = content.len() as u64;
    let sha = f.sha256();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| parse_range(h, len));
    let (start, end, status) = match range {
        Some((a, b)) => (a, b, StatusCode::PARTIAL_CONTENT),
        None => (0, len.saturating_sub(1), StatusCode::OK),
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::ACCEPT_RANGES, "bytes")
        .header("x-linked-size", len.to_string())
        .header("x-linked-etag", format!("\"{sha}\""))
        .header(header::CONTENT_LENGTH, (end + 1 - start).to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    if *method == Method::HEAD {
        return builder.body(Body::empty()).unwrap();
    }
    let slice = content[start as usize..=end as usize].to_vec();

    // Fault: break off after n bytes (counted from the start of this response).
    let break_after = {
        let mut st = s.lock().unwrap();
        let served = st.failures_served.entry(f.path.clone()).or_default();
        match f.fail_after_bytes {
            Some(n) if *served < f.fail_times => {
                *served += 1;
                Some(n as usize)
            }
            _ => None,
        }
    };
    let delay = f.chunk_delay;
    let stream = async_stream(slice, break_after, delay);
    builder.body(Body::from_stream(stream)).unwrap()
}

fn async_stream(
    data: Vec<u8>,
    break_after: Option<usize>,
    delay: Option<Duration>,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    const CHUNK: usize = 64 * 1024;
    let limit = break_after.unwrap_or(usize::MAX);
    futures::stream::unfold((0usize, data), move |(pos, data)| async move {
        if pos >= data.len() {
            return None;
        }
        if pos >= limit {
            // Give the client time to consume what was sent: a TCP reset
            // discards unread data, which would hide the partial transfer.
            tokio::time::sleep(Duration::from_millis(300)).await;
            return Some((
                Err(std::io::Error::other("connection reset")),
                (usize::MAX, data),
            ));
        }
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        let end = (pos + CHUNK).min(data.len()).min(limit.max(pos + 1));
        let chunk = bytes::Bytes::copy_from_slice(&data[pos..end]);
        Some((Ok(chunk), (end, data)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> FakeRepo {
        FakeRepo::new(
            "org/Test-GGUF",
            vec![
                FakeFile::gguf("Test-Q4_K_M.gguf", "qwen3", 32768, 300_000),
                FakeFile::new("README.md", b"# test".to_vec()),
            ],
        )
        .with_gguf_meta(json!({"architecture": "qwen3", "context_length": 32768}))
    }

    // covers: M0-AC-04
    #[tokio::test]
    async fn serves_model_info_and_tree_with_sha256() {
        let hf = FakeHf::start(vec![repo()]).await;
        let info: Value = reqwest::get(format!("{}/api/models/org/Test-GGUF", hf.url()))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(info["gguf"]["architecture"], "qwen3");
        assert_eq!(info["siblings"].as_array().unwrap().len(), 2);
        let tree: Value = reqwest::get(format!(
            "{}/api/models/org/Test-GGUF/tree/main?recursive=1",
            hf.url()
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let gguf = tree
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["path"] == "Test-Q4_K_M.gguf")
            .unwrap();
        assert_eq!(gguf["size"], 300_000);
        assert_eq!(gguf["lfs"]["oid"].as_str().unwrap().len(), 64);
    }

    // covers: M0-AC-04
    #[tokio::test]
    async fn supports_range_requests() {
        let hf = FakeHf::start(vec![repo()]).await;
        let url = format!("{}/org/Test-GGUF/resolve/main/Test-Q4_K_M.gguf", hf.url());
        let r = reqwest::Client::new()
            .get(&url)
            .header("Range", "bytes=0-3")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 206);
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"GGUF");
    }

    // covers: M0-AC-04
    #[tokio::test]
    async fn injects_faults() {
        let files = vec![
            FakeFile::gguf("a.gguf", "llama", 4096, 200_000).failing_once_after(1000),
            FakeFile::gguf("b.gguf", "llama", 4096, 50_000).corrupt(),
        ];
        let expected_b = files[1].sha256();
        let hf = FakeHf::start(vec![FakeRepo::new("o/r", files)]).await;
        let a = format!("{}/o/r/resolve/main/a.gguf", hf.url());
        let first_failed = match reqwest::get(&a).await {
            Err(_) => true,
            Ok(r) => r.bytes().await.is_err(),
        };
        assert!(first_failed, "first download must break off");
        let second = reqwest::get(&a).await.unwrap().bytes().await.unwrap();
        assert_eq!(second.len(), 200_000);
        let b = reqwest::get(format!("{}/o/r/resolve/main/b.gguf", hf.url()))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_ne!(hex::encode(Sha256::digest(&b)), expected_b);
        let missing = reqwest::get(format!("{}/o/r/resolve/main/nope.gguf", hf.url()))
            .await
            .unwrap();
        assert_eq!(missing.status().as_u16(), 404);
        let no_repo = reqwest::get(format!("{}/api/models/x/y", hf.url()))
            .await
            .unwrap();
        assert_eq!(no_repo.status().as_u16(), 404);
    }

    #[tokio::test]
    async fn serves_raw_files() {
        let hf = FakeHf::start_with_raw(vec![], vec![FakeFile::new("a/b/c.tar.gz", vec![1, 2, 3])])
            .await;
        let b = reqwest::get(format!("{}/a/b/c.tar.gz", hf.url()))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(b.as_ref(), &[1, 2, 3]);
    }
}
