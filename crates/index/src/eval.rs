//! Retrieval evals (M5-AC-03/07): questions with the files or documents that
//! answer them. Measured: recall@k (the answer is among the first k hits) and
//! mean reciprocal rank; plus index and search timings.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use ancilo_core::{Error, Result};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Indexer, Strategy};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub name: String,
    /// Project files (code suites); empty for knowledge suites.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    pub queries: Vec<Query>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub q: String,
    /// Paths (or document ids); a hit on any of them counts.
    pub expect: Vec<String>,
}

pub fn builtin(name: &str) -> Option<Suite> {
    let text = match name {
        "retrieval" => include_str!("../../../evals/retrieval.yaml"),
        "knowledge" => include_str!("../../../evals/knowledge.yaml"),
        _ => return None,
    };
    serde_yaml::from_str(text).ok()
}

pub fn load(name_or_path: &str) -> Result<Suite> {
    if let Some(s) = builtin(name_or_path) {
        return Ok(s);
    }
    let text = std::fs::read_to_string(name_or_path)
        .map_err(|_| Error::not_found(format!("no retrieval suite '{name_or_path}'")))?;
    serde_yaml::from_str(&text).map_err(|e| Error::invalid(format!("{name_or_path}: {e}")))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryResult {
    pub q: String,
    /// 1-based rank of the first expected hit.
    pub rank: Option<usize>,
    pub top: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub suite: String,
    pub started_at: DateTime<Utc>,
    /// hybrid | text
    pub mode: String,
    pub embed_model: Option<String>,
    /// The search that was measured: hybrid, text or vector.
    pub strategy: String,
    pub k: usize,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub index_ms: u64,
    pub search_p50_ms: u64,
    pub search_p95_ms: u64,
    pub queries: Vec<QueryResult>,
}

/// Runs a suite: code suites are indexed in a fresh directory, knowledge
/// suites query the current knowledge base.
pub async fn run(
    indexer: &Indexer,
    suite: &Suite,
    k: usize,
    scratch: &Path,
    strategy: Strategy,
) -> Result<Report> {
    let k = k.clamp(1, 50);
    let started_at = Utc::now();
    let project = if suite.files.is_empty() {
        None
    } else {
        let dir = scratch.join(format!("retrieval-{}", uuid_like()));
        for (path, content) in &suite.files {
            let p = dir.join(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(p, content)?;
        }
        // A project of its own, even when the scratch directory lies inside
        // another repository.
        std::fs::create_dir_all(dir.join(".git"))?;
        Some(std::fs::canonicalize(&dir)?)
    };
    let t = Instant::now();
    let status = match &project {
        Some(dir) => indexer.index_project(dir).await?,
        None => indexer.knowledge_status()?,
    };
    let index_ms = t.elapsed().as_millis() as u64;
    let mut results = Vec::new();
    let mut latencies = Vec::new();
    for q in &suite.queries {
        let t = Instant::now();
        let r = match &project {
            Some(dir) => indexer.search_project_with(dir, &q.q, k, strategy).await?,
            None => indexer.search_knowledge_with(&q.q, k, strategy).await?,
        };
        latencies.push(t.elapsed().as_millis() as u64);
        let top: Vec<String> = r.hits.iter().map(|h| h.path.clone()).collect();
        let rank = top.iter().position(|p| q.expect.contains(p)).map(|i| i + 1);
        results.push(QueryResult {
            q: q.q.clone(),
            rank,
            top,
        });
    }
    if let Some(dir) = &project {
        indexer.remove(dir).ok();
        std::fs::remove_dir_all(dir).ok();
    }
    let n = results.len().max(1) as f64;
    Ok(Report {
        suite: suite.name.clone(),
        started_at,
        mode: status.mode,
        embed_model: status.embed_model,
        strategy: match (strategy.text, strategy.vector) {
            (true, true) => "hybrid",
            (true, false) => "text",
            _ => "vector",
        }
        .into(),
        k,
        recall_at_k: results.iter().filter(|r| r.rank.is_some()).count() as f64 / n,
        mrr: results
            .iter()
            .map(|r| r.rank.map_or(0.0, |x| 1.0 / x as f64))
            .sum::<f64>()
            / n,
        index_ms,
        search_p50_ms: ancilo_core::stats::percentile(&latencies, 50.0).unwrap_or(0),
        search_p95_ms: ancilo_core::stats::percentile(&latencies, 95.0).unwrap_or(0),
        queries: results,
    })
}

fn uuid_like() -> String {
    format!(
        "{:x}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_suites_parse() {
        let r = builtin("retrieval").unwrap();
        assert_eq!(r.queries.len(), 15);
        assert!(
            r.queries
                .iter()
                .all(|q| q.expect.iter().all(|e| r.files.contains_key(e)))
        );
        let k = builtin("knowledge").unwrap();
        assert!(k.files.is_empty() && k.queries.len() == 12);
    }
}
