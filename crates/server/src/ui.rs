//! The app's web UI, served under `/app/` (decision `2026-09-30-m7-umsetzung`).
//! Static files only – no secrets: the token is injected by the native shell
//! or passed in the URL fragment, which never reaches the server.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

/// `app/dist`: embedded in release builds, read from disk in debug builds.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../app/dist"]
#[allow_missing = true]
struct Ui;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws://127.0.0.1:* ws://localhost:*; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

fn file(path: &str) -> Option<Response> {
    let f = Ui::get(path)?;
    let cache = if path == "index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    Some(
        (
            [
                (header::CONTENT_TYPE, f.metadata.mimetype().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
                (header::CONTENT_SECURITY_POLICY, CSP.to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (header::REFERRER_POLICY, "no-referrer".to_string()),
            ],
            f.data.into_owned(),
        )
            .into_response(),
    )
}

pub async fn index() -> Response {
    file("index.html").unwrap_or_else(|| {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><title>Ancilo</title><p>The app is not built yet: <code>cd app &amp;&amp; npm run build</code></p>",
        )
            .into_response()
    })
}

pub async fn asset(Path(path): Path<String>) -> Response {
    if path.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    match file(&path) {
        Some(r) => r,
        // Client-side routes fall back to the app.
        None if !path.contains('.') => index().await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn redirect() -> Redirect {
    Redirect::permanent("/app/")
}
