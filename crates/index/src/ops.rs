//! Search and index operations – REST, CLI, MCP and the assistant.

use std::path::PathBuf;

use ancilo_core::{Error, NoInput, OpBuilder, Registry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Indexer;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Code and files of the project in `cwd`.
    #[default]
    Project,
    /// Ancilo's documentation, models, eval results and comparisons.
    Knowledge,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchInput {
    /// What to find – a question, a description or an identifier.
    pub query: String,
    #[serde(default)]
    pub scope: Scope,
    /// Absolute project directory (scope `project`).
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Number of hits (default 8, at most 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectInput {
    /// Absolute project directory.
    pub cwd: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrievalEvalInput {
    /// `retrieval` (code), `knowledge`, or a path to a YAML suite.
    #[serde(default)]
    pub suite: Option<String>,
    /// Hits considered (default 5).
    #[serde(default)]
    pub k: Option<usize>,
    /// `hybrid` (default), `text` or `vector` – to measure each part.
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Removed {
    pub removed: bool,
}

pub const SEARCH_DESCRIPTION: &str = "Find the places in a project (or in Ancilo's knowledge base) that are relevant to a question – hybrid search over meaning and exact words. Returns file, line range, symbol and a snippet per hit. Use it instead of reading many files; the index stays up to date automatically.";

pub fn register(registry: &mut Registry, indexer: Indexer, scratch: PathBuf) {
    let ix = indexer.clone();
    registry.register(
        OpBuilder::new("run_retrieval_eval")
            .summary("Measure search quality: recall@k and MRR on questions with known answers")
            .manage()
            .handler(move |_ctx, i: RetrievalEvalInput| {
                let (ix, scratch) = (ix.clone(), scratch.clone());
                async move {
                    let suite = crate::eval::load(i.suite.as_deref().unwrap_or("retrieval"))?;
                    let strategy = crate::Strategy::parse(i.mode.as_deref().unwrap_or("hybrid"))
                        .ok_or_else(|| Error::invalid("mode: hybrid, text or vector"))?;
                    crate::eval::run(&ix, &suite, i.k.unwrap_or(5), &scratch, strategy).await
                }
            }),
    );
    let ix = indexer.clone();
    registry.register(
        OpBuilder::new("search")
            .summary("Search a project's code or Ancilo's knowledge base")
            .description(SEARCH_DESCRIPTION)
            .handler(move |_ctx, i: SearchInput| {
                let ix = ix.clone();
                async move {
                    let limit = i.limit.unwrap_or(8);
                    match i.scope {
                        Scope::Knowledge => ix.search_knowledge(&i.query, limit).await,
                        Scope::Project => {
                            let cwd = i
                                .cwd
                                .ok_or_else(|| Error::invalid("scope project needs cwd"))?;
                            ix.search_project(&cwd, &i.query, limit).await
                        }
                    }
                }
            }),
    );
    let ix = indexer.clone();
    registry.register(
        OpBuilder::new("index_project")
            .summary("Index a project for search (or bring its index up to date)")
            .manage()
            .handler(move |_ctx, i: ProjectInput| {
                let ix = ix.clone();
                async move { ix.index_project(&i.cwd).await }
            }),
    );
    let ix = indexer.clone();
    registry.register(
        OpBuilder::new("index_status")
            .summary("Size and state of a project's search index")
            .handler(move |_ctx, i: ProjectInput| {
                let ix = ix.clone();
                async move { ix.status(&i.cwd) }
            }),
    );
    let ix = indexer.clone();
    registry.register(
        OpBuilder::new("remove_index")
            .summary("Delete a project's search index (it is rebuilt when needed)")
            .manage()
            .handler(move |_ctx, i: ProjectInput| {
                let ix = ix.clone();
                async move { ix.remove(&i.cwd).map(|_| Removed { removed: true }) }
            }),
    );
    let ix = indexer;
    registry.register(
        OpBuilder::new("knowledge_status")
            .summary("Size and state of Ancilo's knowledge base")
            .handler(move |_ctx, _i: NoInput| {
                let ix = ix.clone();
                async move { ix.knowledge_status() }
            }),
    );
}
