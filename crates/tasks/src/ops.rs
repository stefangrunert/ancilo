//! Task operations – the core of delegation.

use std::time::Duration;

use ancilo_core::{OpBuilder, Registry, Result};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{DelegateInput, TaskRunner, TaskView};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskRef {
    pub task_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResultInput {
    pub task_id: String,
    /// Include the full diff.
    #[serde(default)]
    pub detail: bool,
    /// Wait up to this many seconds for the task to finish (default 0).
    #[serde(default)]
    pub wait_s: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionInput {
    /// The most delegated tasks may do: read, edit or shell (sandboxed).
    pub max_access: ancilo_agent::Access,
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct Permissions {
    pub max_access: ancilo_agent::Access,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Description of `delegate` – it decides whether Claude/Codex use Ancilo well.
pub const DELEGATE_DESCRIPTION: &str = "Hand a well-defined coding task to Ancilo, a local model on this machine – free, private, and in parallel to your own work.
Good for: writing or extending tests, boilerplate, mechanical refactorings following a clear pattern, renames, documentation and comments, small well-specified fixes, summarising files or logs.
Not for: architecture decisions, subtle debugging, anything that needs broad judgement – keep those yourself.
Write the task self-contained: goal, files, constraints, how to verify. Synchronous by default (returns summary, changed files and a diff stat); with background=true it works on its own git branch and returns a task_id at once – fetch the result later with task_result.";

pub fn register(registry: &mut Registry, runner: TaskRunner) {
    let r = runner.clone();
    registry.register(
        OpBuilder::new("get_permissions")
            .summary("The most delegated tasks may do (read, edit, shell)")
            .handler(move |_ctx, _i: ancilo_core::NoInput| {
                let r = r.clone();
                async move {
                    Ok(Permissions {
                        max_access: r.max_access(),
                    })
                }
            }),
    );
    let r = runner.clone();
    registry.register(
        OpBuilder::new("set_permissions")
            .summary("Limit what delegated tasks may do: read, edit or shell (sandboxed)")
            .manage()
            .handler(move |_ctx, i: PermissionInput| {
                let r = r.clone();
                async move {
                    r.set_max_access(i.max_access)
                        .map(|max_access| Permissions { max_access })
                }
            }),
    );
    let r = runner.clone();
    registry.register(
        OpBuilder::new("delegate")
            .summary("Hand a well-defined coding task to the local model")
            .description(DELEGATE_DESCRIPTION)
            .manage()
            .handler(move |_ctx, i: DelegateInput| {
                let r = r.clone();
                async move { r.delegate(i).await as Result<TaskView> }
            }),
    );
    let r = runner.clone();
    registry.register(
        OpBuilder::new("task_status")
            .summary("Show the status of a delegated task")
            .handler(move |_ctx, i: TaskRef| {
                let r = r.clone();
                async move { r.status(&i.task_id) }
            }),
    );
    let r = runner.clone();
    registry.register(
        OpBuilder::new("task_result")
            .summary("Get the result of a delegated task (optionally wait for it, optionally with full diff)")
            .handler(move |_ctx, i: ResultInput| {
                let r = r.clone();
                async move { r.result(&i.task_id, i.detail, i.wait_s.map(Duration::from_secs)).await }
            }),
    );
    let r = runner.clone();
    registry.register(
        OpBuilder::new("cancel_task")
            .summary("Cancel a delegated task")
            .manage()
            .handler(move |_ctx, i: TaskRef| {
                let r = r.clone();
                async move { r.cancel(&i.task_id) }
            }),
    );
    let r = runner;
    registry.register(
        OpBuilder::new("list_tasks")
            .summary("List recent delegated tasks")
            .handler(move |_ctx, i: ListInput| {
                let r = r.clone();
                async move { r.list(i.limit.unwrap_or(20)) }
            }),
    );
}
