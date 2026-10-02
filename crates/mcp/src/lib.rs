//! MCP server: Claude Code, Codex and other MCP clients use Ancilo's
//! operations as tools.
//!
//! JSON-RPC 2.0 over Streamable HTTP (`POST /mcp`, JSON responses). The CLI's
//! `ancilo mcp` bridges stdio clients to this endpoint.
//!
//! Tools: the delegation essentials under their own names (so the model sees
//! lean, well-described tools) plus one generic tool `ancilo` that reaches every
//! other operation – nothing exists only in another surface.

use ancilo_core::{OpCtx, OpSpec, Surface};
use ancilo_server::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

/// Protocol versions we speak, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Operations exposed as MCP tools under their own name (→ tool name).
pub const DIRECT_TOOLS: &[(&str, &str)] = &[
    ("delegate", "delegate"),
    ("task_status", "task_status"),
    ("task_result", "task_result"),
    ("cancel_task", "cancel_task"),
    ("list_tasks", "list_tasks"),
    ("list_models", "models"),
    ("search", "search"),
    ("ask", "ask"),
];

pub const INSTRUCTIONS: &str = "Ancilo runs local language models on this machine. Use `delegate` to hand well-defined coding tasks (tests, boilerplate, mechanical refactorings, docs, small fixes, summaries) to the local model – it is free, private and works in parallel to you. Keep architecture and tricky debugging yourself. Use `ancilo` for anything else Ancilo can do (list operations with {\"operation\": \"help\"}).";

pub fn router() -> Router<AppState> {
    Router::new().route("/mcp", post(handle).get(no_stream))
}

async fn no_stream() -> Response {
    // No server-initiated messages: SSE on GET is not offered.
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn schema(spec: &OpSpec) -> Value {
    let mut s = spec.input_schema.clone();
    if let Value::Object(m) = &mut s {
        m.remove("$schema");
        m.remove("title");
        m.entry("type").or_insert(json!("object"));
        m.entry("properties").or_insert(json!({}));
    }
    s
}

fn tools(state: &AppState) -> Vec<Value> {
    let mut out: Vec<Value> = DIRECT_TOOLS
        .iter()
        .filter_map(|(op, tool)| {
            let spec = state.registry.get(op)?.spec();
            Some(json!({
                "name": tool,
                "description": spec.description,
                "inputSchema": schema(spec),
                "annotations": {"readOnlyHint": spec.permission == ancilo_core::Permission::Read}
            }))
        })
        .collect();
    let names: Vec<&str> = state.registry.names();
    out.push(json!({
        "name": "ancilo",
        "description": format!(
            "Any other Ancilo operation (models, hardware, evals, settings, …). Pass the operation name and its input. Operations with consequences (large downloads, deleting, changing other tools' configuration) need \"confirm\": true – ask the user first. Use {{\"operation\": \"help\"}} for descriptions. Operations: {}",
            names.join(", ")
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "operation": {"type": "string", "description": "Operation name, or \"help\""},
                "input": {"type": "object", "description": "Input of the operation"},
                "confirm": {"type": "boolean", "description": "Confirm a consequential operation (after asking the user)"}
            },
            "required": ["operation"]
        }
    }));
    out
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool_result(value: Result<Value, ancilo_core::Error>) -> Value {
    match value {
        Ok(v) => {
            let text = serde_json::to_string_pretty(&v).unwrap_or_default();
            let mut r = json!({"content": [{"type": "text", "text": text}], "isError": false});
            if v.is_object() {
                r["structuredContent"] = v;
            }
            r
        }
        Err(e) => {
            json!({"content": [{"type": "text", "text": format!("{} ({})", e.message(), e.code())}], "isError": true})
        }
    }
}

async fn call_tool(state: &AppState, name: &str, args: Value) -> Value {
    let ctx = OpCtx::new(Surface::Mcp);
    if let Some((op, _)) = DIRECT_TOOLS.iter().find(|(_, t)| *t == name) {
        return tool_result(state.registry.call(op, ctx, args).await);
    }
    if name == "ancilo" {
        let op = args["operation"].as_str().unwrap_or_default();
        if op == "help" || op.is_empty() {
            let help: Vec<Value> = state
                .registry
                .specs()
                .map(|s| json!({"operation": s.name, "summary": s.summary, "consequential": s.consequential, "input": schema(s)}))
                .collect();
            return tool_result(Ok(Value::Array(help)));
        }
        let confirmed = args["confirm"].as_bool().unwrap_or(false);
        let input = if args["input"].is_null() {
            json!({})
        } else {
            args["input"].clone()
        };
        return tool_result(
            state
                .registry
                .call(op, ctx.confirmed(confirmed), input)
                .await,
        );
    }
    tool_result(Err(ancilo_core::Error::not_found(format!(
        "unknown tool '{name}'"
    ))))
}

async fn dispatch(state: &AppState, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or_default();
    // Notifications (no id) get no answer.
    let id = id?;
    let params = &msg["params"];
    Some(match method {
        "initialize" => {
            let requested = params["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            let version = if PROTOCOL_VERSIONS.contains(&requested) {
                requested
            } else {
                PROTOCOL_VERSIONS[0]
            };
            rpc_result(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "ancilo", "version": state.version},
                    "instructions": INSTRUCTIONS
                }),
            )
        }
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({"tools": tools(state)})),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let args = if params["arguments"].is_null() {
                json!({})
            } else {
                params["arguments"].clone()
            };
            rpc_result(&id, call_tool(state, name, args).await)
        }
        "resources/list" => rpc_result(&id, json!({"resources": []})),
        "prompts/list" => rpc_result(&id, json!({"prompts": []})),
        other => rpc_error(&id, -32601, &format!("method not found: {other}")),
    })
}

async fn handle(State(state): State<AppState>, body: String) -> Response {
    let parsed: Result<Value, _> = serde_json::from_str(&body);
    let msg = match parsed {
        Ok(v) => v,
        Err(e) => {
            return axum::Json(rpc_error(
                &Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ))
            .into_response();
        }
    };
    if let Value::Array(batch) = msg {
        let mut out = Vec::new();
        for m in batch {
            if let Some(r) = dispatch(&state, m).await {
                out.push(r);
            }
        }
        return if out.is_empty() {
            StatusCode::ACCEPTED.into_response()
        } else {
            axum::Json(Value::Array(out)).into_response()
        };
    }
    match dispatch(&state, msg).await {
        Some(r) => axum::Json(r).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    use ancilo_core::{EventBus, NoInput, OpBuilder, Registry};
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(Deserialize, JsonSchema)]
    struct Task {
        task: String,
    }

    async fn serve() -> (String, String) {
        let mut r = Registry::new();
        r.register(
            OpBuilder::new("delegate")
                .summary("Delegate")
                .description("Hand a task to the local model")
                .manage()
                .handler(|_c, i: Task| async move {
                    Ok(serde_json::json!({"status": "done", "summary": format!("did {}", i.task)}))
                }),
        );
        r.register(
            OpBuilder::new("list_models")
                .summary("List")
                .handler(|_c, _i: NoInput| async move { Ok(vec!["m1"]) }),
        );
        r.register(
            OpBuilder::new("remove_model")
                .summary("Remove")
                .manage()
                .consequential()
                .handler(|_c, _i: NoInput| async move { Ok("removed") }),
        );
        let db = ancilo_storage::Db::in_memory().unwrap();
        let state = AppState {
            registry: Arc::new(r),
            token: Arc::new("tok".into()),
            bus: EventBus::in_memory(),
            db,
            started: Instant::now(),
            version: "t",
            shutdown: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, ancilo_server::router(state, router()))
                .await
                .unwrap()
        });
        (url, "tok".into())
    }

    async fn rpc(url: &str, token: &str, body: Value) -> (u16, Value) {
        let r = reqwest::Client::new()
            .post(format!("{url}/mcp"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    // covers: M3-AC-08
    #[tokio::test]
    async fn speaks_mcp_and_exposes_every_operation() {
        let (url, tok) = serve().await;
        let (_, init) = rpc(&url, &tok, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).await;
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "ancilo");
        let (s, _) = rpc(
            &url,
            &tok,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(s, 202);
        let (_, list) = rpc(
            &url,
            &tok,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
        let tools = list["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["delegate", "models", "ancilo"]);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(!t["description"].as_str().unwrap().is_empty());
        }
        assert!(
            tools[2]["description"]
                .as_str()
                .unwrap()
                .contains("remove_model")
        );
        // Direct tool.
        let (_, r) = rpc(&url, &tok, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "delegate", "arguments": {"task": "x"}}})).await;
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["structuredContent"]["summary"], "did x");
        // Generic tool: consequential operations need confirmation.
        let (_, r) = rpc(&url, &tok, json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "ancilo", "arguments": {"operation": "remove_model"}}})).await;
        assert_eq!(r["result"]["isError"], true);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("confirmation_required")
        );
        let (_, r) = rpc(&url, &tok, json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "ancilo", "arguments": {"operation": "remove_model", "confirm": true}}})).await;
        assert_eq!(r["result"]["isError"], false);
        let (_, r) = rpc(&url, &tok, json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "ancilo", "arguments": {"operation": "help"}}})).await;
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("list_models")
        );
        // Invalid input is a tool error, not a protocol error.
        let (_, r) = rpc(&url, &tok, json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "delegate", "arguments": {}}})).await;
        assert_eq!(r["result"]["isError"], true);
        let (_, r) = rpc(
            &url,
            &tok,
            json!({"jsonrpc": "2.0", "id": 8, "method": "nope"}),
        )
        .await;
        assert_eq!(r["error"]["code"], -32601);
        // Without the token: rejected.
        let r = reqwest::Client::new()
            .post(format!("{url}/mcp"))
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 401);
    }
}
