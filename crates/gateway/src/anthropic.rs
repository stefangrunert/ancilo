//! Anthropic Messages API ⇄ OpenAI chat completions.
//!
//! Internally everything is OpenAI format; this module translates requests,
//! responses and streams so Anthropic clients (Claude Code) work unchanged.

use serde_json::{Map, Value, json};

use ancilo_core::{Error, Result};

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Anthropic request → OpenAI request.
pub fn request_to_openai(req: &Value) -> Result<Value> {
    let mut messages = Vec::new();
    if let Some(system) = req.get("system").filter(|s| !s.is_null()) {
        let text = text_of(system);
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    for m in req["messages"]
        .as_array()
        .ok_or_else(|| Error::invalid("messages must be an array"))?
    {
        let role = m["role"].as_str().unwrap_or("user");
        match &m["content"] {
            Value::String(s) => messages.push(json!({"role": role, "content": s})),
            Value::Array(blocks) => {
                let mut text_parts: Vec<Value> = Vec::new();
                let mut tool_calls = Vec::new();
                let mut tool_results = Vec::new();
                for b in blocks {
                    match b["type"].as_str() {
                        Some("text") => text_parts.push(json!({"type": "text", "text": b["text"]})),
                        Some("image") => {
                            let src = &b["source"];
                            if src["type"] == "base64" {
                                let url = format!(
                                    "data:{};base64,{}",
                                    src["media_type"].as_str().unwrap_or("image/png"),
                                    src["data"].as_str().unwrap_or("")
                                );
                                text_parts
                                    .push(json!({"type": "image_url", "image_url": {"url": url}}));
                            }
                        }
                        Some("tool_use") => tool_calls.push(json!({
                            "id": b["id"], "type": "function",
                            "function": {"name": b["name"], "arguments": b["input"].to_string()}
                        })),
                        Some("tool_result") => {
                            let mut content = text_of(&b["content"]);
                            if b["is_error"].as_bool() == Some(true) {
                                content = format!("Error: {content}");
                            }
                            tool_results.push(json!({"role": "tool", "tool_call_id": b["tool_use_id"], "content": content}));
                        }
                        // thinking / redacted_thinking / documents: not forwarded.
                        _ => {}
                    }
                }
                // Tool results answer the previous assistant turn: they come first.
                messages.extend(tool_results);
                if role == "assistant" {
                    let text: String = text_parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
                    if !tool_calls.is_empty() {
                        msg["tool_calls"] = Value::Array(tool_calls);
                    }
                    messages.push(msg);
                } else if !text_parts.is_empty() {
                    let only_text = text_parts.iter().all(|p| p["type"] == "text");
                    let content = if only_text {
                        json!(
                            text_parts
                                .iter()
                                .filter_map(|p| p["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    } else {
                        Value::Array(text_parts)
                    };
                    messages.push(json!({"role": role, "content": content}));
                }
            }
            _ => {}
        }
    }
    let mut out = Map::new();
    out.insert("model".into(), req["model"].clone());
    out.insert("messages".into(), Value::Array(messages));
    if let Some(n) = req["max_tokens"].as_u64() {
        out.insert("max_tokens".into(), json!(n));
    }
    for key in ["temperature", "top_p", "top_k"] {
        if !req[key].is_null() {
            out.insert(key.into(), req[key].clone());
        }
    }
    if let Some(stop) = req["stop_sequences"].as_array() {
        out.insert("stop".into(), Value::Array(stop.clone()));
    }
    if let Some(tools) = req["tools"].as_array().filter(|t| !t.is_empty()) {
        let tools: Vec<Value> = tools
            .iter()
            .filter(|t| t["name"].is_string())
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t["name"], "description": t["description"].as_str().unwrap_or(""),
                    "parameters": if t["input_schema"].is_null() { json!({"type": "object", "properties": {}}) } else { t["input_schema"].clone() }
                }})
            })
            .collect();
        if !tools.is_empty() {
            out.insert("tools".into(), Value::Array(tools));
        }
    }
    match req["tool_choice"]["type"].as_str() {
        Some("any") => {
            out.insert("tool_choice".into(), json!("required"));
        }
        Some("tool") => {
            out.insert(
                "tool_choice".into(),
                json!({"type": "function", "function": {"name": req["tool_choice"]["name"]}}),
            );
        }
        Some("none") => {
            out.insert("tool_choice".into(), json!("none"));
        }
        _ => {}
    }
    Ok(Value::Object(out))
}

fn stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        Some("stop") | None => "end_turn",
        Some(_) => "end_turn",
    }
}

/// Content blocks of an Anthropic message from an OpenAI message.
fn blocks(message: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(t) = message["content"].as_str().filter(|t| !t.trim().is_empty()) {
        out.push(json!({"type": "text", "text": t}));
    }
    for c in message["tool_calls"].as_array().into_iter().flatten() {
        let args = &c["function"]["arguments"];
        let input = match args {
            Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            v if v.is_object() => v.clone(),
            _ => json!({}),
        };
        out.push(json!({
            "type": "tool_use",
            "id": c["id"].as_str().map(str::to_string).unwrap_or_else(|| format!("toolu_{}", uuid::Uuid::new_v4().simple())),
            "name": c["function"]["name"], "input": input
        }));
    }
    out
}

/// OpenAI response → Anthropic response.
pub fn response_from_openai(resp: &Value, model: &str) -> Value {
    let choice = &resp["choices"][0];
    let message = &choice["message"];
    let mut finish = choice["finish_reason"].as_str();
    let content = blocks(message);
    if content.iter().any(|b| b["type"] == "tool_use") {
        finish = Some("tool_calls");
    }
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason(finish),
        "stop_sequence": null,
        "usage": {
            "input_tokens": resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            "output_tokens": resp["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        }
    })
}

/// A complete Anthropic message as a stream of SSE events.
pub fn stream_events(message: &Value) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    let mut start = message.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["usage"]["output_tokens"] = json!(0);
    events.push((
        "message_start",
        json!({"type": "message_start", "message": start}),
    ));
    for (i, b) in message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        match b["type"].as_str() {
            Some("text") => {
                events.push(("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})));
                events.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b["text"]}})));
            }
            Some("tool_use") => {
                events.push(("content_block_start", json!({"type": "content_block_start", "index": i,
                    "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}})));
                events.push((
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": i,
                    "delta": {"type": "input_json_delta", "partial_json": b["input"].to_string()}}),
                ));
            }
            _ => continue,
        }
        events.push((
            "content_block_stop",
            json!({"type": "content_block_stop", "index": i}),
        ));
    }
    events.push((
        "message_delta",
        json!({"type": "message_delta",
        "delta": {"stop_reason": message["stop_reason"], "stop_sequence": null},
        "usage": {"output_tokens": message["usage"]["output_tokens"]}}),
    ));
    events.push(("message_stop", json!({"type": "message_stop"})));
    events
}

/// Converts an OpenAI chat-completion stream into Anthropic events,
/// chunk by chunk (used for requests without tools).
pub struct StreamConverter {
    model: String,
    started: bool,
    text_open: Option<usize>,
    next_index: usize,
    tools: Vec<(usize, String)>,
    output_tokens: u64,
    input_tokens: u64,
    finish: Option<String>,
}

impl StreamConverter {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.into(),
            started: false,
            text_open: None,
            next_index: 0,
            tools: Vec::new(),
            output_tokens: 0,
            input_tokens: 0,
            finish: None,
        }
    }

    fn start(&mut self, out: &mut Vec<(&'static str, Value)>) {
        if !self.started {
            self.started = true;
            out.push(("message_start", json!({"type": "message_start", "message": {
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()), "type": "message", "role": "assistant",
                "model": self.model, "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": self.input_tokens, "output_tokens": 0}}})));
        }
    }

    /// Feeds one OpenAI chunk.
    pub fn chunk(&mut self, chunk: &Value) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        self.start(&mut out);
        if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
            self.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(self.input_tokens);
            self.output_tokens = u["completion_tokens"]
                .as_u64()
                .unwrap_or(self.output_tokens);
        }
        let choice = &chunk["choices"][0];
        let delta = &choice["delta"];
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            let idx = match self.text_open {
                Some(i) => i,
                None => {
                    let i = self.next_index;
                    self.next_index += 1;
                    self.text_open = Some(i);
                    out.push(("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})));
                    i
                }
            };
            out.push(("content_block_delta", json!({"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": t}})));
        }
        for tc in delta["tool_calls"].as_array().into_iter().flatten() {
            let key = tc["index"]
                .as_u64()
                .map(|i| i.to_string())
                .or_else(|| tc["id"].as_str().map(str::to_string))
                .unwrap_or_default();
            let idx = match self.tools.iter().find(|(_, k)| *k == key) {
                Some((i, _)) => *i,
                None => {
                    if let Some(open) = self.text_open.take() {
                        out.push((
                            "content_block_stop",
                            json!({"type": "content_block_stop", "index": open}),
                        ));
                    }
                    let i = self.next_index;
                    self.next_index += 1;
                    self.tools.push((i, key));
                    out.push(("content_block_start", json!({"type": "content_block_start", "index": i,
                        "content_block": {"type": "tool_use", "id": tc["id"].as_str().map(str::to_string).unwrap_or_else(|| format!("toolu_{i}")), "name": tc["function"]["name"], "input": {}}})));
                    i
                }
            };
            if let Some(args) = tc["function"]["arguments"]
                .as_str()
                .filter(|a| !a.is_empty())
            {
                out.push(("content_block_delta", json!({"type": "content_block_delta", "index": idx, "delta": {"type": "input_json_delta", "partial_json": args}})));
            }
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
        }
        out
    }

    /// Closes open blocks and ends the message.
    pub fn finish(&mut self) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        self.start(&mut out);
        if let Some(i) = self.text_open.take() {
            out.push((
                "content_block_stop",
                json!({"type": "content_block_stop", "index": i}),
            ));
        }
        for (i, _) in self.tools.drain(..) {
            out.push((
                "content_block_stop",
                json!({"type": "content_block_stop", "index": i}),
            ));
        }
        let reason = if self.next_index > 0 && self.finish.as_deref() == Some("tool_calls") {
            "tool_use"
        } else {
            stop_reason(self.finish.as_deref())
        };
        out.push(("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": reason, "stop_sequence": null}, "usage": {"output_tokens": self.output_tokens}})));
        out.push(("message_stop", json!({"type": "message_stop"})));
        out
    }
}

/// Rough token estimate for `count_tokens` (≈ 4 characters per token).
pub fn estimate_tokens(req: &Value) -> u64 {
    let mut chars = text_of(&req["system"]).len();
    for m in req["messages"].as_array().into_iter().flatten() {
        chars += match &m["content"] {
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        };
    }
    chars += req["tools"].to_string().len();
    (chars as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_a_full_tool_conversation() {
        let req = json!({
            "model": "claude-sonnet-4-6", "max_tokens": 1024,
            "system": [{"type": "text", "text": "You are helpful."}],
            "tools": [{"name": "read_file", "description": "Read", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}}],
            "tool_choice": {"type": "any"},
            "stop_sequences": ["END"],
            "messages": [
                {"role": "user", "content": "Read main.rs"},
                {"role": "assistant", "content": [{"type": "text", "text": "Reading."}, {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "main.rs"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "fn main() {}"}]}, {"type": "text", "text": "Explain it."}]}
            ]
        });
        let o = request_to_openai(&req).unwrap();
        let m = o["messages"].as_array().unwrap();
        assert_eq!(
            m[0],
            json!({"role": "system", "content": "You are helpful."})
        );
        assert_eq!(m[1], json!({"role": "user", "content": "Read main.rs"}));
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            serde_json::from_str::<Value>(
                m[2]["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            json!({"path": "main.rs"})
        );
        assert_eq!(
            m[3],
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": "fn main() {}"})
        );
        assert_eq!(m[4], json!({"role": "user", "content": "Explain it."}));
        assert_eq!(o["tool_choice"], "required");
        assert_eq!(
            o["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(o["stop"], json!(["END"]));
        assert_eq!(o["max_tokens"], 1024);
    }

    #[test]
    fn translates_responses_with_tool_use() {
        let resp = json!({"choices": [{"message": {"role": "assistant", "content": "Let me look.",
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "glob", "arguments": "{\"pattern\":\"*.rs\"}"}}]},
            "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 12, "completion_tokens": 7}});
        let a = response_from_openai(&resp, "local");
        assert_eq!(a["type"], "message");
        assert_eq!(a["stop_reason"], "tool_use");
        assert_eq!(
            a["content"][0],
            json!({"type": "text", "text": "Let me look."})
        );
        assert_eq!(a["content"][1]["input"], json!({"pattern": "*.rs"}));
        assert_eq!(a["usage"], json!({"input_tokens": 12, "output_tokens": 7}));
        let events: Vec<&str> = stream_events(&a).iter().map(|e| e.0).collect();
        assert_eq!(
            events,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
    }

    #[test]
    fn converts_openai_chunks_to_anthropic_events() {
        let mut c = StreamConverter::new("m");
        let mut events = Vec::new();
        for text in ["Hel", "lo"] {
            events.extend(
                c.chunk(&json!({"choices": [{"delta": {"content": text}, "finish_reason": null}]})),
            );
        }
        events.extend(c.chunk(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 2}})));
        events.extend(c.finish());
        let kinds: Vec<&str> = events.iter().map(|e| e.0).collect();
        assert_eq!(
            kinds,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        let text: String = events
            .iter()
            .filter(|e| e.0 == "content_block_delta")
            .map(|e| e.1["delta"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(text, "Hello");
        assert_eq!(events[5].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(events[5].1["usage"]["output_tokens"], 2);
    }
}
