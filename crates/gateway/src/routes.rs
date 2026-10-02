//! The model API: OpenAI- and Anthropic-compatible HTTP endpoints.

use std::convert::Infallible;
use std::time::Duration;

use ancilo_core::Error;
use ancilo_server::{AppState, status_of};
use axum::Extension;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::{Value, json};

use crate::reliability::ReliabilityConfig;
use crate::scheduler::Priority;
use crate::{CallOpts, ChatReply, Gateway, completion_to_sse};
use crate::{anthropic, responses};

pub fn router(gateway: Gateway) -> Router<AppState> {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses_api))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/embeddings", post(embeddings))
        .layer(Extension(gateway))
}

fn openai_error(e: &Error) -> Response {
    let kind = match e.code() {
        "invalid_input" => "invalid_request_error",
        "not_found" => "not_found_error",
        "unauthorized" => "authentication_error",
        _ => "server_error",
    };
    (
        status_of(e),
        axum::Json(json!({"error": {"message": e.message(), "type": kind, "code": e.code()}})),
    )
        .into_response()
}

fn anthropic_error(e: &Error) -> Response {
    let kind = match e.code() {
        "invalid_input" => "invalid_request_error",
        "not_found" => "not_found_error",
        "unauthorized" => "authentication_error",
        "unavailable" | "insufficient_resources" => "overloaded_error",
        _ => "api_error",
    };
    (
        status_of(e),
        axum::Json(json!({"type": "error", "error": {"type": kind, "message": e.message()}})),
    )
        .into_response()
}

fn opts(headers: &HeaderMap, api: &'static str) -> Result<CallOpts, Error> {
    let reliability = match headers
        .get("x-ancilo-reliability")
        .and_then(|v| v.to_str().ok())
    {
        Some(spec) => Some(ReliabilityConfig::parse(spec).map_err(Error::invalid)?),
        None => None,
    };
    let priority = headers
        .get("x-ancilo-priority")
        .and_then(|v| v.to_str().ok())
        .and_then(Priority::parse)
        .unwrap_or(Priority::Interactive);
    Ok(CallOpts {
        priority,
        reliability,
        api,
        // Clients choose their model themselves.
        local_only: false,
    })
}

fn sse(body: impl Stream<Item = Result<Bytes, Infallible>> + Send + 'static) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(body))
        .unwrap()
}

/// While `fut` runs (e.g. a model is loading), sends SSE comments so clients
/// see progress instead of timing out; then the stream `fut` produces.
fn with_keepalive<F, S>(fut: F) -> impl Stream<Item = Result<Bytes, Infallible>> + Send
where
    F: std::future::Future<Output = S> + Send + 'static,
    S: Stream<Item = Result<Bytes, Infallible>> + Send + 'static,
{
    enum St<F, S> {
        Waiting(std::pin::Pin<Box<F>>, tokio::time::Interval),
        Streaming(std::pin::Pin<Box<S>>),
    }
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    futures::stream::unfold(
        St::<F, S>::Waiting(Box::pin(fut), interval),
        |state| async move {
            match state {
                St::Waiting(mut fut, mut interval) => {
                    tokio::select! {
                        s = &mut fut => {
                            let mut s = Box::pin(s);
                            let first = s.next().await?;
                            Some((first, St::Streaming(s)))
                        }
                        _ = interval.tick() => Some((Ok(Bytes::from_static(b": loading model\n\n")), St::Waiting(fut, interval))),
                    }
                }
                St::Streaming(mut s) => {
                    let item = s.next().await?;
                    Some((item, St::Streaming(s)))
                }
            }
        },
    )
}

async fn models(Extension(gw): Extension<Gateway>) -> Response {
    match gw.models() {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => openai_error(&e),
    }
}

fn error_sse_openai(e: &Error) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"error": {"message": e.message(), "code": e.code()}})
    ))
}

async fn chat(
    Extension(gw): Extension<Gateway>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let opts = match opts(&headers, "openai") {
        Ok(o) => o,
        Err(e) => return openai_error(&e),
    };
    let stream = body["stream"].as_bool().unwrap_or(false);
    if !stream {
        return match gw.chat(body, opts).await {
            Ok((ChatReply::Complete(v), _)) => axum::Json(v).into_response(),
            Ok((ChatReply::Stream(_), _)) => openai_error(&Error::internal("unexpected stream")),
            Err(e) => openai_error(&e),
        };
    }
    let fut = async move {
        let out: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, Infallible>> + Send>> =
            match gw.chat(body, opts).await {
                Ok((ChatReply::Complete(v), _)) => Box::pin(futures::stream::once(async move {
                    Ok(Bytes::from(completion_to_sse(&v)))
                })),
                Ok((ChatReply::Stream(s), _)) => {
                    Box::pin(s.map(|r| Ok(r.unwrap_or_else(|e| error_sse_openai(&e)))))
                }
                Err(e) => Box::pin(futures::stream::once(
                    async move { Ok(error_sse_openai(&e)) },
                )),
            };
        out
    };
    sse(with_keepalive(fut))
}

fn anthropic_sse(events: Vec<(&'static str, Value)>) -> Bytes {
    let mut s = String::new();
    for (name, data) in events {
        s.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    Bytes::from(s)
}

async fn messages(
    Extension(gw): Extension<Gateway>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let opts = match opts(&headers, "anthropic") {
        Ok(o) => o,
        Err(e) => return anthropic_error(&e),
    };
    let requested = body["model"].as_str().unwrap_or_default().to_string();
    let stream = body["stream"].as_bool().unwrap_or(false);
    let mut req = match anthropic::request_to_openai(&body) {
        Ok(r) => r,
        Err(e) => return anthropic_error(&e),
    };
    let has_tools = req["tools"].is_array();
    if !stream {
        return match gw.chat(req, opts).await {
            Ok((ChatReply::Complete(v), _)) => {
                axum::Json(anthropic::response_from_openai(&v, &requested)).into_response()
            }
            Ok((ChatReply::Stream(_), _)) => anthropic_error(&Error::internal("unexpected stream")),
            Err(e) => anthropic_error(&e),
        };
    }
    let error_event = |e: &Error| {
        anthropic_sse(vec![(
            "error",
            json!({"type": "error", "error": {"type": "api_error", "message": e.message()}}),
        )])
    };
    // Tool requests go through the pipeline as a whole; plain text streams live.
    req["stream"] = json!(!has_tools);
    let fut = async move {
        let out: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, Infallible>> + Send>> = match gw
            .chat(req, opts)
            .await
        {
            Ok((ChatReply::Complete(v), _)) => {
                let msg = anthropic::response_from_openai(&v, &requested);
                Box::pin(futures::stream::once(async move {
                    Ok(anthropic_sse(anthropic::stream_events(&msg)))
                }))
            }
            Ok((ChatReply::Stream(s), _)) => {
                let converter = anthropic::StreamConverter::new(&requested);
                let lines = s.scan((converter, String::new(), false), move |(conv, buf, done), chunk| {
                    let mut events = Vec::new();
                    match chunk {
                        Ok(bytes) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(end) = buf.find("\n\n") {
                                let block: String = buf.drain(..end + 2).collect();
                                for line in block.lines().filter_map(|l| l.strip_prefix("data: ")) {
                                    if line.trim() == "[DONE]" {
                                        if !*done {
                                            events.extend(conv.finish());
                                            *done = true;
                                        }
                                    } else if let Ok(v) = serde_json::from_str::<Value>(line) {
                                        events.extend(conv.chunk(&v));
                                    }
                                }
                            }
                        }
                        Err(e) => events.push(("error", json!({"type": "error", "error": {"type": "api_error", "message": e.message()}}))),
                    }
                    futures::future::ready(Some(Ok::<_, Infallible>(anthropic_sse(events))))
                });
                Box::pin(lines)
            }
            Err(e) => Box::pin(futures::stream::once(async move { Ok(error_event(&e)) })),
        };
        out
    };
    sse(with_keepalive(fut))
}

fn responses_sse(events: Vec<(String, Value)>) -> Bytes {
    let mut s = String::new();
    for (name, data) in events {
        s.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    Bytes::from(s)
}

/// OpenAI Responses API (used by Codex). Answers are computed as a whole –
/// through the reliability pipeline – and streamed as `response.*` events.
async fn responses_api(
    Extension(gw): Extension<Gateway>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let opts = match opts(&headers, "responses") {
        Ok(o) => o,
        Err(e) => return openai_error(&e),
    };
    let requested = body["model"].as_str().unwrap_or_default().to_string();
    let custom = responses::custom_tool_names(&body);
    let stream = body["stream"].as_bool().unwrap_or(false);
    let mut req = match responses::request_to_chat(&body) {
        Ok(r) => r,
        Err(e) => return openai_error(&e),
    };
    req["stream"] = json!(false);
    if !stream {
        return match gw.chat(req, opts).await {
            Ok((ChatReply::Complete(v), _)) => {
                axum::Json(responses::response_from_chat(&v, &requested, &custom)).into_response()
            }
            Ok((ChatReply::Stream(_), _)) => openai_error(&Error::internal("unexpected stream")),
            Err(e) => openai_error(&e),
        };
    }
    let fut = async move {
        let out: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, Infallible>> + Send>> = match gw
            .chat(req, opts)
            .await
        {
            Ok((ChatReply::Complete(v), _)) => {
                let r = responses::response_from_chat(&v, &requested, &custom);
                Box::pin(futures::stream::once(async move {
                    Ok(responses_sse(responses::stream_events(&r)))
                }))
            }
            Ok((ChatReply::Stream(_), _)) => Box::pin(futures::stream::once(async move {
                Ok(responses_sse(vec![(
                    "response.failed".into(),
                    json!({"type": "response.failed", "response": {"status": "failed", "error": {"message": "unexpected stream"}}}),
                )]))
            })),
            Err(e) => {
                let data = json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": e.code(), "message": e.message()}}});
                Box::pin(futures::stream::once(async move {
                    Ok(responses_sse(vec![("response.failed".into(), data)]))
                }))
            }
        };
        out
    };
    sse(with_keepalive(fut))
}

async fn count_tokens(axum::Json(body): axum::Json<Value>) -> Response {
    axum::Json(json!({"input_tokens": anthropic::estimate_tokens(&body)})).into_response()
}

async fn embeddings(
    Extension(gw): Extension<Gateway>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    match gw.embeddings(body).await {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => openai_error(&e),
    }
}
