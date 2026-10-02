//! OpenAI Responses API ⇄ chat completions (Codex speaks only Responses).
//!
//! Requests are translated to chat completions (so the reliability pipeline
//! applies), answers back into Responses output items and – for streaming
//! clients – into the `response.*` event sequence.

use serde_json::{Value, json};

use ancilo_core::{Error, Result};

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Names of tools that were declared as `custom` (free-form input).
pub fn custom_tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "custom")
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect()
}

/// Responses request → chat-completions request.
pub fn request_to_chat(req: &Value) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instr) = req["instructions"].as_str().filter(|s| !s.is_empty()) {
        messages.push(json!({"role": "system", "content": instr}));
    }
    let items: Vec<Value> = match &req["input"] {
        Value::String(s) => vec![json!({"type": "message", "role": "user", "content": s})],
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        _ => return Err(Error::invalid("input must be a string or an array")),
    };
    for item in items {
        let kind = item["type"].as_str().unwrap_or("message");
        match kind {
            "message" => {
                let role = match item["role"].as_str().unwrap_or("user") {
                    "developer" | "system" => "system",
                    "assistant" => "assistant",
                    _ => "user",
                };
                let text = content_text(&item["content"]);
                // Consecutive system messages are merged (chat templates expect one).
                let only_system_so_far = messages.len() == 1;
                if role == "system"
                    && only_system_so_far
                    && let Some(first) = messages.first_mut()
                    && first["role"] == "system"
                {
                    let merged = format!("{}\n\n{}", first["content"].as_str().unwrap_or(""), text);
                    first["content"] = json!(merged);
                    continue;
                }
                messages.push(json!({"role": role, "content": text}));
            }
            "function_call" | "custom_tool_call" => {
                let args = if kind == "custom_tool_call" {
                    json!({"input": item["input"].as_str().unwrap_or("")}).to_string()
                } else {
                    item["arguments"].as_str().unwrap_or("{}").to_string()
                };
                let call = json!({"id": item["call_id"], "type": "function", "function": {"name": item["name"], "arguments": args}});
                // Consecutive calls belong to one assistant turn.
                match messages.last_mut() {
                    Some(last) if last["role"] == "assistant" && last["tool_calls"].is_array() => {
                        last["tool_calls"].as_array_mut().unwrap().push(call);
                    }
                    _ => messages
                        .push(json!({"role": "assistant", "content": null, "tool_calls": [call]})),
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let output = match &item["output"] {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                messages.push(
                    json!({"role": "tool", "tool_call_id": item["call_id"], "content": output}),
                );
            }
            // reasoning, web search results, …: not forwarded
            _ => {}
        }
    }
    let mut out = json!({"model": req["model"], "messages": messages});
    let tools: Vec<Value> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| match t["type"].as_str() {
            Some("function") => Some(json!({"type": "function", "function": {
                "name": t["name"], "description": t["description"].as_str().unwrap_or(""),
                "parameters": if t["parameters"].is_null() { json!({"type": "object", "properties": {}}) } else { t["parameters"].clone() }
            }})),
            Some("custom") => Some(json!({"type": "function", "function": {
                "name": t["name"],
                "description": format!("{} (Put the complete free-form input into the `input` argument.)", t["description"].as_str().unwrap_or("")),
                "parameters": {"type": "object", "properties": {"input": {"type": "string"}}, "required": ["input"]}
            }})),
            _ => None,
        })
        .collect();
    if !tools.is_empty() {
        out["tools"] = Value::Array(tools);
    }
    match &req["tool_choice"] {
        Value::String(s) if s == "required" || s == "none" => out["tool_choice"] = json!(s),
        Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("function") => {
            out["tool_choice"] = json!({"type": "function", "function": {"name": o.get("name")}});
        }
        _ => {}
    }
    for (from, to) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if !req[from].is_null() {
            out[to] = req[from].clone();
        }
    }
    Ok(out)
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Chat completion → Responses object.
pub fn response_from_chat(resp: &Value, model: &str, custom_tools: &[String]) -> Value {
    let message = &resp["choices"][0]["message"];
    let mut output = Vec::new();
    if let Some(r) = message["reasoning_content"]
        .as_str()
        .filter(|r| !r.is_empty())
    {
        output.push(json!({"type": "reasoning", "id": new_id("rs"), "summary": [{"type": "summary_text", "text": r}]}));
    }
    if let Some(t) = message["content"].as_str().filter(|t| !t.trim().is_empty()) {
        output.push(json!({
            "type": "message", "id": new_id("msg"), "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": t, "annotations": []}]
        }));
    }
    for c in message["tool_calls"].as_array().into_iter().flatten() {
        let name = c["function"]["name"].as_str().unwrap_or_default();
        let args = c["function"]["arguments"].as_str().unwrap_or("{}");
        let call_id = c["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| new_id("call"));
        if custom_tools.iter().any(|t| t == name) {
            let input = serde_json::from_str::<Value>(args)
                .ok()
                .and_then(|v| v["input"].as_str().map(str::to_string))
                .unwrap_or_else(|| args.to_string());
            output.push(json!({"type": "custom_tool_call", "id": new_id("ctc"), "call_id": call_id, "name": name, "input": input, "status": "completed"}));
        } else {
            output.push(json!({"type": "function_call", "id": new_id("fc"), "call_id": call_id, "name": name, "arguments": args, "status": "completed"}));
        }
    }
    let input_tokens = resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let output_tokens = resp["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    json!({
        "id": new_id("resp"),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": "completed",
        "model": model,
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": output_tokens,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": input_tokens + output_tokens
        }
    })
}

/// The `response.*` event sequence for a complete response.
pub fn stream_events(response: &Value) -> Vec<(String, Value)> {
    let mut events = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, mut data: Value| {
        data["type"] = json!(kind);
        data["sequence_number"] = json!(seq);
        seq += 1;
        events.push((kind.to_string(), data));
    };
    let mut shell = response.clone();
    shell["status"] = json!("in_progress");
    shell["output"] = json!([]);
    push("response.created", json!({"response": shell}));
    push("response.in_progress", json!({"response": shell}));
    for (i, item) in response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let mut added = item.clone();
        added["status"] = json!("in_progress");
        if item["type"] == "message" {
            added["content"] = json!([]);
        }
        if item["type"] == "function_call" {
            added["arguments"] = json!("");
        }
        push(
            "response.output_item.added",
            json!({"output_index": i, "item": added}),
        );
        match item["type"].as_str() {
            Some("message") => {
                let text = item["content"][0]["text"].clone();
                push(
                    "response.content_part.added",
                    json!({"item_id": item["id"], "output_index": i, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}),
                );
                push(
                    "response.output_text.delta",
                    json!({"item_id": item["id"], "output_index": i, "content_index": 0, "delta": text}),
                );
                push(
                    "response.output_text.done",
                    json!({"item_id": item["id"], "output_index": i, "content_index": 0, "text": text}),
                );
                push(
                    "response.content_part.done",
                    json!({"item_id": item["id"], "output_index": i, "content_index": 0, "part": item["content"][0]}),
                );
            }
            Some("function_call") => {
                push(
                    "response.function_call_arguments.delta",
                    json!({"item_id": item["id"], "output_index": i, "delta": item["arguments"]}),
                );
                push(
                    "response.function_call_arguments.done",
                    json!({"item_id": item["id"], "output_index": i, "arguments": item["arguments"]}),
                );
            }
            _ => {}
        }
        push(
            "response.output_item.done",
            json!({"output_index": i, "item": item}),
        );
    }
    push("response.completed", json!({"response": response}));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_a_codex_style_request() {
        let req = json!({
            "model": "default",
            "instructions": "You are Codex.",
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "Sandbox: read-only"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "list files"}]},
                {"type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"command\":[\"ls\"]}"},
                {"type": "function_call_output", "call_id": "c1", "output": "a.rs\nb.rs"},
                {"type": "reasoning", "summary": []},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "patch a.rs"}]}
            ],
            "tools": [
                {"type": "function", "name": "shell", "description": "Run", "parameters": {"type": "object", "properties": {"command": {"type": "array", "items": {"type": "string"}}}}},
                {"type": "custom", "name": "apply_patch", "description": "Apply a patch", "format": {"type": "grammar"}},
                {"type": "web_search"}
            ],
            "tool_choice": "auto",
            "stream": true
        });
        let chat = request_to_chat(&req).unwrap();
        let m = chat["messages"].as_array().unwrap();
        assert_eq!(
            m[0],
            json!({"role": "system", "content": "You are Codex.\n\nSandbox: read-only"})
        );
        assert_eq!(m[1], json!({"role": "user", "content": "list files"}));
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(
            m[3],
            json!({"role": "tool", "tool_call_id": "c1", "content": "a.rs\nb.rs"})
        );
        assert_eq!(m[4]["content"], "patch a.rs");
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[1]["function"]["parameters"]["required"],
            json!(["input"])
        );
        assert_eq!(custom_tool_names(&req), vec!["apply_patch"]);
    }

    #[test]
    fn builds_responses_and_the_event_stream() {
        let chat = json!({"choices": [{"message": {"role": "assistant", "content": "Done.", "tool_calls": [
            {"id": "c9", "type": "function", "function": {"name": "apply_patch", "arguments": "{\"input\": \"*** Begin Patch\"}"}},
            {"id": "c10", "type": "function", "function": {"name": "shell", "arguments": "{\"command\": [\"ls\"]}"}}
        ]}}], "usage": {"prompt_tokens": 10, "completion_tokens": 5}});
        let r = response_from_chat(&chat, "default", &["apply_patch".to_string()]);
        assert_eq!(r["output"][0]["type"], "message");
        assert_eq!(
            r["output"][1],
            json!({"type": "custom_tool_call", "id": r["output"][1]["id"], "call_id": "c9", "name": "apply_patch", "input": "*** Begin Patch", "status": "completed"})
        );
        assert_eq!(r["output"][2]["type"], "function_call");
        assert_eq!(r["usage"]["total_tokens"], 15);
        let kinds: Vec<String> = stream_events(&r).into_iter().map(|e| e.0).collect();
        assert_eq!(kinds.first().unwrap(), "response.created");
        assert_eq!(kinds.last().unwrap(), "response.completed");
        assert_eq!(
            kinds
                .iter()
                .filter(|k| *k == "response.output_item.done")
                .count(),
            3
        );
        assert!(kinds.contains(&"response.output_text.delta".to_string()));
    }
}
