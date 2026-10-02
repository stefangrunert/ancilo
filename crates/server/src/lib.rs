//! The local HTTP server.
//!
//! Security (Leitplanke 4) is enforced here for every request:
//! - bound to loopback only (by the daemon)
//! - `Host` must be a loopback name (defeats DNS rebinding)
//! - `Origin`, if present, must be a local app origin (defeats cross-site requests)
//! - a bearer token is required for everything except `/api/v1/health`

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use ancilo_core::{Error, EventBus, OpCtx, Registry, Surface};
use ancilo_storage::Db;
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

mod openapi;
mod ui;

pub use openapi::openapi;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<Registry>,
    pub token: Arc<String>,
    pub bus: EventBus,
    pub db: Db,
    pub started: Instant,
    /// Extra routes mounted by later milestones (model API, MCP).
    pub version: &'static str,
    /// Cancelled when the daemon stops: open event streams end, so the
    /// graceful shutdown does not wait for them forever.
    pub shutdown: tokio_util::sync::CancellationToken,
}

/// HTTP status for an error code.
pub fn status_of(e: &Error) -> StatusCode {
    match e {
        Error::InvalidInput(_) => StatusCode::BAD_REQUEST,
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::Conflict(_) => StatusCode::CONFLICT,
        Error::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        Error::PermissionDenied(_) => StatusCode::FORBIDDEN,
        Error::ConfirmationRequired(_) => StatusCode::PRECONDITION_REQUIRED,
        Error::InsufficientResources(_) => StatusCode::INSUFFICIENT_STORAGE,
        Error::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub struct ApiError(pub Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (status_of(&self.0), axum::Json(self.0.body())).into_response()
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

fn is_loopback_host(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host.rsplit_once(':').map_or(host, |(h, p)| {
            if p.chars().all(|c| c.is_ascii_digit()) {
                h
            } else {
                host
            }
        })
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

fn is_allowed_origin(origin: &str) -> bool {
    if matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    ) {
        return true;
    }
    for scheme in ["http://", "https://"] {
        if let Some(rest) = origin.strip_prefix(scheme) {
            return is_loopback_host(rest);
        }
    }
    false
}

/// Host/Origin guard – applied to every route, including unauthenticated ones.
async fn guard(headers: HeaderMap, req: Request, next: Next) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if !is_loopback_host(host) {
        return ApiError(Error::PermissionDenied(format!(
            "host '{host}' is not allowed"
        )))
        .into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok())
        && !is_allowed_origin(origin)
    {
        return ApiError(Error::PermissionDenied(format!(
            "origin '{origin}' is not allowed"
        )))
        .into_response();
    }
    next.run(req).await
}

/// Bearer token check.
async fn auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    // Bearer token (OpenAI clients, our CLI/app) or `x-api-key` (Anthropic clients).
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .unwrap_or_default();
    let ok: bool =
        subtle::ConstantTimeEq::ct_eq(presented.as_bytes(), state.token.as_bytes()).into();
    if !ok {
        return ApiError(Error::Unauthorized("missing or invalid token".into())).into_response();
    }
    next.run(req).await
}

async fn health(State(state): State<AppState>) -> Response {
    axum::Json(json!({
        "status": "ok",
        "product": ancilo_core::PRODUCT,
        "version": state.version,
        "protocol": ancilo_core::version::PROTOCOL,
        "uptime_s": state.started.elapsed().as_secs(),
    }))
    .into_response()
}

async fn list_ops(State(state): State<AppState>) -> Response {
    let specs: Vec<Value> = state
        .registry
        .specs()
        .map(|s| serde_json::to_value(s).unwrap_or_default())
        .collect();
    axum::Json(specs).into_response()
}

fn surface_of(headers: &HeaderMap) -> Surface {
    match headers
        .get("x-ancilo-surface")
        .and_then(|v| v.to_str().ok())
    {
        Some("cli") => Surface::Cli,
        Some("mcp") => Surface::Mcp,
        Some("assistant") => Surface::Assistant,
        _ => Surface::Rest,
    }
}

async fn call_op(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Option<axum::Json<Value>>,
) -> Result<Response, ApiError> {
    let confirmed = headers
        .get("x-ancilo-confirm")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "true" || v == "1");
    let ctx = OpCtx::new(surface_of(&headers)).confirmed(confirmed);
    let input = body.map(|b| b.0).unwrap_or(Value::Null);
    let out = state.registry.call(&name, ctx, input).await?;
    Ok(axum::Json(out).into_response())
}

async fn openapi_doc(State(state): State<AppState>) -> Response {
    axum::Json(openapi(&state.registry, state.version)).into_response()
}

#[derive(Deserialize)]
struct EventQuery {
    /// Replay persisted events with a sequence number greater than this.
    after: Option<i64>,
    /// Only kinds starting with this prefix.
    kind: Option<String>,
    /// Only events about this subject.
    subject: Option<String>,
}

fn to_sse(e: &ancilo_core::Event) -> SseEvent {
    SseEvent::default()
        .id(e.seq.to_string())
        .event(e.kind.clone())
        .data(serde_json::to_string(e).unwrap_or_default())
}

async fn events(
    State(state): State<AppState>,
    Query(q): Query<EventQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    // Subscribe first so nothing is lost between replay and live.
    let rx = state.bus.subscribe();
    let replay = match q.after {
        Some(after) => state
            .db
            .events_since(after, q.kind.as_deref(), 10_000)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let last_replayed = replay.last().map_or(q.after.unwrap_or(0), |e| e.seq);
    let kind = q.kind.clone();
    let subject = q.subject.clone();
    let matches = move |e: &ancilo_core::Event| {
        kind.as_deref().is_none_or(|k| e.kind.starts_with(k))
            && subject
                .as_deref()
                .is_none_or(|s| e.subject.as_deref() == Some(s))
    };
    let m2 = matches.clone();
    let replay = stream::iter(
        replay
            .into_iter()
            .filter(move |e| m2(e))
            .map(|e| Ok(to_sse(&e))),
    );
    let live = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |r| {
        let keep = match &r {
            Ok(e) => e.seq > last_replayed && matches(e),
            Err(_) => false,
        };
        async move {
            if keep {
                r.ok().map(|e| Ok(to_sse(&e)))
            } else {
                None
            }
        }
    });
    let stop = state.shutdown.clone().cancelled_owned();
    Sse::new(replay.chain(live).take_until(stop)).keep_alive(KeepAlive::default())
}

/// The control API. `extra` is merged in (authenticated) – later milestones
/// add the model API and MCP this way.
pub fn router(state: AppState, extra_authenticated: Router<AppState>) -> Router {
    router_with(state, extra_authenticated, Router::new())
}

/// Like [`router`], plus routes that authenticate themselves (the terminal
/// WebSocket uses one-time tickets) – still behind the Host/Origin guard.
pub fn router_with(
    state: AppState,
    extra_authenticated: Router<AppState>,
    public: Router,
) -> Router {
    let protected = Router::new()
        .route("/api/v1/ops", get(list_ops))
        .route("/api/v1/ops/{name}", post(call_op))
        .route("/api/v1/openapi.json", get(openapi_doc))
        .route("/api/v1/events", get(events))
        .merge(extra_authenticated)
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/app", get(ui::redirect))
        .route("/app/", get(ui::index))
        .route("/app/{*path}", get(ui::asset))
        .merge(protected)
        .with_state(state)
        .merge(public)
        .layer(middleware::from_fn(guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ancilo_core::{NoInput, OpBuilder};
    use schemars::JsonSchema;

    #[derive(Deserialize, JsonSchema)]
    struct Echo {
        text: String,
    }

    async fn serve() -> (String, String, EventBus) {
        let mut registry = Registry::new();
        registry.register(
            OpBuilder::new("echo")
                .summary("Echo")
                .handler(|_c, i: Echo| async move { Ok(i.text) }),
        );
        registry.register(
            OpBuilder::new("danger")
                .summary("Dangerous")
                .manage()
                .consequential()
                .handler(|_c, _i: NoInput| async move { Ok("done") }),
        );
        let db = Db::in_memory().unwrap();
        let bus = EventBus::new(Some(Arc::new(db.clone())), 0);
        let state = AppState {
            registry: Arc::new(registry),
            token: Arc::new("tok".into()),
            bus: bus.clone(),
            db,
            started: Instant::now(),
            version: "test",
            shutdown: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, router(state, Router::new()))
                .await
                .unwrap()
        });
        (url, "tok".into(), bus)
    }

    // covers: M1-AC-09
    #[tokio::test]
    async fn rejects_missing_token_foreign_host_and_foreign_origin() {
        let (url, token, _) = serve().await;
        let c = reqwest::Client::new();
        // Health is public but still guarded.
        assert_eq!(
            c.get(format!("{url}/api/v1/health"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        // No token.
        let r = c.get(format!("{url}/api/v1/ops")).send().await.unwrap();
        assert_eq!(r.status(), 401);
        // Wrong token.
        let r = c
            .get(format!("{url}/api/v1/ops"))
            .bearer_auth("nope")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        // DNS rebinding: foreign Host header.
        let r = c
            .get(format!("{url}/api/v1/health"))
            .header("Host", "evil.example:80")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        // Cross-site request from a web page.
        let r = c
            .post(format!("{url}/api/v1/ops/echo"))
            .bearer_auth(&token)
            .header("Origin", "https://evil.example")
            .json(&json!({"text": "x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        // The local app origin is fine.
        let r = c
            .post(format!("{url}/api/v1/ops/echo"))
            .bearer_auth(&token)
            .header("Origin", "tauri://localhost")
            .json(&json!({"text": "x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }

    #[tokio::test]
    async fn calls_operations_and_maps_errors() {
        let (url, token, _) = serve().await;
        let c = reqwest::Client::new();
        let r: Value = c
            .post(format!("{url}/api/v1/ops/echo"))
            .bearer_auth(&token)
            .json(&json!({"text": "hi"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r, json!("hi"));
        let r = c
            .post(format!("{url}/api/v1/ops/nope"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let r = c
            .post(format!("{url}/api/v1/ops/echo"))
            .bearer_auth(&token)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        let body: Value = r.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_input");
        let r = c
            .post(format!("{url}/api/v1/ops/danger"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 428);
        let r = c
            .post(format!("{url}/api/v1/ops/danger"))
            .bearer_auth(&token)
            .header("x-ancilo-confirm", "true")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }

    #[tokio::test]
    async fn streams_replayed_and_live_events() {
        let (url, token, bus) = serve().await;
        bus.emit("model.added", Some("m1"), json!({}));
        bus.emit("other.thing", Some("m2"), json!({}));
        let resp = reqwest::Client::new()
            .get(format!("{url}/api/v1/events?after=0&subject=m1"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        let mut body = resp.bytes_stream();
        let bus2 = bus.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            bus2.emit("model.running", Some("m1"), json!({"tokens_per_sec": 42}));
        });
        let mut text = String::new();
        while !text.contains("model.running") {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            text.push_str(&String::from_utf8_lossy(&chunk));
        }
        assert!(text.contains("event: model.added"));
        assert!(!text.contains("other.thing"));
    }

    #[tokio::test]
    async fn serves_openapi_with_every_operation() {
        let (url, token, _) = serve().await;
        let doc: Value = reqwest::Client::new()
            .get(format!("{url}/api/v1/openapi.json"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(doc["openapi"], "3.1.0");
        assert!(doc["paths"]["/api/v1/ops/echo"]["post"].is_object());
        assert!(doc["paths"]["/api/v1/ops/danger"]["post"].is_object());
    }

    #[test]
    fn loopback_detection() {
        for ok in ["127.0.0.1:7424", "localhost:1", "[::1]:80", "localhost"] {
            assert!(is_loopback_host(ok), "{ok}");
        }
        for bad in ["evil.com", "127.0.0.1.evil.com:80", "10.0.0.1:7424", ""] {
            assert!(!is_loopback_host(bad), "{bad}");
        }
        assert!(is_allowed_origin("http://localhost:5173"));
        assert!(!is_allowed_origin("http://localhost.evil.com"));
        assert!(!is_allowed_origin("null"));
    }
}
