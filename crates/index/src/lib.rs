//! Knowledge for models (M5): a code index per project and a knowledge base
//! about Ancilo and models – both searched with hybrid search (vectors +
//! full text, merged by Reciprocal Rank Fusion).
//!
//! - Everything lives in SQLite (one file per index); vectors are BLOBs,
//!   searched exactly in Rust (see decision `2026-09-30-m5-umsetzung`).
//! - A project index is brought up to date on access: only files whose size
//!   or modification time changed are read, and only changed content is
//!   chunked and embedded again.
//! - Without an embedding model the search is full text only – nothing breaks.

pub mod chunk;
pub mod eval;
pub mod ops;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};

use ancilo_core::{BoxFuture, Error, EventBus, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::chunk::{Lang, chunk_file, split_identifier};
use crate::store::{FileState, Store};

/// Files larger than this are not indexed (generated code, data, bundles).
pub const MAX_FILE_BYTES: u64 = 1_000_000;
const EMBED_BATCH: usize = 32;
const RRF_K: f64 = 60.0;
const CANDIDATES: usize = 50;

/// Computes embeddings (normally the model with role `embed`).
pub trait Embedder: Send + Sync {
    /// The embedding model, or `None` when there is none.
    fn model(&self) -> Option<String>;
    fn embed<'a>(&'a self, texts: Vec<String>) -> BoxFuture<'a, Result<Vec<Vec<f32>>>>;
}

/// Embeddings through the gateway (role `embed`).
pub struct GatewayEmbedder(pub ancilo_gateway::Gateway);

impl GatewayEmbedder {
    /// Texts longer than the embedding model's input window are rejected by
    /// llama.cpp ("input … is too large"). Then each text is embedded on its
    /// own and shortened step by step until it fits – whatever the model's
    /// window and tokenizer are.
    async fn embed_fitting(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        match self.embed_raw(texts.clone()).await {
            Err(e) if e.message().contains("too large") => {
                let mut out = Vec::with_capacity(texts.len());
                for text in texts {
                    let mut t = text;
                    let mut attempt = 0;
                    loop {
                        match self.embed_raw(vec![t.clone()]).await {
                            Ok(mut v) if !v.is_empty() => {
                                out.push(v.remove(0));
                                break;
                            }
                            Err(e) if e.message().contains("too large") && attempt < 6 => {
                                let keep = t.chars().count() * 3 / 4;
                                t = t.chars().take(keep).collect();
                                attempt += 1;
                            }
                            Ok(_) => return Err(Error::unavailable("empty embeddings answer")),
                            Err(e) => return Err(e),
                        }
                    }
                }
                Ok(out)
            }
            other => other,
        }
    }

    async fn embed_raw(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        {
            let model = self
                .model()
                .ok_or_else(|| Error::unavailable("no embedding model"))?;
            let resp = self
                .0
                .embeddings(json!({"model": model, "input": texts}))
                .await?;
            let data = resp["data"]
                .as_array()
                .ok_or_else(|| Error::unavailable("invalid embeddings answer"))?;
            let mut out: Vec<(usize, Vec<f32>)> = data
                .iter()
                .map(|d| {
                    let v: Vec<f32> = d["embedding"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_f64())
                                .map(|x| x as f32)
                                .collect()
                        })
                        .unwrap_or_default();
                    (d["index"].as_u64().unwrap_or(0) as usize, normalize(v))
                })
                .collect();
            out.sort_by_key(|(i, _)| *i);
            Ok(out.into_iter().map(|(_, v)| v).collect())
        }
    }
}

impl Embedder for GatewayEmbedder {
    fn model(&self) -> Option<String> {
        self.0.manager().embedding_model()
    }

    fn embed<'a>(&'a self, texts: Vec<String>) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
        Box::pin(self.embed_fitting(texts))
    }
}

pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A text for the knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Document {
    /// Unique and stable, e.g. `docs/models.md` or `hf:unsloth/Qwen3-4B-GGUF`.
    pub id: String,
    /// Markdown.
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RefreshStats {
    pub scanned: u64,
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub unchanged: u64,
    pub embedded: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexStatus {
    /// Project root, or `knowledge`.
    pub name: String,
    pub files: u64,
    pub chunks: u64,
    pub embedded: u64,
    /// `hybrid` (vectors + full text) or `text` (no embedding model).
    pub mode: String,
    pub embed_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<RefreshStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Hit {
    /// File path relative to the project root, or document id.
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub kind: String,
    pub score: f64,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Chunk ids with their normalized embeddings, kept in memory per index.
type Vectors = Arc<Vec<(i64, Vec<f32>)>>;

/// One index (a project or the knowledge base).
pub struct Index {
    name: String,
    root: Option<PathBuf>,
    store: Store,
    vectors: Mutex<Option<Vectors>>,
    refreshing: tokio::sync::Mutex<()>,
    last: Mutex<Option<RefreshStats>>,
}

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "be", "to", "of", "in", "on", "for", "and", "or", "with",
    "where", "what", "which", "how", "does", "do", "this", "that", "it", "we", "i", "me", "my",
    "der", "die", "das", "und", "oder", "wo", "wird", "ist", "wie", "was", "ein", "eine", "den",
    "im", "in", "zu", "mit", "von", "für",
];

/// FTS5 query: every word (and every part of an identifier) as a prefix term,
/// combined with OR – BM25 ranks chunks that match more and rarer words first.
pub fn fts_query(q: &str) -> String {
    let mut terms: Vec<String> = Vec::new();
    for raw in q.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.is_empty() {
            continue;
        }
        let mut words = vec![raw.to_lowercase()];
        words.extend(split_identifier(raw).split(' ').map(String::from));
        for w in words {
            if w.chars().count() < 2 || STOPWORDS.contains(&w.as_str()) || terms.contains(&w) {
                continue;
            }
            terms.push(w);
        }
    }
    terms
        .iter()
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Words of the query that may name a symbol (`verify_token`, `Parser`).
fn symbol_candidates(q: &str) -> Vec<String> {
    q.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':' || c == '.'))
        .map(|w| w.trim_matches(|c| c == ':' || c == '.'))
        .filter(|w| w.len() >= 3 && !STOPWORDS.contains(&w.to_lowercase().as_str()))
        .map(|w| w.rsplit([':', '.']).next().unwrap_or(w).to_string())
        .collect()
}

/// Reciprocal Rank Fusion of ranked id lists.
pub fn rrf(lists: &[Vec<i64>]) -> Vec<(i64, f64)> {
    let mut score: HashMap<i64, f64> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *score.entry(*id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut out: Vec<(i64, f64)> = score.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

fn snippet(text: &str) -> String {
    let s: String = text.lines().take(30).collect::<Vec<_>>().join("\n");
    if s.chars().count() > 1500 {
        format!("{}…", s.chars().take(1500).collect::<String>())
    } else {
        s
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(&Sha256::digest(bytes)[..16])
}

/// Files of a project: `.gitignore`/`.ignore` respected, hidden files and
/// `.git` skipped, binaries and very large files excluded.
fn scan(root: &Path) -> Vec<(String, FileState)> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build()
        .flatten()
    {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_BYTES || meta.len() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i64);
        out.push((
            rel.to_string_lossy().replace('\\', "/"),
            FileState {
                size: meta.len(),
                mtime,
            },
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

impl Index {
    pub fn open(name: &str, root: Option<PathBuf>, db: &Path) -> Result<Self> {
        Ok(Self::with_store(name, root, Store::open(db)?))
    }

    pub fn with_store(name: &str, root: Option<PathBuf>, store: Store) -> Self {
        Self {
            name: name.to_string(),
            root,
            store,
            vectors: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
            last: Mutex::new(None),
        }
    }

    pub fn status(&self, embedder: &dyn Embedder) -> Result<IndexStatus> {
        let (files, chunks, embedded) = self.store.counts()?;
        let embed_model = embedder.model();
        Ok(IndexStatus {
            name: self.name.clone(),
            files,
            chunks,
            embedded,
            mode: if embed_model.is_some() {
                "hybrid"
            } else {
                "text"
            }
            .into(),
            embed_model,
            last_refresh: self.last.lock().unwrap().clone(),
        })
    }

    /// Brings a project index up to date (only changed files are processed).
    pub async fn refresh(
        &self,
        embedder: &dyn Embedder,
        bus: Option<&EventBus>,
    ) -> Result<RefreshStats> {
        let root = self
            .root
            .clone()
            .ok_or_else(|| Error::internal("not a project index"))?;
        let _guard = self.refreshing.lock().await;
        let started = Instant::now();
        let files = tokio::task::spawn_blocking({
            let root = root.clone();
            move || scan(&root)
        })
        .await
        .map_err(Error::internal)?;
        let known = self.store.files()?;
        let mut stats = RefreshStats {
            scanned: files.len() as u64,
            ..Default::default()
        };
        let present: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        let mut changed = false;
        for (path, state) in &files {
            match known.get(path) {
                Some((s, _)) if s == state => {
                    stats.unchanged += 1;
                    continue;
                }
                _ => {}
            }
            let Ok(bytes) = std::fs::read(root.join(path)) else {
                continue;
            };
            let h = hash(&bytes);
            if let Some((_, old)) = known.get(path)
                && *old == h
            {
                self.store.touch_file(path, *state)?;
                stats.unchanged += 1;
                continue;
            }
            if is_binary(&bytes) {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            let chunks = chunk_file(path, &text);
            self.store
                .put_file(path, Lang::of(path).name(), *state, &h, &chunks)?;
            changed = true;
            if known.contains_key(path) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
            if let Some(bus) = bus
                && (stats.added + stats.updated).is_multiple_of(200)
            {
                bus.emit(
                    "index.progress",
                    Some(&self.name),
                    json!({"processed": stats.added + stats.updated, "scanned": stats.scanned}),
                );
            }
        }
        for path in known.keys() {
            if !present.contains(path.as_str()) {
                self.store.remove_file(path)?;
                stats.removed += 1;
                changed = true;
            }
        }
        stats.embedded = self.embed_pending(embedder).await?;
        if changed || stats.embedded > 0 {
            *self.vectors.lock().unwrap() = None;
        }
        stats.duration_ms = started.elapsed().as_millis() as u64;
        *self.last.lock().unwrap() = Some(stats.clone());
        Ok(stats)
    }

    /// Replaces the documents of a document index (knowledge base).
    pub async fn put_documents(
        &self,
        docs: &[Document],
        embedder: &dyn Embedder,
    ) -> Result<RefreshStats> {
        let _guard = self.refreshing.lock().await;
        let started = Instant::now();
        let known = self.store.files()?;
        let mut stats = RefreshStats {
            scanned: docs.len() as u64,
            ..Default::default()
        };
        let ids: HashSet<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        for d in docs {
            let h = hash(d.text.as_bytes());
            if known.get(&d.id).is_some_and(|(_, old)| *old == h) {
                stats.unchanged += 1;
                continue;
            }
            // Documents are Markdown, whatever their id looks like.
            let chunks = chunk_file("doc.md", &d.text);
            let state = FileState {
                size: d.text.len() as u64,
                mtime: 0,
            };
            self.store.put_file(&d.id, "markdown", state, &h, &chunks)?;
            if known.contains_key(&d.id) {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
        }
        for id in known.keys() {
            if !ids.contains(id.as_str()) {
                self.store.remove_file(id)?;
                stats.removed += 1;
            }
        }
        stats.embedded = self.embed_pending(embedder).await?;
        *self.vectors.lock().unwrap() = None;
        stats.duration_ms = started.elapsed().as_millis() as u64;
        *self.last.lock().unwrap() = Some(stats.clone());
        Ok(stats)
    }

    /// Embeds chunks that have no embedding yet. A changed embedding model
    /// invalidates all embeddings.
    async fn embed_pending(&self, embedder: &dyn Embedder) -> Result<u64> {
        let Some(model) = embedder.model() else {
            return Ok(0);
        };
        if self.store.meta("embed_model")?.as_deref() != Some(model.as_str()) {
            self.store.clear_embeddings()?;
            self.store.set_meta("embed_model", &model)?;
        }
        let mut done = 0u64;
        loop {
            let batch = self.store.unembedded(EMBED_BATCH)?;
            if batch.is_empty() {
                return Ok(done);
            }
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let vectors = embedder.embed(texts).await?;
            if vectors.len() != batch.len() {
                return Err(Error::unavailable(
                    "the embedding model returned too few vectors",
                ));
            }
            let items: Vec<(i64, Vec<f32>)> =
                batch.iter().map(|(id, _)| *id).zip(vectors).collect();
            self.store.set_embeddings(&items)?;
            done += items.len() as u64;
        }
    }

    fn vector_cache(&self) -> Result<Vectors> {
        if let Some(v) = self.vectors.lock().unwrap().clone() {
            return Ok(v);
        }
        let v = Arc::new(self.store.vectors()?);
        *self.vectors.lock().unwrap() = Some(v.clone());
        Ok(v)
    }

    /// Hybrid search (no refresh – the caller decides).
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        embedder: &dyn Embedder,
    ) -> Result<SearchResult> {
        self.search_with(query, limit, embedder, Strategy::default())
            .await
    }

    /// Search with some parts switched off (to measure what each contributes).
    pub async fn search_with(
        &self,
        query: &str,
        limit: usize,
        embedder: &dyn Embedder,
        strategy: Strategy,
    ) -> Result<SearchResult> {
        let limit = limit.clamp(1, 50);
        let mut lists = Vec::new();
        if strategy.text {
            lists.push(self.store.fts(&fts_query(query), CANDIDATES)?);
        }
        let mut note = None;
        let hybrid = match embedder.model().filter(|_| strategy.vector) {
            Some(_) => match embedder.embed(vec![query.to_string()]).await {
                Ok(mut v) if !v.is_empty() => {
                    let q = v.remove(0);
                    let cache = self.vector_cache()?;
                    let mut scored: Vec<(i64, f32)> =
                        cache.iter().map(|(id, e)| (*id, dot(&q, e))).collect();
                    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
                    lists.push(
                        scored
                            .into_iter()
                            .take(CANDIDATES)
                            .map(|(id, _)| id)
                            .collect(),
                    );
                    true
                }
                Ok(_) => false,
                Err(e) => {
                    note = Some(format!(
                        "vector search unavailable ({}); full text only",
                        e.message()
                    ));
                    false
                }
            },
            None => {
                note = Some("full-text search only – add an embedding model for semantic search, e.g. `ancilo add hf.co/second-state/All-MiniLM-L6-v2-Embedding-GGUF`".into());
                false
            }
        };
        let mut fused = rrf(&lists);
        // An exact symbol name goes first.
        let mut exact: Vec<i64> = Vec::new();
        for s in symbol_candidates(query)
            .into_iter()
            .filter(|_| strategy.symbols)
        {
            for id in self.store.by_symbol(&s)? {
                if !exact.contains(&id) {
                    exact.push(id);
                }
            }
        }
        for (id, score) in &mut fused {
            if exact.contains(id) {
                *score += 1.0;
            }
        }
        for id in &exact {
            if !fused.iter().any(|(f, _)| f == id) {
                fused.push((*id, 1.0));
            }
        }
        fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        fused.truncate(limit);
        let ids: Vec<i64> = fused.iter().map(|(id, _)| *id).collect();
        let rows = self.store.rows(&ids)?;
        let hits = fused
            .iter()
            .filter_map(|(id, score)| {
                let r = rows.iter().find(|r| r.id == *id)?;
                Some(Hit {
                    path: r.path.clone(),
                    start_line: r.start_line,
                    end_line: r.end_line,
                    symbol: r.symbol.clone(),
                    kind: r.kind.clone(),
                    score: (*score * 1000.0).round() / 1000.0,
                    snippet: snippet(&r.text),
                })
            })
            .collect();
        Ok(SearchResult {
            hits,
            mode: match (hybrid, strategy.text) {
                (true, true) => "hybrid",
                (true, false) => "vector",
                _ => "text",
            }
            .into(),
            note,
        })
    }
}

/// Which parts of the search are used (all by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Strategy {
    pub text: bool,
    pub vector: bool,
    /// Exact symbol names first.
    pub symbols: bool,
}

impl Default for Strategy {
    fn default() -> Self {
        Self {
            text: true,
            vector: true,
            symbols: true,
        }
    }
}

impl Strategy {
    /// `hybrid` (default), `text` or `vector`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "hybrid" => Some(Self::default()),
            "text" => Some(Self {
                vector: false,
                ..Self::default()
            }),
            "vector" => Some(Self {
                text: false,
                symbols: false,
                ..Self::default()
            }),
            _ => None,
        }
    }
}

/// For a git worktree (`.git` is a file `gitdir: <main>/.git/worktrees/<n>`):
/// the main repository's root.
pub fn worktree_main(root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join(".git")).ok()?;
    let gitdir = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    let git = gitdir
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == ".git"))?;
    git.parent().map(Path::to_path_buf)
}

/// The project a directory belongs to: the enclosing git repository, else
/// the directory itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut dir = cwd.as_path();
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return cwd,
        }
    }
}

struct Inner {
    dir: PathBuf,
    bus: EventBus,
    embedder: Arc<dyn Embedder>,
    projects: Mutex<HashMap<PathBuf, Arc<Index>>>,
    knowledge: Mutex<Option<Arc<Index>>>,
}

/// The index service of the daemon.
#[derive(Clone)]
pub struct Indexer {
    inner: Arc<Inner>,
}

impl Indexer {
    /// `dir`: where index files live (`<home>/index`).
    pub fn new(dir: PathBuf, bus: EventBus, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir,
                bus,
                embedder,
                projects: Mutex::new(HashMap::new()),
                knowledge: Mutex::new(None),
            }),
        }
    }

    pub fn embedder(&self) -> &dyn Embedder {
        self.inner.embedder.as_ref()
    }

    fn file_for(&self, root: &Path) -> PathBuf {
        self.inner
            .dir
            .join(format!("{}.db", hash(root.to_string_lossy().as_bytes())))
    }

    fn project(&self, cwd: &Path) -> Result<Arc<Index>> {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(Error::invalid(format!(
                "cwd must be an existing absolute directory: {}",
                cwd.display()
            )));
        }
        let root = project_root(cwd);
        if let Some(i) = self.inner.projects.lock().unwrap().get(&root) {
            return Ok(i.clone());
        }
        let file = self.file_for(&root);
        // A git worktree (background task, comparison run) starts from the
        // index of its main repository: unchanged files are not embedded again.
        if !file.exists()
            && let Some(main) = worktree_main(&root)
            && main != root
            && self.file_for(&main).exists()
            && let Ok(main_index) = self.project(&main)
        {
            std::fs::create_dir_all(&self.inner.dir)?;
            if let Err(e) = main_index.store.copy_to(&file) {
                tracing::warn!(error = %e.message(), "could not seed the worktree index");
            }
        }
        let index = Arc::new(Index::open(
            &root.display().to_string(),
            Some(root.clone()),
            &file,
        )?);
        index.store.set_meta("root", &root.display().to_string())?;
        self.inner
            .projects
            .lock()
            .unwrap()
            .insert(root, index.clone());
        Ok(index)
    }

    fn knowledge(&self) -> Result<Arc<Index>> {
        let mut k = self.inner.knowledge.lock().unwrap();
        if let Some(i) = k.as_ref() {
            return Ok(i.clone());
        }
        let i = Arc::new(Index::open(
            "knowledge",
            None,
            &self.inner.dir.join("knowledge.db"),
        )?);
        *k = Some(i.clone());
        Ok(i)
    }

    /// Indexes a project (or brings its index up to date).
    pub async fn index_project(&self, cwd: &Path) -> Result<IndexStatus> {
        let index = self.project(cwd)?;
        let bus = &self.inner.bus;
        bus.emit("index.started", Some(&index.name), json!({}));
        match index.refresh(self.embedder(), Some(bus)).await {
            Ok(stats) => {
                bus.emit("index.updated", Some(&index.name), json!(stats));
                index.status(self.embedder())
            }
            Err(e) => {
                bus.emit(
                    "index.failed",
                    Some(&index.name),
                    json!({"error": e.message()}),
                );
                Err(e)
            }
        }
    }

    pub fn status(&self, cwd: &Path) -> Result<IndexStatus> {
        self.project(cwd)?.status(self.embedder())
    }

    /// Forgets a project's index (the file is deleted).
    pub fn remove(&self, cwd: &Path) -> Result<()> {
        let root = project_root(cwd);
        self.inner.projects.lock().unwrap().remove(&root);
        let file = self.file_for(&root);
        for suffix in ["", "-wal", "-shm"] {
            std::fs::remove_file(format!("{}{suffix}", file.display())).ok();
        }
        Ok(())
    }

    /// Searches a project (after bringing its index up to date).
    pub async fn search_project(
        &self,
        cwd: &Path,
        query: &str,
        limit: usize,
    ) -> Result<SearchResult> {
        self.search_project_with(cwd, query, limit, Strategy::default())
            .await
    }

    pub async fn search_project_with(
        &self,
        cwd: &Path,
        query: &str,
        limit: usize,
        strategy: Strategy,
    ) -> Result<SearchResult> {
        let index = self.project(cwd)?;
        // While the index is being built (e.g. right after a task started),
        // answer from what is there instead of waiting.
        let building = index.refreshing.try_lock().is_err();
        if !building && let Err(e) = index.refresh(self.embedder(), Some(&self.inner.bus)).await {
            tracing::warn!(error = %e.message(), "index refresh failed; searching what is indexed");
        }
        let mut r = index
            .search_with(query, limit, self.embedder(), strategy)
            .await?;
        if building {
            r.note = Some("the index is still being built – results may be incomplete".into());
        }
        Ok(r)
    }

    pub async fn search_knowledge(&self, query: &str, limit: usize) -> Result<SearchResult> {
        self.search_knowledge_with(query, limit, Strategy::default())
            .await
    }

    pub async fn search_knowledge_with(
        &self,
        query: &str,
        limit: usize,
        strategy: Strategy,
    ) -> Result<SearchResult> {
        self.knowledge()?
            .search_with(query, limit, self.embedder(), strategy)
            .await
    }

    /// Replaces the knowledge base's documents.
    pub async fn put_knowledge(&self, docs: &[Document]) -> Result<IndexStatus> {
        let k = self.knowledge()?;
        let stats = k.put_documents(docs, self.embedder()).await?;
        self.inner
            .bus
            .emit("knowledge.refreshed", Some("knowledge"), json!(stats));
        k.status(self.embedder())
    }

    /// Deletes indexes of projects that no longer exist (e.g. removed
    /// worktrees). Returns how many were removed.
    pub fn collect_garbage(&self) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.inner.dir) else {
            return 0;
        };
        let mut removed = 0;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "db")
                || path.file_name().is_some_and(|n| n == "knowledge.db")
            {
                continue;
            }
            let root = Store::open(&path)
                .ok()
                .and_then(|s| s.meta("root").ok().flatten());
            if root.is_some_and(|r| !Path::new(&r).exists()) {
                for suffix in ["", "-wal", "-shm"] {
                    std::fs::remove_file(format!("{}{suffix}", path.display())).ok();
                }
                removed += 1;
            }
        }
        removed
    }

    pub fn knowledge_status(&self) -> Result<IndexStatus> {
        self.knowledge()?.status(self.embedder())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic embeddings: hashed bag of words (like fake-llm).
    pub struct HashEmbedder;

    impl Embedder for HashEmbedder {
        fn model(&self) -> Option<String> {
            Some("hash".into())
        }
        fn embed<'a>(&'a self, texts: Vec<String>) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
            Box::pin(async move {
                Ok(texts
                    .iter()
                    .map(|t| {
                        let mut v = vec![0f32; 64];
                        for w in split_identifier(t).split_whitespace() {
                            let h = w
                                .bytes()
                                .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
                            v[(h % 64) as usize] += 1.0;
                        }
                        normalize(v)
                    })
                    .collect())
            })
        }
    }

    struct NoEmbedder;

    impl Embedder for NoEmbedder {
        fn model(&self) -> Option<String> {
            None
        }
        fn embed<'a>(&'a self, _: Vec<String>) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
            Box::pin(async { Err(Error::unavailable("none")) })
        }
    }

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (p, c) in files {
            let path = dir.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        dir
    }

    #[test]
    fn queries_become_prefix_terms_without_stopwords() {
        assert_eq!(
            fts_query("where is the token checked?"),
            "\"token\"* OR \"checked\"*"
        );
        assert_eq!(
            fts_query("parseHttpRequest"),
            "\"parsehttprequest\"* OR \"parse\"* OR \"http\"* OR \"request\"*"
        );
        assert_eq!(fts_query("\"; DROP"), "\"drop\"*");
        assert_eq!(
            symbol_candidates("where is Parser::parse called"),
            vec!["parse", "called"]
        );
    }

    #[test]
    fn rrf_rewards_agreement() {
        let fused = rrf(&[vec![1, 2, 3], vec![3, 1, 4]]);
        assert_eq!(fused[0].0, 1);
        assert_eq!(fused[1].0, 3);
        assert_eq!(fused.len(), 4);
    }

    // covers: M5-AC-02
    #[tokio::test]
    async fn only_changed_files_are_processed_and_ignored_files_are_left_out() {
        let dir = project(&[
            (
                "src/auth.rs",
                "pub fn verify_token(t: &str) -> bool { !t.is_empty() }\n",
            ),
            ("src/db.rs", "pub fn connect() {}\n"),
            ("target/debug/gen.rs", "pub fn generated() {}\n"),
            (".gitignore", "target/\n*.log\n"),
            ("app.log", "noise\n"),
            ("img.png", "\u{0}\u{1}binary"),
        ]);
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let db = tempfile::tempdir().unwrap();
        let index =
            Index::open("p", Some(dir.path().to_path_buf()), &db.path().join("p.db")).unwrap();
        let s = index.refresh(&HashEmbedder, None).await.unwrap();
        // auth.rs and db.rs; the PNG is read but skipped as binary, hidden and ignored files are not scanned.
        assert_eq!(
            (s.scanned, s.added, s.updated, s.removed),
            (3, 2, 0, 0),
            "{s:?}"
        );
        let files: Vec<String> = index.store.files().unwrap().into_keys().collect();
        assert!(
            files.contains(&"src/auth.rs".into())
                && !files
                    .iter()
                    .any(|f| f.starts_with("target") || f.ends_with(".log") || f.ends_with(".png")),
            "{files:?}"
        );
        assert_eq!(s.embedded, index.store.counts().unwrap().1);

        // Nothing changed → nothing processed.
        let s = index.refresh(&HashEmbedder, None).await.unwrap();
        assert_eq!((s.added, s.updated, s.removed, s.embedded), (0, 0, 0, 0));

        // Change one, add one, delete one.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("src/db.rs"), "pub fn connect_pool() {}\n").unwrap();
        std::fs::write(dir.path().join("src/new.rs"), "pub fn fresh() {}\n").unwrap();
        std::fs::remove_file(dir.path().join("src/auth.rs")).unwrap();
        let s = index.refresh(&HashEmbedder, None).await.unwrap();
        assert_eq!((s.added, s.updated, s.removed), (1, 1, 1), "{s:?}");
        assert_eq!(s.embedded, 2);
        let r = index
            .search("connect_pool", 5, &HashEmbedder)
            .await
            .unwrap();
        assert_eq!(r.hits[0].symbol.as_deref(), Some("connect_pool"));
        assert!(
            index
                .search("verify_token", 5, &HashEmbedder)
                .await
                .unwrap()
                .hits
                .iter()
                .all(|h| h.path != "src/auth.rs")
        );
    }

    // covers: M5-AC-04
    #[tokio::test]
    async fn identifiers_are_found_exactly_and_text_only_works() {
        let mut files: Vec<(String, String)> = (0..30)
            .map(|i| (format!("src/m{i}.rs"), format!("/// Mentions verify and token often: verify token token.\npub fn helper_{i}() {{ verify(); token(); }}\n")))
            .collect();
        files.push((
            "src/auth.rs".into(),
            "pub fn verify_token(t: &str) -> bool {\n    !t.is_empty()\n}\n".into(),
        ));
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let dir = project(&refs);
        let db = tempfile::tempdir().unwrap();
        let index =
            Index::open("p", Some(dir.path().to_path_buf()), &db.path().join("p.db")).unwrap();
        index.refresh(&NoEmbedder, None).await.unwrap();
        let r = index.search("verify_token", 3, &NoEmbedder).await.unwrap();
        assert_eq!(r.mode, "text");
        assert!(r.note.as_deref().unwrap().contains("embedding model"));
        assert_eq!(r.hits[0].path, "src/auth.rs");
        assert_eq!((r.hits[0].start_line, r.hits[0].end_line), (1, 3));
        // Embeddings are added later when a model appears.
        let s = index.refresh(&HashEmbedder, None).await.unwrap();
        assert_eq!(s.embedded, 31);
        let r = index
            .search("where is verify_token defined", 3, &HashEmbedder)
            .await
            .unwrap();
        assert_eq!(r.mode, "hybrid");
        assert_eq!(r.hits[0].path, "src/auth.rs");
    }

    #[tokio::test]
    async fn documents_replace_and_are_searchable() {
        let db = tempfile::tempdir().unwrap();
        let index = Index::open("knowledge", None, &db.path().join("k.db")).unwrap();
        let docs = vec![
            Document { id: "docs/models.md".into(), text: "# Models\n\n## Roles\n\nThe embed role serves embeddings.\n\n## Memory\n\nThe reserve keeps RAM free.\n".into() },
            Document { id: "hf:o/Qwen".into(), text: "# Qwen\n\nGood at tool calling.\n".into() },
        ];
        let s = index.put_documents(&docs, &HashEmbedder).await.unwrap();
        assert_eq!(s.added, 2);
        let r = index
            .search("how much RAM is kept free", 2, &HashEmbedder)
            .await
            .unwrap();
        assert_eq!(r.hits[0].symbol.as_deref(), Some("Memory"));
        let s = index
            .put_documents(&docs[..1], &HashEmbedder)
            .await
            .unwrap();
        assert_eq!((s.unchanged, s.removed), (1, 1));
    }

    // covers: M5-AC-05
    /// Benchmark: a repository with 10 000 files. Run with
    /// `cargo test -p ancilo-index --release -- --ignored bench --nocapture`.
    /// Embedding time of a real model comes on top (≈ 500 chunks/s with
    /// all-MiniLM-L6-v2 on an M5 Max); this measures Ancilo's own share.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "benchmark"]
    async fn bench_index_and_search_10k_files() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10_000 {
            let module = format!("src/m{}/f{i}.rs", i / 100);
            let path = dir.path().join(&module);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("/// Handles case {i}.\npub fn handle_{i}(x: u32) -> u32 {{\n    x + {i}\n}}\n\npub struct State{i} {{ pub value: u32 }}\n"),
            )
            .unwrap();
        }
        let db = tempfile::tempdir().unwrap();
        let index = Index::open(
            "bench",
            Some(dir.path().to_path_buf()),
            &db.path().join("b.db"),
        )
        .unwrap();
        let t = Instant::now();
        let s = index.refresh(&HashEmbedder, None).await.unwrap();
        let first = t.elapsed();
        let t = Instant::now();
        let s2 = index.refresh(&HashEmbedder, None).await.unwrap();
        let unchanged = t.elapsed();
        let mut lat = Vec::new();
        for i in 0..50 {
            let t = Instant::now();
            let r = index
                .search(&format!("handle case {}", i * 97), 8, &HashEmbedder)
                .await
                .unwrap();
            lat.push(t.elapsed().as_millis() as u64);
            assert!(!r.hits.is_empty());
        }
        let p95 = ancilo_core::stats::percentile(&lat, 95.0).unwrap();
        println!(
            "10k files: first index {:.1} s ({} chunks), no-change refresh {} ms, search p50 {} ms p95 {p95} ms",
            first.as_secs_f64(),
            s.embedded,
            unchanged.as_millis(),
            ancilo_core::stats::percentile(&lat, 50.0).unwrap()
        );
        assert_eq!(s2.unchanged, 10_000);
        assert!(
            unchanged.as_millis() < 2_000,
            "refresh without changes must be cheap"
        );
        assert!(p95 < 250, "search p95 {p95} ms");
    }
}
