//! Sources of the knowledge base (M5): Ancilo's documentation, the curated
//! model notes, model cards of installed models, eval reports and the
//! comparison leaderboard. The index itself is source-agnostic
//! ([`ancilo_index::Indexer::put_knowledge`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ancilo_compare::Comparer;
use ancilo_core::{BoxFuture, NoInput, OpBuilder, Registry, Result};
use ancilo_index::{Document, IndexStatus, Indexer, SearchResult};
use ancilo_models::ModelManager;
use ancilo_models::manager::ModelSource;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

/// Ancilo's user documentation, built into the binary.
const DOCS: &[(&str, &str)] = &[
    (
        "docs/getting-started.md",
        include_str!("../../../docs/getting-started.md"),
    ),
    ("docs/models.md", include_str!("../../../docs/models.md")),
    (
        "docs/delegation.md",
        include_str!("../../../docs/delegation.md"),
    ),
    (
        "docs/model-api.md",
        include_str!("../../../docs/model-api.md"),
    ),
    (
        "docs/comparing-models.md",
        include_str!("../../../docs/comparing-models.md"),
    ),
    (
        "docs/troubleshooting.md",
        include_str!("../../../docs/troubleshooting.md"),
    ),
    ("docs/search.md", include_str!("../../../docs/search.md")),
    (
        "docs/assistant.md",
        include_str!("../../../docs/assistant.md"),
    ),
];

const CURATED: &str = include_str!("../../../knowledge/models.yaml");

/// The curated model notes as one document per model and topic.
pub fn curated() -> Vec<Document> {
    let Ok(v) = ancilo_eval::yaml_value(CURATED) else {
        return Vec::new();
    };
    let s = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let mut docs = Vec::new();
    for m in v["models"].as_array().into_iter().flatten() {
        let good: Vec<String> = m["good_for"]
            .as_array()
            .into_iter()
            .flatten()
            .map(s)
            .collect();
        docs.push(Document {
            id: format!("models/{}", s(&m["name"])),
            text: format!(
                "# {} ({} model)\n\nAdd it: `ancilo add {}`\n\nSize: {}\n\nGood for: {}\n\n{}\n",
                s(&m["name"]),
                s(&m["kind"]),
                s(&m["address"]),
                s(&m["size"]),
                good.join(", "),
                s(&m["notes"])
            ),
        });
    }
    for g in v["general"].as_array().into_iter().flatten() {
        docs.push(Document {
            id: format!("models/general/{}", s(&g["title"])),
            text: format!("# {}\n\n{}\n", s(&g["title"]), s(&g["text"])),
        });
    }
    docs
}

/// The latest report per suite × model × pipeline, summarized.
fn eval_reports(dir: &Path) -> Vec<Document> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut latest: std::collections::BTreeMap<String, (String, Value)> = Default::default();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(v) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let model = v["target"]["model"]
            .as_str()
            .or(v["model"].as_str())
            .unwrap_or_default();
        let label = v["target"]["label"].as_str().unwrap_or("delegation");
        let key = format!(
            "{}/{model}/{label}",
            v["suite"].as_str().unwrap_or_default()
        );
        let started = v["started_at"].as_str().unwrap_or_default().to_string();
        if latest.get(&key).is_none_or(|(s, _)| *s < started) {
            latest.insert(key, (started, v));
        }
    }
    latest
        .into_iter()
        .map(|(key, (started, v))| {
            let failed: Vec<&str> = v["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["success_rate"].as_f64().unwrap_or(0.0) < 1.0)
                .filter_map(|t| t["id"].as_str())
                .collect();
            Document {
                id: format!("evals/{key}"),
                text: format!(
                    "# Eval result: {key}\n\nMeasured on this machine at {started}.\n\nSuccess rate: {:.0} %. Failed tasks: {}.\n",
                    v["success_rate"].as_f64().unwrap_or(0.0) * 100.0,
                    if failed.is_empty() { "none".to_string() } else { failed.join(", ") }
                ),
            }
        })
        .collect()
}

/// Gathers every source and replaces the knowledge base. Model cards come
/// from the cache directory; `fetch_cards` refreshes it from Hugging Face.
pub struct Knowledge {
    pub indexer: Indexer,
    pub manager: ModelManager,
    pub comparer: Comparer,
    pub evals_dir: PathBuf,
    pub cards_dir: PathBuf,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Refreshed {
    pub documents: usize,
    pub model_cards_fetched: usize,
    pub model_card_errors: Vec<String>,
    pub index: IndexStatus,
}

fn card_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.md", id.replace(['/', ':'], "_")))
}

impl Knowledge {
    pub fn documents(&self) -> Vec<Document> {
        let mut docs: Vec<Document> = DOCS
            .iter()
            .map(|(id, text)| Document {
                id: id.to_string(),
                text: text.to_string(),
            })
            .collect();
        docs.extend(curated());
        docs.extend(eval_reports(&self.evals_dir));
        if let Ok(board) = self.comparer.leaderboard(None)
            && !board.entries.is_empty()
        {
            docs.push(Document {
                id: "results/leaderboard".into(),
                text: board.markdown,
            });
        }
        if let Ok(names) = self.manager.names() {
            for m in names.models {
                if let ModelSource::HuggingFace { repo, .. } = &m.source
                    && let Ok(card) = std::fs::read_to_string(card_file(&self.cards_dir, &m.id))
                {
                    let body: String = card.chars().take(20_000).collect();
                    docs.push(Document {
                        id: format!("hf:{repo}"),
                        text: format!("# Model card: {} ({repo})\n\n{body}", m.name),
                    });
                }
            }
        }
        docs
    }

    /// Downloads the model card of one model into the cache. `Ok(false)`:
    /// not a Hugging Face model, or it has no card. Contacts Hugging Face –
    /// only on the user's behalf (adding a model, `refresh_model_knowledge`).
    pub async fn fetch_card(&self, id: &str) -> Result<bool> {
        let Some(card) = self.manager.model_card(id).await? else {
            return Ok(false);
        };
        std::fs::create_dir_all(&self.cards_dir)?;
        std::fs::write(card_file(&self.cards_dir, id), card)?;
        Ok(true)
    }

    /// Downloads model cards of installed Hugging Face models (best effort).
    pub async fn fetch_cards(&self) -> (usize, Vec<String>) {
        let Ok(names) = self.manager.names() else {
            return (0, Vec::new());
        };
        let (mut n, mut errors) = (0, Vec::new());
        for m in names.models {
            if !matches!(m.source, ModelSource::HuggingFace { .. }) {
                continue;
            }
            match self.fetch_card(&m.id).await {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => errors.push(format!("{}: {}", m.id, e.message())),
            }
        }
        (n, errors)
    }

    /// Rebuilds the knowledge base. `fetch` first refreshes the model cards
    /// from Hugging Face – only when the user asked for it; otherwise the
    /// cards already on disk are indexed and nothing leaves the machine.
    pub async fn refresh(&self, fetch: bool) -> Result<Refreshed> {
        let (fetched, errors) = if fetch {
            self.fetch_cards().await
        } else {
            (0, Vec::new())
        };
        let docs = self.documents();
        let index = self.indexer.put_knowledge(&docs).await?;
        Ok(Refreshed {
            documents: docs.len(),
            model_cards_fetched: fetched,
            model_card_errors: errors,
            index,
        })
    }
}

pub fn register(registry: &mut Registry, knowledge: Arc<Knowledge>) {
    registry.register(
        OpBuilder::new("refresh_model_knowledge")
            .summary("Rebuild the knowledge base: docs, model notes, model cards from Hugging Face, eval results, leaderboard")
            .manage()
            .handler(move |_ctx, _i: NoInput| {
                let k = knowledge.clone();
                async move { k.refresh(true).await }
            }),
    );
}

/// The project index as the agent's `search` tool.
pub struct IndexSearch(pub Indexer);

/// Hits as compact text for a model.
pub fn hits_text(r: &SearchResult) -> String {
    if r.hits.is_empty() {
        return format!(
            "no matches{}",
            r.note
                .as_deref()
                .map(|n| format!(" ({n})"))
                .unwrap_or_default()
        );
    }
    let mut out = String::new();
    for h in &r.hits {
        out.push_str(&format!(
            "{}:{}-{}{}\n{}\n\n",
            h.path,
            h.start_line,
            h.end_line,
            h.symbol
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default(),
            h.snippet
        ));
    }
    out
}

/// Hits for an agent: where, what, and a short preview – the agent reads the
/// lines it needs itself, so full snippets would only cost tokens twice.
pub fn hits_brief(r: &SearchResult) -> String {
    if r.hits.is_empty() {
        return hits_text(r);
    }
    let mut out = String::new();
    for h in &r.hits {
        let preview: Vec<&str> = h
            .snippet
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(4)
            .collect();
        let preview: String = preview
            .iter()
            .map(|l| format!("  {}", l.chars().take(120).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str(&format!(
            "{}:{}-{}{}\n{}\n",
            h.path,
            h.start_line,
            h.end_line,
            h.symbol
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default(),
            preview
        ));
    }
    out.push_str("(read the lines you need with read_file and offset)");
    out
}

impl ancilo_agent::CodeSearch for IndexSearch {
    fn search(
        &self,
        root: PathBuf,
        query: String,
        limit: usize,
    ) -> BoxFuture<'static, std::result::Result<String, String>> {
        let ix = self.0.clone();
        Box::pin(async move {
            ix.search_project(&root, &query, limit)
                .await
                .map(|r| hits_brief(&r))
                .map_err(|e| e.message())
        })
    }

    fn prepare(&self, root: PathBuf) {
        let ix = self.0.clone();
        tokio::spawn(async move {
            if let Err(e) = ix.index_project(&root).await {
                tracing::info!(error = %e.message(), "background indexing failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_notes_and_docs_are_complete() {
        let docs = curated();
        assert!(docs.len() >= 8);
        assert!(docs.iter().all(|d| d.text.contains("# ")));
        assert!(docs.iter().any(|d| d.text.contains("ancilo add hf.co/")));
        assert!(DOCS.iter().all(|(_, t)| t.starts_with("# ")));
    }
}
