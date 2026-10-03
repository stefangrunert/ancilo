//! Documents attached to chats (decision `2026-10-03-drei-bereiche`): the
//! app sends a file's bytes to `POST /api/v1/attachments?name=…`; tools and
//! the command line attach a path. Either way only the text is kept.

use std::path::PathBuf;

use ancilo_core::{OpBuilder, Registry};
use ancilo_docs::library::Library;
use ancilo_docs::{AttachmentView, Attachments, extract::MAX_BYTES};
use axum::extract::{DefaultBodyLimit, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachPath {
    /// Full path of a PDF, Word (.docx), Excel, CSV or text file.
    pub path: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentRef {
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Removed {
    pub removed: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FolderRef {
    /// A chat project's folder.
    pub folder: PathBuf,
}

pub fn register(registry: &mut Registry, attachments: Attachments, library: Library) {
    let l = library.clone();
    registry.register(
        OpBuilder::new("folder_documents")
            .summary("What Ancilo has read of a chat project's folder: documents read, still reading, and what could not be read and why")
            .handler(move |_ctx, i: FolderRef| {
                let l = l.clone();
                async move { l.status(&i.folder) }
            }),
    );
    let l = library;
    registry.register(
        OpBuilder::new("read_folder")
            .summary(
                "Read a chat project's folder again: new and changed documents, in the background",
            )
            .handler(move |_ctx, i: FolderRef| {
                let l = l.clone();
                async move {
                    if !i.folder.is_absolute() || !i.folder.is_dir() {
                        return Err(ancilo_core::Error::invalid(format!(
                            "not a folder: {}",
                            i.folder.display()
                        )));
                    }
                    l.refresh_soon(&i.folder);
                    l.status(&i.folder)
                }
            }),
    );
    let a = attachments.clone();
    registry.register(
        OpBuilder::new("add_attachment")
            .summary("Read a document (PDF, Word, Excel, CSV, text) to ask about it in a chat")
            .description("Reads the file in a sandboxed process (no network, time limit) and keeps only its text. Pass the returned id in `attachments` of `ask`. A conversation with a document stays with the AI on this computer: no cloud model, no tools, and every web search asks first.")
            .handler(move |_ctx, i: AttachPath| {
                let a = a.clone();
                async move { a.add_path(&i.path).await }
            }),
    );
    let a = attachments.clone();
    registry.register(
        OpBuilder::new("get_attachment")
            .summary(
                "What was read of an attached document: kind, pages or sheets, length, warnings",
            )
            .handler(move |_ctx, i: AttachmentRef| {
                let a = a.clone();
                async move { a.view(&i.id) }
            }),
    );
    let a = attachments;
    registry.register(
        OpBuilder::new("remove_attachment")
            .summary("Remove an attached document and its text")
            .manage()
            .handler(move |_ctx, i: AttachmentRef| {
                let a = a.clone();
                async move { a.delete(&i.id).map(|_| Removed { removed: true }) }
            }),
    );
}

#[derive(Debug, Deserialize)]
struct UploadQuery {
    name: String,
}

/// `POST /api/v1/attachments?name=Vertrag.pdf` with the file as the body.
pub fn routes(attachments: Attachments) -> axum::Router<ancilo_server::AppState> {
    axum::Router::new()
        .route(
            "/api/v1/attachments",
            axum::routing::post(
                move |Query(q): Query<UploadQuery>, body: axum::body::Bytes| {
                    let a = attachments.clone();
                    async move { reply(a.add_bytes(&q.name, &body).await) }
                },
            ),
        )
        // The body limit sits a little above the largest file Ancilo reads,
        // so a too large file gets the plain answer, not a cut connection.
        .layer(DefaultBodyLimit::max(MAX_BYTES as usize + 1024 * 1024))
}

fn reply(r: ancilo_core::Result<AttachmentView>) -> Response {
    match r {
        Ok(v) => (StatusCode::OK, axum::Json(v)).into_response(),
        Err(e) => ancilo_server::ApiError(e).into_response(),
    }
}
