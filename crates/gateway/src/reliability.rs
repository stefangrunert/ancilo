//! The reliability pipeline: makes tool calls of local models dependable.
//!
//! Six stages, each switchable so evals can measure its contribution:
//!
//! 1. **prompt** – tighten tool definitions, add a short tool-use instruction
//! 2. **constrained** – when a call failed, ask again with `tool_choice:
//!    required`, which makes llama.cpp enforce the tool-call grammar
//! 3. **validate** – check tool names and arguments against the JSON schemas
//! 4. **repair** – fix broken JSON, near-miss tool names, and tool calls the
//!    model wrote as plain text
//! 5. **retry** – ask again with precise feedback about what was wrong
//! 6. **nudge** – re-ask once when the model dodged an action on the project
//!
//! Everything here is pure; the gateway drives it.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Prompt,
    Constrained,
    Validate,
    Repair,
    Retry,
    /// The model answered in text although the request needs an action (or
    /// wrongly claimed it has no suitable tool): ask once more.
    Nudge,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Prompt,
        Stage::Constrained,
        Stage::Validate,
        Stage::Repair,
        Stage::Retry,
        Stage::Nudge,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "prompt" => Some(Stage::Prompt),
            "constrained" => Some(Stage::Constrained),
            "validate" => Some(Stage::Validate),
            "repair" => Some(Stage::Repair),
            "retry" => Some(Stage::Retry),
            "nudge" => Some(Stage::Nudge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReliabilityConfig {
    pub stages: BTreeSet<Stage>,
    /// Retries with feedback (stage `retry`).
    pub max_retries: u32,
}

/// Without per-model tuning: every stage except the prompt hint. Measured on
/// Qwen3-0.6B and Qwen3.5-4B, never worse than no pipeline on either the
/// tool-calling or the no-tool set; the hint helps some models and hurts
/// others (decision 2026-09-30-m2-reliability) – `tune_reliability` decides.
impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self::standard()
    }
}

impl ReliabilityConfig {
    pub fn all() -> Self {
        Self {
            stages: Stage::ALL.into_iter().collect(),
            max_retries: 2,
        }
    }

    /// All stages except `prompt`.
    pub fn standard() -> Self {
        let mut c = Self::all();
        c.stages.remove(&Stage::Prompt);
        c
    }

    pub fn off() -> Self {
        Self {
            stages: BTreeSet::new(),
            max_retries: 0,
        }
    }

    pub fn has(&self, s: Stage) -> bool {
        self.stages.contains(&s)
    }

    pub fn is_off(&self) -> bool {
        self.stages.is_empty()
    }

    /// The inverse of [`parse`](Self::parse): `off`, `all` or a list.
    pub fn spec(&self) -> String {
        if self.is_off() {
            "off".into()
        } else if self.stages.len() == Stage::ALL.len() {
            "all".into()
        } else {
            self.stages
                .iter()
                .map(|s| {
                    serde_json::to_value(s)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(",")
        }
    }

    /// `off`, `all`, or a comma-separated list of stages.
    pub fn parse(spec: &str) -> Result<Self, String> {
        match spec.trim() {
            "off" | "none" => Ok(Self::off()),
            "all" | "on" => Ok(Self::all()),
            "standard" => Ok(Self::standard()),
            list => {
                let mut stages = BTreeSet::new();
                for s in list.split(',').filter(|s| !s.trim().is_empty()) {
                    stages.insert(
                        Stage::parse(s)
                            .ok_or_else(|| format!("unknown reliability stage '{s}'"))?,
                    );
                }
                Ok(Self {
                    stages,
                    max_retries: 2,
                })
            }
        }
    }
}

/// A tool call after analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Valid tool calls (possibly after repair).
    Calls(Vec<ToolCall>),
    /// A plain answer without tool calls.
    Text,
    /// Tool calls (or attempts) that could not be made valid.
    Invalid(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub verdict: Verdict,
    /// Interventions, e.g. `repair:json`, `repair:name`, `repair:extract`.
    pub applied: Vec<String>,
    /// Remaining text content (tool-call JSON extracted from it removed).
    pub content: Option<String>,
}

// ---- stage 1: prompt ------------------------------------------------------

pub const TOOL_HINT: &str = "You work inside the user's project and can act on it with the listed tools. Use a tool when the request needs something from the project or a change to it – reading, searching or listing files, editing or creating files, running commands – and then do it with a tool call instead of describing or printing the result. Answer directly, without any tool, when the request needs nothing from the project: general questions, explanations, examples, text or code you write into the chat, calculations, translations. Use exact tool names and JSON arguments that match the schemas, including every required argument and the optional ones the request implies (such as line ranges).";

fn strip_schema(v: &mut Value) {
    if let Value::Object(map) = v {
        map.remove("$schema");
        map.remove("strict");
        for child in map.values_mut() {
            strip_schema(child);
        }
    } else if let Value::Array(items) = v {
        items.iter_mut().for_each(strip_schema);
    }
}

/// Tightens tool definitions and adds a short instruction to the system prompt.
pub fn adapt_prompt(req: &mut Value) {
    if let Some(tools) = req["tools"].as_array_mut() {
        for t in tools.iter_mut() {
            let f = &mut t["function"];
            if let Some(d) = f["description"].as_str()
                && d.chars().count() > 1024
            {
                let short: String = d.chars().take(1000).collect();
                f["description"] = json!(format!("{short}…"));
            }
            strip_schema(&mut f["parameters"]);
            if f["parameters"].is_null() {
                f["parameters"] = json!({"type": "object", "properties": {}});
            }
        }
    }
    let Some(messages) = req["messages"].as_array_mut() else {
        return;
    };
    match messages.first_mut() {
        Some(first) if first["role"] == "system" => {
            let existing = first["content"].as_str().unwrap_or_default().to_string();
            if !existing.contains(TOOL_HINT) {
                first["content"] = json!(format!("{existing}\n\n{TOOL_HINT}").trim().to_string());
            }
        }
        _ => messages.insert(0, json!({"role": "system", "content": TOOL_HINT})),
    }
}

// ---- stage 4: repair ------------------------------------------------------

/// Tries hard to read `s` as JSON: code fences, trailing commas, single quotes,
/// unclosed strings, brackets and braces.
pub fn repair_json(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if let Ok(v) = serde_json::from_str(trimmed) {
        return Some(v);
    }
    let mut t = trimmed.to_string();
    if let Some(rest) = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")) {
        t = rest.trim_end().trim_end_matches("```").trim().to_string();
    }
    if !t.contains('"') && t.contains('\'') {
        t = t.replace('\'', "\"");
    }
    // Remove trailing commas before closers.
    let mut cleaned = String::with_capacity(t.len());
    let chars: Vec<char> = t.chars().collect();
    let mut in_str = false;
    let mut escape = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            cleaned.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}' | ']') | None) {
                continue;
            }
        }
        cleaned.push(c);
    }
    // Close what is open.
    let mut stack = Vec::new();
    let (mut in_str, mut escape) = (false, false);
    for c in cleaned.chars() {
        if in_str {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut fixed = cleaned.trim_end().to_string();
    if in_str {
        fixed.push('"');
    }
    if fixed.ends_with(':') {
        fixed.push_str("null");
    }
    if fixed.ends_with(',') {
        fixed.pop();
    }
    while let Some(c) = stack.pop() {
        fixed.push(c);
    }
    serde_json::from_str(&fixed).ok()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The tool a near-miss name was meant to be, if unambiguous.
pub fn closest_tool(name: &str, tools: &[String]) -> Option<String> {
    let norm = |s: &str| s.to_ascii_lowercase().replace(['-', ' '], "_");
    let n = norm(
        name.trim()
            .trim_start_matches("functions.")
            .trim_start_matches("tools."),
    );
    if let Some(t) = tools.iter().find(|t| norm(t) == n) {
        return Some(t.clone());
    }
    let mut scored: Vec<(usize, &String)> = tools
        .iter()
        .map(|t| (levenshtein(&norm(t), &n), t))
        .collect();
    scored.sort();
    match scored.as_slice() {
        [(d, t), rest @ ..] => {
            let limit = (n.chars().count() / 3).max(2);
            let unique = rest.first().is_none_or(|(d2, _)| d2 > d);
            (*d <= limit && unique).then(|| (*t).clone())
        }
        [] => None,
    }
}

/// Balanced `{…}` spans in text (outside of JSON strings).
fn json_spans(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let (mut depth, mut in_str, mut esc) = (0i32, false, false);
            let mut end = None;
            for (j, &b) in bytes.iter().enumerate().skip(i) {
                if in_str {
                    if esc {
                        esc = false;
                    } else if b == b'\\' {
                        esc = true;
                    } else if b == b'"' {
                        in_str = false;
                    }
                    continue;
                }
                match b {
                    b'"' => in_str = true,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(e) => {
                    spans.push(&text[i..=e]);
                    i = e + 1;
                }
                None => {
                    // Unclosed: the rest might still be a (truncated) call.
                    spans.push(&text[i..]);
                    break;
                }
            }
        } else {
            i += 1;
        }
    }
    spans
}

/// Tool calls written as text: `<tool_call>{…}</tool_call>`, fenced JSON, or
/// bare `{"name": …, "arguments": …}`. Returns calls and the remaining text.
pub fn extract_text_calls(text: &str, tools: &[String]) -> (Vec<(String, Value)>, String) {
    let mut calls = Vec::new();
    let mut remaining = text.to_string();
    for span in json_spans(text) {
        let Some(v) = repair_json(span) else { continue };
        let obj = if v.get("function").is_some_and(Value::is_object) {
            &v["function"]
        } else {
            &v
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            continue;
        };
        let args = obj
            .get("arguments")
            .or_else(|| obj.get("parameters"))
            .or_else(|| obj.get("input"))
            .cloned()
            .unwrap_or(json!({}));
        let args = match args {
            Value::String(s) => repair_json(&s).unwrap_or(Value::String(s)),
            other => other,
        };
        let known = tools.iter().any(|t| t == name) || closest_tool(name, tools).is_some();
        if known {
            calls.push((name.to_string(), args));
            remaining = remaining.replacen(span, "", 1);
        }
    }
    for tag in ["<tool_call>", "</tool_call>", "```json", "```"] {
        remaining = remaining.replace(tag, "");
    }
    (calls, remaining.trim().to_string())
}

// ---- stage 3: validate ----------------------------------------------------

fn tool_schema<'a>(tools: &'a [Value], name: &str) -> Option<&'a Value> {
    tools
        .iter()
        .find(|t| t["function"]["name"] == name)
        .map(|t| &t["function"]["parameters"])
}

pub fn validate_args(args: &Value, schema: &Value) -> Result<(), String> {
    if !args.is_object() {
        return Err("arguments must be a JSON object".into());
    }
    if schema.is_null() {
        return Ok(());
    }
    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("invalid tool schema: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(args)
        .take(3)
        .map(|e| e.to_string())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

// ---- stage 6: nudge -------------------------------------------------------
//
// Small models often *dodge* a needed action: they answer in text, print a
// file instead of writing it, or claim a tool is missing. The nudge asks once
// more. To avoid false positives, a request counts as an action only if it
// refers to the project (a path, files, tests, the repository, …) – a verb
// alone is not enough – and generic/example requests never count.
// A tool call is *forced* only for read-only intents; for changing intents
// the model is merely asked again, so a false positive can never cause a
// write or a shell command on its own.

/// What a person's request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Nothing on the project (explanations, general questions, chat).
    None,
    /// Look at the project (read, list, search, status).
    Read,
    /// Change or run something in the project.
    Act,
}

const READ_VERBS: &[&str] = &[
    "show",
    "read",
    "open",
    "display",
    "view",
    "list",
    "find",
    "search",
    "grep",
    "look",
    "check",
    "cat",
    "inspect",
    "zeig",
    "zeige",
    "lies",
    "öffne",
    "liste",
    "finde",
    "suche",
    "durchsuche",
    "prüfe",
    "schau",
];
const ACT_VERBS: &[&str] = &[
    "create",
    "write",
    "add",
    "edit",
    "change",
    "modify",
    "rename",
    "replace",
    "fix",
    "delete",
    "remove",
    "run",
    "execute",
    "build",
    "install",
    "commit",
    "push",
    "refactor",
    "implement",
    "move",
    "format",
    "update",
    "erstelle",
    "schreib",
    "schreibe",
    "füge",
    "ändere",
    "bearbeite",
    "benenne",
    "ersetze",
    "behebe",
    "lösche",
    "entferne",
    "führe",
    "starte",
    "baue",
    "installiere",
    "committe",
    "aktualisiere",
    "implementiere",
];
const ARTIFACTS: &[&str] = &[
    "file",
    "files",
    "folder",
    "directory",
    "repository",
    "repo",
    "branch",
    "commit",
    "codebase",
    "project",
    "function",
    "method",
    "class",
    "module",
    "test",
    "tests",
    "issue",
    "ticket",
    "config",
    "package",
    "datei",
    "dateien",
    "ordner",
    "verzeichnis",
    "projekt",
    "funktion",
    "methode",
    "klasse",
    "modul",
    "zweig",
];
const STATE_WORDS: &[&str] = &[
    "current",
    "currently",
    "status",
    "defined",
    "contain",
    "contains",
    "aktuell",
    "aktuelle",
    "aktuellen",
    "definiert",
    "enthält",
];
const STATE_PHRASES: &[&str] = &[
    "where is",
    "where are",
    "which",
    "what's in",
    "what is in",
    "are there",
    "wo ist",
    "wo sind",
    "welche",
    "was steht",
];
const GENERIC: &[&str] = &[
    "example",
    "typical",
    "typically",
    "usually",
    "in general",
    "generally",
    "in theory",
    "how do i",
    "how does",
    "how would",
    "how can i",
    "what is a ",
    "what are ",
    "explain",
    "difference",
    "beispiel",
    "typisch",
    "normalerweise",
    "allgemein",
    "wie funktioniert",
    "wie starte",
    "was ist ein",
    "erkläre",
    "unterschied",
];

/// Harness blocks embedded in user messages (Claude Code, Codex).
const HARNESS_TAGS: &[&str] = &[
    "system-reminder",
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-stderr",
    "user-prompt-submit-hook",
    "environment_context",
    "user_instructions",
];

fn has_path(text: &str) -> bool {
    text.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '"' | '\'' | '`' | ':')
    })
    .map(|w| w.trim_end_matches(['.', '?', '!']))
    .any(|w| {
        let letters = w.chars().filter(|c| c.is_ascii_alphabetic()).count();
        let path = w.contains('/') && letters >= 2 && !w.starts_with("http");
        let ext = w.rsplit_once('.').is_some_and(|(stem, ext)| {
            (1..=6).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
                && (stem.chars().any(|c| c.is_ascii_alphabetic())
                    || stem.is_empty() && ext.len() >= 3)
        });
        path || ext
    })
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Classifies a person's request (English and German heuristics).
pub fn intent(text: &str) -> Intent {
    let lower = format!(" {} ", text.to_lowercase());
    // Text inside backticks is provided material, not a reference to the project.
    let outside_code: String = text.split('`').step_by(2).collect::<Vec<_>>().join(" ");
    let w = words(&outside_code);
    if GENERIC.iter().any(|g| lower.contains(g)) {
        return Intent::None;
    }
    let artifact = has_path(&outside_code) || w.iter().any(|x| ARTIFACTS.contains(&x.as_str()));
    if !artifact {
        return Intent::None;
    }
    if w.iter().any(|x| ACT_VERBS.contains(&x.as_str())) {
        return Intent::Act;
    }
    let state = w.iter().any(|x| STATE_WORDS.contains(&x.as_str()))
        || STATE_PHRASES.iter().any(|p| lower.contains(p));
    if w.iter().any(|x| READ_VERBS.contains(&x.as_str())) || state {
        return Intent::Read;
    }
    Intent::None
}

const INABILITY: &[&str] = &[
    "don't have access",
    "do not have access",
    "don't have the",
    "do not have the",
    "don't have a tool",
    "no tool",
    "not have a tool",
    "cannot access",
    "can't access",
    "unable to access",
    "not able to access",
    "don't include",
    "do not include",
    "doesn't include",
    "i can't run",
    "i cannot run",
    "keinen zugriff",
    "kein werkzeug",
    "kann ich nicht",
];

/// Text of the last user message.
pub fn last_user_text(req: &Value) -> String {
    req["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| match &m["content"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

pub fn claims_inability(text: &str) -> bool {
    let t = text.to_lowercase();
    INABILITY.iter().any(|p| t.contains(p))
}

/// Removes known harness blocks (`<system-reminder>…</system-reminder>`, …)
/// from a user message. Other markup written by the person is kept.
pub fn human_text(text: &str) -> String {
    let mut out = text.to_string();
    for tag in HARNESS_TAGS {
        let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
        while let Some(start) = out.find(&open) {
            match out[start..].find(&close) {
                Some(end) => out.replace_range(start..start + end + close.len(), ""),
                None => break,
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nudge {
    /// Require a tool call on the second attempt (read-only intents only).
    pub force: bool,
}

/// Whether (and how) to re-ask after a text answer.
///
/// Only right after a person's request: when the last message is a tool
/// result, a text answer (a summary) is exactly what is expected.
pub fn nudge_decision(req: &Value, answer: &str) -> Option<Nudge> {
    let last_is_user = req["messages"]
        .as_array()
        .and_then(|m| m.last())
        .is_some_and(|m| m["role"] == "user");
    if !last_is_user {
        return None;
    }
    match intent(&human_text(&last_user_text(req))) {
        Intent::Read => Some(Nudge { force: true }),
        Intent::Act => Some(Nudge { force: false }),
        Intent::None if claims_inability(answer) => Some(Nudge { force: false }),
        Intent::None => None,
    }
}

pub fn should_nudge(req: &Value, answer: &str) -> bool {
    nudge_decision(req, answer).is_some()
}

/// A neutral second question – it must not assert that an action is needed.
pub fn nudge_message(tools: &[Value]) -> Value {
    json!({
        "role": "user",
        "content": format!(
            "Check your last answer. If the request needs an action on the project, do it now with the appropriate tool ({}); a shell tool can run any command. If it needs no action, give your answer again.",
            tool_names(tools).join(", ")
        )
    })
}

// ---- analysis ---------------------------------------------------------------

pub fn tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
        .collect()
}

/// Examines a model message against the offered tools.
pub fn analyze(message: &Value, tools: &[Value], cfg: &ReliabilityConfig) -> Analysis {
    let names = tool_names(tools);
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    let mut content = message["content"].as_str().map(str::to_string);
    let mut raw: Vec<(Option<String>, String, Value)> = message["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .map(|c| {
                    let args = c["function"]["arguments"].clone();
                    (
                        c["id"].as_str().map(str::to_string),
                        c["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        args,
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    if raw.is_empty() {
        if cfg.has(Stage::Repair)
            && let Some(text) = &content
        {
            let (found, rest) = extract_text_calls(text, &names);
            if !found.is_empty() {
                applied.push("repair:extract".to_string());
                raw = found.into_iter().map(|(n, a)| (None, n, a)).collect();
                content = (!rest.is_empty()).then_some(rest);
            }
        }
        if raw.is_empty() {
            // A text that *looks* like a failed tool call is not a final answer.
            let looks_like_call = content.as_deref().is_some_and(|t| {
                t.contains("<tool_call>")
                    || (t.contains("\"name\"")
                        && (t.contains("\"arguments\"") || t.contains("\"parameters\"")))
            });
            if looks_like_call && cfg.has(Stage::Validate) {
                return Analysis {
                    verdict: Verdict::Invalid(vec![
                        "the tool call was written as text and could not be read".into(),
                    ]),
                    applied,
                    content,
                };
            }
            return Analysis {
                verdict: Verdict::Text,
                applied,
                content,
            };
        }
    }

    let mut calls = Vec::new();
    for (i, (id, name, args)) in raw.into_iter().enumerate() {
        let args = match args {
            Value::String(s) => match serde_json::from_str::<Value>(&s) {
                Ok(v) => v,
                Err(e) => {
                    if cfg.has(Stage::Repair)
                        && let Some(v) = repair_json(&s)
                    {
                        applied.push("repair:json".into());
                        v
                    } else {
                        errors.push(format!(
                            "the arguments of '{name}' are not valid JSON ({e})"
                        ));
                        continue;
                    }
                }
            },
            Value::Null => json!({}),
            other => other,
        };
        let name = if names.contains(&name) {
            name
        } else if cfg.has(Stage::Repair)
            && let Some(fixed) = closest_tool(&name, &names)
        {
            applied.push("repair:name".into());
            fixed
        } else {
            errors.push(format!(
                "'{name}' is not an available tool (available: {})",
                names.join(", ")
            ));
            continue;
        };
        if cfg.has(Stage::Validate)
            && let Err(e) = validate_args(&args, tool_schema(tools, &name).unwrap_or(&Value::Null))
        {
            errors.push(format!("invalid arguments for '{name}': {e}"));
            continue;
        }
        calls.push(ToolCall {
            id: id.unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple())),
            name,
            arguments: args,
        });
        let _ = i;
    }
    let verdict = if errors.is_empty() && !calls.is_empty() {
        Verdict::Calls(calls)
    } else if errors.is_empty() {
        Verdict::Text
    } else {
        Verdict::Invalid(errors)
    };
    Analysis {
        verdict,
        applied,
        content,
    }
}

/// Feedback message for stage `retry`.
pub fn feedback(errors: &[String], tools: &[Value]) -> Value {
    json!({
        "role": "user",
        "content": format!(
            "Your previous tool call could not be used: {}. Call one of the available tools ({}) again, with valid JSON arguments that match its schema.",
            errors.join("; "),
            tool_names(tools).join(", ")
        )
    })
}

/// Rewrites an OpenAI message with the analysed tool calls.
pub fn message_with_calls(content: Option<String>, calls: &[ToolCall]) -> Value {
    let mut m = Map::new();
    m.insert("role".into(), json!("assistant"));
    m.insert("content".into(), content.map_or(Value::Null, Value::String));
    if !calls.is_empty() {
        m.insert(
            "tool_calls".into(),
            Value::Array(
                calls
                    .iter()
                    .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments.to_string()}}))
                    .collect(),
            ),
        );
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<Value> {
        vec![
            json!({"type": "function", "function": {"name": "read_file", "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "limit": {"type": "integer"}}, "required": ["path"]}}}),
            json!({"type": "function", "function": {"name": "glob", "description": "Find files",
                "parameters": {"type": "object", "properties": {"pattern": {"type": "string"}}, "required": ["pattern"]}}}),
        ]
    }

    fn call(name: &str, args: &str) -> Value {
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": name, "arguments": args}}]})
    }

    fn only(stages: &[Stage]) -> ReliabilityConfig {
        ReliabilityConfig {
            stages: stages.iter().copied().collect(),
            max_retries: 2,
        }
    }

    // covers: M2-AC-03
    #[test]
    fn repair_fixes_broken_json_arguments() {
        for (broken, expected) in [
            (r#"{"path": "src/main.rs""#, json!({"path": "src/main.rs"})),
            (r#"{"path": "a.rs",}"#, json!({"path": "a.rs"})),
            ("{'path': 'a.rs'}", json!({"path": "a.rs"})),
            (
                "```json\n{\"path\": \"a.rs\"}\n```",
                json!({"path": "a.rs"}),
            ),
            (
                r#"{"path": "a.rs", "limit": 10"#,
                json!({"path": "a.rs", "limit": 10}),
            ),
            (r#"{"path": "unterminated"#, json!({"path": "unterminated"})),
        ] {
            let a = analyze(
                &call("read_file", broken),
                &tools(),
                &only(&[Stage::Repair, Stage::Validate]),
            );
            assert_eq!(
                a.verdict,
                Verdict::Calls(vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: expected
                }]),
                "{broken}"
            );
            assert_eq!(a.applied, vec!["repair:json"]);
        }
        // Without the stage the same input is an error.
        let a = analyze(
            &call("read_file", r#"{"path": "a.rs""#),
            &tools(),
            &only(&[Stage::Validate]),
        );
        assert!(matches!(a.verdict, Verdict::Invalid(_)));
    }

    // covers: M2-AC-03
    #[test]
    fn repair_fixes_near_miss_tool_names() {
        for name in [
            "readFile",
            "read-file",
            "Read_File",
            "functions.read_file",
            "read_fil",
        ] {
            let a = analyze(
                &call(name, r#"{"path": "x"}"#),
                &tools(),
                &only(&[Stage::Repair, Stage::Validate]),
            );
            assert!(
                matches!(&a.verdict, Verdict::Calls(c) if c[0].name == "read_file"),
                "{name}: {:?}",
                a.verdict
            );
            assert_eq!(a.applied, vec!["repair:name"]);
        }
        let a = analyze(
            &call("delete_everything", "{}"),
            &tools(),
            &ReliabilityConfig::all(),
        );
        assert!(
            matches!(a.verdict, Verdict::Invalid(ref e) if e[0].contains("not an available tool"))
        );
    }

    // covers: M2-AC-03
    #[test]
    fn repair_extracts_calls_written_as_text() {
        let texts = [
            "<tool_call>\n{\"name\": \"glob\", \"arguments\": {\"pattern\": \"**/*.rs\"}}\n</tool_call>",
            "I'll search.\n```json\n{\"name\": \"glob\", \"arguments\": {\"pattern\": \"**/*.rs\"}}\n```",
            "{\"function\": {\"name\": \"glob\", \"arguments\": \"{\\\"pattern\\\": \\\"**/*.rs\\\"}\"}}",
            "{\"name\": \"glob\", \"parameters\": {\"pattern\": \"**/*.rs\"",
        ];
        for text in texts {
            let msg = json!({"role": "assistant", "content": text});
            let a = analyze(&msg, &tools(), &ReliabilityConfig::all());
            match &a.verdict {
                Verdict::Calls(c) => {
                    assert_eq!(c[0].name, "glob");
                    assert_eq!(c[0].arguments, json!({"pattern": "**/*.rs"}));
                }
                other => panic!("{text}: {other:?}"),
            }
            assert!(a.applied.contains(&"repair:extract".to_string()));
        }
        let a = analyze(
            &json!({"role": "assistant", "content": texts[1]}),
            &tools(),
            &ReliabilityConfig::all(),
        );
        assert_eq!(a.content.as_deref(), Some("I'll search."));
    }

    // covers: M2-AC-03
    #[test]
    fn validation_catches_schema_violations() {
        let a = analyze(
            &call("read_file", r#"{"limit": 5}"#),
            &tools(),
            &only(&[Stage::Validate]),
        );
        assert!(
            matches!(a.verdict, Verdict::Invalid(ref e) if e[0].contains("path")),
            "{:?}",
            a.verdict
        );
        let a = analyze(
            &call("read_file", r#"{"path": 42}"#),
            &tools(),
            &only(&[Stage::Validate]),
        );
        assert!(matches!(a.verdict, Verdict::Invalid(_)));
        // Without validation, schema violations pass through unnoticed.
        let a = analyze(&call("read_file", r#"{"limit": 5}"#), &tools(), &only(&[]));
        assert!(matches!(a.verdict, Verdict::Calls(_)));
    }

    #[test]
    fn plain_answers_stay_answers() {
        let a = analyze(
            &json!({"role": "assistant", "content": "The file contains 3 functions."}),
            &tools(),
            &ReliabilityConfig::all(),
        );
        assert_eq!(a.verdict, Verdict::Text);
        assert!(a.applied.is_empty());
        let a = analyze(
            &json!({"role": "assistant", "content": "Use {curly} braces in Rust."}),
            &tools(),
            &ReliabilityConfig::all(),
        );
        assert_eq!(a.verdict, Verdict::Text);
    }

    // covers: M2-AC-03
    #[test]
    fn prompt_stage_tightens_tools_and_adds_the_hint() {
        let mut req = json!({"messages": [{"role": "user", "content": "hi"}], "tools": [
            {"type": "function", "function": {"name": "x", "description": "d".repeat(3000), "parameters": {"$schema": "http://json-schema.org/draft-07/schema#", "type": "object", "strict": true}}},
            {"type": "function", "function": {"name": "y"}}
        ]});
        adapt_prompt(&mut req);
        assert_eq!(req["messages"][0]["role"], "system");
        assert!(
            req["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("tool call")
        );
        assert!(
            req["tools"][0]["function"]["description"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                < 1100
        );
        assert!(
            req["tools"][0]["function"]["parameters"]
                .get("$schema")
                .is_none()
        );
        assert_eq!(req["tools"][1]["function"]["parameters"]["type"], "object");
        // Idempotent.
        let before = req.clone();
        adapt_prompt(&mut req);
        assert_eq!(req, before);
    }

    // covers: M2-AC-03
    #[test]
    fn nudge_detects_answers_that_dodge_an_action() {
        let req = |text: &str| json!({"messages": [{"role": "user", "content": text}]});
        let d = |user: &str, answer: &str| nudge_decision(&req(user), answer);
        for (user, answer, expected) in [
            (
                "What is the current git status of the repository?",
                "I don't have access to a git tool.",
                Some(true),
            ),
            (
                "Show me the contents of src/main.rs.",
                "It has a main function.",
                Some(true),
            ),
            (
                "Show me that function.",
                "It is defined in loader.rs.",
                Some(true),
            ),
            (
                "Zeig mir die Datei main.rs",
                "Die Datei enthält …",
                Some(true),
            ),
            (
                "Write a .gitignore that ignores target/",
                "```\ntarget/\n```",
                Some(false),
            ),
            (
                "Run only the tests whose names contain parser",
                "Sure.",
                Some(false),
            ),
            (
                "In general terms, what is the difference between a mutex and a semaphore?",
                "A mutex …",
                None,
            ),
            (
                "Here is a function: `fn add(a: i32, b: i32) -> i32 { a + b }`. What does it return for 2 and 3?",
                "5",
                None,
            ),
            ("Thanks, that's all for now!", "You're welcome!", None),
            ("What is a repository?", "A repository is …", None),
            (
                "How do I run tests in a Rust project?",
                "Use cargo test.",
                None,
            ),
            (
                "Write an explanation of what a race condition is.",
                "A race condition …",
                None,
            ),
            (
                "Show an example of a Dockerfile for a Node app.",
                "FROM node",
                None,
            ),
            (
                "Create a short motivational sentence for my team.",
                "You can do it!",
                None,
            ),
            (
                "Rename these variables: `let a = 5; let b = a * 2;`",
                "let count = 5;",
                None,
            ),
        ] {
            assert_eq!(d(user, answer).map(|n| n.force), expected, "{user}");
        }
        assert!(claims_inability(
            "Sorry, I do not have access to the file system."
        ));
        // Harness-injected context does not count as the person's request …
        let wrapped = "<system-reminder>Check the current repository state in src/main.rs.</system-reminder>\nReply with exactly: hello";
        assert!(!should_nudge(&req(wrapped), "hello"));
        // … but markup written by the person is kept.
        assert_eq!(
            human_text("a <b>x</b> <system-reminder>y</system-reminder>c"),
            "a <b>x</b> c"
        );
        // After tool results, a text answer is a summary – no nudge.
        let after_tool = json!({"messages": [
            {"role": "user", "content": "Show me main.rs"},
            {"role": "assistant", "content": null, "tool_calls": [{"id": "1", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "1", "content": "fn main() {}"}
        ]});
        assert!(!should_nudge(&after_tool, "It prints nothing."));
    }

    fn suite(yaml: &str) -> Vec<(String, Value)> {
        let v: Value = serde_yaml::from_str(yaml).unwrap();
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["id"].as_str().unwrap().to_string(),
                    json!({"messages": t["messages"]}),
                )
            })
            .collect()
    }

    // covers: M2-AC-03
    /// No request of the counter-check set may trigger a nudge – whatever the
    /// model answers (the nudge must not depend on the answer to stay quiet).
    #[test]
    fn nudge_never_fires_on_the_no_tool_set() {
        let tasks = suite(include_str!("../../../evals/no-tool.yaml"));
        assert!(tasks.len() >= 50);
        for (id, req) in tasks {
            assert_eq!(nudge_decision(&req, "Here is my answer."), None, "{id}");
        }
    }

    /// Every task of the tool-calling set that needs an action is recognised.
    #[test]
    fn nudge_recognises_the_actions_of_the_tool_calling_set() {
        for (id, req) in suite(include_str!("../../../evals/tool-calling.yaml")) {
            let expected = !id.starts_with("no-tool");
            assert_eq!(
                nudge_decision(&req, "Here is my answer.").is_some(),
                expected,
                "{id}"
            );
        }
    }

    #[test]
    fn parses_stage_lists() {
        assert!(ReliabilityConfig::parse("off").unwrap().is_off());
        assert_eq!(
            ReliabilityConfig::parse("all").unwrap(),
            ReliabilityConfig::all()
        );
        let c = ReliabilityConfig::parse("repair,validate").unwrap();
        assert!(c.has(Stage::Repair) && c.has(Stage::Validate) && !c.has(Stage::Retry));
        assert!(ReliabilityConfig::parse("magic").is_err());
    }

    proptest::proptest! {
        #[test]
        fn repair_never_panics(s in ".{0,200}") {
            let _ = repair_json(&s);
            let _ = extract_text_calls(&s, &["glob".to_string()]);
        }
    }
}
