//! Session and terminal operations – the app's coding view uses exactly
//! these; so can CLI, API and MCP clients (M8-AC-08).

use std::path::PathBuf;

use ancilo_agent::Access;
use ancilo_core::{Error, NoInput, OpBuilder, Registry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Sessions;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameProjectInput {
    /// The project's folder.
    pub path: PathBuf,
    /// The name shown (empty: the folder's name).
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReorderProjectsInput {
    /// Project folders in the order wanted.
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReorderSessionsInput {
    /// Session ids in the order wanted.
    pub sessions: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NameInput {
    /// What it is, e.g. "Meine Rezepte-Webseite".
    pub name: String,
    /// The folder the project folder goes into (default: ~/Ancilo).
    #[serde(default)]
    pub parent: Option<std::path::PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathInput {
    /// Absolute project directory.
    pub path: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInput {
    /// Absolute project directory.
    pub cwd: PathBuf,
    /// Model id or role (default: role `coding`, else the default model).
    #[serde(default)]
    pub model: Option<String>,
    /// What the agent may do without asking: read, edit (default), shell.
    #[serde(default)]
    pub permission: Option<Access>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    /// A folder of the Tasks area to work on. Without one: a free task in a
    /// folder of its own (`<projects folder>/<free_dir>/<title>`).
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// The folder free tasks go into (the app's word for "Tasks").
    #[serde(default)]
    pub free_dir: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// read: ask before every change; edit/shell: changes in the copy without asking.
    #[serde(default)]
    pub permission: Option<Access>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskFileInput {
    pub session: String,
    /// Full path of a file to put into the task's copy.
    pub path: PathBuf,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Added {
    /// The name the file got in the copy.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub session: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageInput {
    pub session: String,
    pub text: String,
    /// Wait until the turn is done (default: return at once, follow events).
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveInput {
    /// Id of the approval (`ap-…`).
    pub approval: String,
    /// Allow this kind of action for the rest of the session.
    #[serde(default)]
    pub remember: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRef {
    pub approval: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInput {
    /// Only sessions of this project.
    #[serde(default)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiffInput {
    pub session: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangesInput {
    pub session: String,
    /// A variant from "retry with another model".
    #[serde(default)]
    pub variant: Option<String>,
    /// Only these files (default: all).
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Tasks: the changes as the user saw them (`changes_version`); if they
    /// differ now, nothing is applied.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryInput {
    pub session: String,
    pub model: String,
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateInput {
    pub session: String,
    /// Model id or role; empty string: back to the role `coding`.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission: Option<Access>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalInput {
    /// Working directory (absolute; default: the session's work area).
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// The terminal belongs to this session: it opens where the agent works
    /// and shows the agent's commands.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalRef {
    pub terminal: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Files {
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Opened {
    pub terminal: crate::pty::TerminalView,
    pub ticket: crate::pty::Ticket,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Done {
    pub ok: bool,
}

pub fn register(registry: &mut Registry, sessions: Sessions) {
    macro_rules! op {
        ($name:literal, $summary:literal, manage = $m:expr, conseq = $c:expr, |$s:ident, $i:ident : $t:ty| $body:expr) => {{
            let s2 = sessions.clone();
            let b = OpBuilder::new($name).summary($summary);
            let b = if $m { b.manage() } else { b };
            let b = if $c { b.consequential() } else { b };
            registry.register(b.handler(move |_ctx, $i: $t| {
                let $s = s2.clone();
                async move { $body }
            }));
        }};
    }
    op!(
        "open_project",
        "Open a project for coding (starts its search index)",
        manage = false,
        conseq = false,
        |s, i: PathInput| s.open_project(&i.path)
    );
    op!(
        "create_project",
        "Start a new, empty project to build something (a folder named after it in `parent`, default ~/Ancilo, set up for undo)",
        manage = true,
        conseq = false,
        |s, i: NameInput| s.create_project(&i.name, i.parent.as_deref())
    );
    op!(
        "rename_project",
        "Give a project another name in the list (the folder stays as it is)",
        manage = true,
        conseq = false,
        |s, i: RenameProjectInput| s.rename_project(&i.path, &i.name)
    );
    op!(
        "reorder_projects",
        "Put the projects in the list in this order",
        manage = true,
        conseq = false,
        |s, i: ReorderProjectsInput| s.reorder_projects(&i.paths)
    );
    op!(
        "reorder_sessions",
        "Put a project's sessions in this order",
        manage = true,
        conseq = false,
        |s, i: ReorderSessionsInput| s.reorder_sessions(&i.sessions).map(|_| Done { ok: true })
    );
    op!(
        "list_projects",
        "Projects opened for coding or with sessions (most recently used first)",
        manage = false,
        conseq = false,
        |s, _i: NoInput| s.projects()
    );
    op!(
        "remove_project",
        "Take a project off the list, with its sessions (unapplied changes are lost; the folder stays)",
        manage = true,
        conseq = true,
        |s, i: PathInput| s.remove_project(&i.path).await.map(|_| Done { ok: true })
    );
    op!(
        "create_session",
        "Start a coding session in a project",
        manage = true,
        conseq = false,
        |s, i: CreateInput| s.create(&i.cwd, i.model, i.permission, i.title)
    );
    op!(
        "create_task",
        "Start a task: the agent works with documents in a copy of a folder (or, without one, in a new folder of its own); the user keeps the changes or not",
        manage = true,
        conseq = false,
        |s, i: TaskInput| s.create_task(
            i.folder.as_deref(),
            i.free_dir.as_deref(),
            i.title,
            i.permission
        )
    );
    op!(
        "open_task_folder",
        "Add a folder to the Tasks area (Ancilo works on it only in a copy)",
        manage = true,
        conseq = false,
        |s, i: PathInput| s.open_task_folder(&i.path)
    );
    op!(
        "undo_apply",
        "Take back a task's last applied changes – if the folder still holds them",
        manage = true,
        conseq = true,
        |s, i: SessionRef| s.undo_apply(&i.session).await
    );
    op!(
        "add_task_file",
        "Put a file into a task's copy (it reaches the folder only if kept)",
        manage = true,
        conseq = false,
        |s, i: TaskFileInput| {
            let bytes = std::fs::read(&i.path)
                .map_err(|e| Error::invalid(format!("cannot read {}: {e}", i.path.display())))?;
            s.add_file(&i.session, &i.path.to_string_lossy(), &bytes)
                .await
                .map(|name| Added { name })
        }
    );
    op!(
        "send_message",
        "Tell the coding agent what to do next in a session",
        manage = true,
        conseq = false,
        |s, i: MessageInput| s.send(&i.session, &i.text, i.wait).await
    );
    op!(
        "approve",
        "Allow an action the coding agent asked for",
        manage = true,
        conseq = false,
        |s, i: ApproveInput| s.decide(&i.approval, true, i.remember)
    );
    op!(
        "reject",
        "Refuse an action the coding agent asked for",
        manage = true,
        conseq = false,
        |s, i: ApprovalRef| s.decide(&i.approval, false, false)
    );
    op!(
        "get_session",
        "A coding session with its conversation, changes and open approvals",
        manage = false,
        conseq = false,
        |s, i: SessionRef| s.get(&i.session)
    );
    op!(
        "list_sessions",
        "Coding sessions (newest first)",
        manage = false,
        conseq = false,
        |s, i: ListInput| s.list(i.project.as_deref())
    );
    op!(
        "session_diff",
        "The changes of a session (or of a variant) as a diff",
        manage = false,
        conseq = false,
        |s, i: DiffInput| s.diff(&i.session, i.variant.as_deref(), i.path.as_deref())
    );
    op!(
        "apply_changes",
        "Put a session's changes into the project (all or some files)",
        manage = true,
        conseq = true,
        |s, i: ChangesInput| s
            .apply_seen(
                &i.session,
                i.variant.as_deref(),
                i.paths,
                i.version.as_deref()
            )
            .await
            .map(|files| Files { files })
    );
    op!(
        "discard_changes",
        "Drop a session's changes (all or some files) without traces",
        manage = true,
        conseq = false,
        |s, i: ChangesInput| s
            .discard(&i.session, i.variant.as_deref(), i.paths)
            .await
            .map(|files| Files { files })
    );
    op!(
        "retry_with_model",
        "Do the last turn again with another model, side by side",
        manage = true,
        conseq = false,
        |s, i: RetryInput| s.retry(&i.session, &i.model, i.wait).await
    );
    op!(
        "cancel_turn",
        "Stop the coding agent's current turn",
        manage = true,
        conseq = false,
        |s, i: SessionRef| s.cancel(&i.session).map(|_| Done { ok: true })
    );
    op!(
        "update_session",
        "Change a session's model, permission or title",
        manage = true,
        conseq = false,
        |s, i: UpdateInput| s
            .set(
                &i.session,
                i.model.map(|m| (!m.is_empty()).then_some(m)),
                i.permission,
                i.title
            )
            .await
    );
    op!(
        "delete_session",
        "Delete a session and its work area (unapplied changes are lost)",
        manage = true,
        conseq = true,
        |s, i: SessionRef| s.delete(&i.session).await.map(|_| Done { ok: true })
    );
    op!(
        "open_terminal",
        "Open a terminal (lives in Ancilo, survives closing the window)",
        manage = true,
        conseq = false,
        |s, i: TerminalInput| {
            let cwd = match (i.cwd, &i.session) {
                (Some(c), _) => c,
                // The project folder – where the user works; the agent's
                // changes arrive there once they are kept.
                (None, Some(id)) => s.get(id)?.project,
                (None, None) => {
                    return Err(ancilo_core::Error::invalid(
                        "give a directory (cwd) or a session",
                    ));
                }
            };
            let t =
                s.terminals()
                    .open(&cwd, i.session, i.cols.unwrap_or(100), i.rows.unwrap_or(30))?;
            let ticket = s.terminals().ticket(&t.id)?;
            Ok(Opened {
                terminal: t,
                ticket,
            })
        }
    );
    op!(
        "terminal_ticket",
        "A one-time ticket to connect to a terminal",
        manage = true,
        conseq = false,
        |s, i: TerminalRef| s.terminals().ticket(&i.terminal)
    );
    op!(
        "list_terminals",
        "Open terminals",
        manage = false,
        conseq = false,
        |s, _i: NoInput| Ok::<_, ancilo_core::Error>(s.terminals().list())
    );
    op!(
        "close_terminal",
        "Close a terminal",
        manage = true,
        conseq = false,
        |s, i: TerminalRef| s.terminals().close(&i.terminal).map(|_| Done { ok: true })
    );
}

#[derive(Debug, Deserialize)]
struct FileQuery {
    name: String,
}

/// `POST /api/v1/sessions/{id}/files?name=…` with a file as the body: into a
/// task's copy (the app's drag and drop).
pub fn files_route<S: Clone + Send + Sync + 'static>(sessions: Sessions) -> axum::Router<S> {
    use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query};
    use axum::response::IntoResponse;
    axum::Router::new()
        .route(
            "/api/v1/sessions/{id}/files",
            axum::routing::post(
                move |UrlPath(id): UrlPath<String>,
                      Query(q): Query<FileQuery>,
                      body: axum::body::Bytes| {
                    let s = sessions.clone();
                    async move {
                        match s.add_file(&id, &q.name, &body).await {
                            Ok(name) => (
                                axum::http::StatusCode::OK,
                                axum::Json(serde_json::json!({"name": name})),
                            )
                                .into_response(),
                            Err(e) => {
                                let status = match e {
                                    Error::NotFound(_) => axum::http::StatusCode::NOT_FOUND,
                                    Error::Conflict(_) => axum::http::StatusCode::CONFLICT,
                                    _ => axum::http::StatusCode::BAD_REQUEST,
                                };
                                (status, axum::Json(e.body())).into_response()
                            }
                        }
                    }
                },
            ),
        )
        .layer(DefaultBodyLimit::max(
            ancilo_docs::extract::MAX_BYTES as usize + 1024 * 1024,
        ))
}
