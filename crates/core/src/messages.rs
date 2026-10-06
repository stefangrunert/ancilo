//! The messages people see – each with a key and its English template.
//!
//! Ancilo speaks English internally (logs, the API, the history a model
//! reads). The app shows the messages in the user's language: it knows these
//! templates (generated into `app/src/i18n/messages.gen.ts` by `just
//! app-api`), recognizes a message by its template, takes the values out of
//! the placeholders and shows its own translation of the same key. A value
//! may itself be such a message ("model call failed: {why}") – it is
//! translated too.
//!
//! So a message built from a template here never drifts from its
//! translation: change the English, and the German key stays what it means.

use std::fmt::Display;

/// Key and English template; placeholders are `{name}`.
pub const MESSAGES: &[(&str, &str)] = &[
    // A turn that failed, as kept in a conversation.
    ("failed", "(failed: {why})"),
    // Memory and models
    (
        "memory.short",
        "your computer is short of memory right now – close some programs, then try again",
    ),
    (
        "memory.need",
        "this model needs about {need} of memory, but only about {room} are free right now – close some programs or choose a smaller model",
    ),
    (
        "model.no_room",
        "'{model}' cannot be loaded right now: {why}",
    ),
    ("model.others_loaded", "{message} – loaded now: {models}"),
    (
        "model.over_budget",
        "'{model}' needs about {need}, but only {free} of {budget} are free. Stop another model first.",
    ),
    (
        "model.does_not_fit",
        "This model does not fit on this machine: {reason}",
    ),
    (
        "model.load_timeout",
        "timed out waiting for the model to load",
    ),
    (
        "model.load_too_long",
        "model did not finish loading within {time}",
    ),
    (
        "model.exited",
        "llama-server exited unexpectedly ({status}): {log}",
    ),
    ("model.no_answer_http", "the model did not answer: {why}"),
    ("model.rejected", "the model rejected the request: {why}"),
    (
        "model.failed_http",
        "the model failed (HTTP {status}): {why}",
    ),
    (
        "model.none_fits",
        "This computer does not have enough memory for any of Ancilo's AI models, so there is no AI to answer here yet. Set up › The AI for your computer shows what it would need.",
    ),
    ("agent.call_failed", "model call failed: {why}"),
    ("agent.no_answer", "the model ended without an answer"),
    (
        "agent.repeating",
        "the model kept repeating the same tool call",
    ),
    // Downloads
    (
        "download.no_space",
        "not enough disk space: the download needs {need}, {free} are free",
    ),
    (
        "download.failed",
        "download failed after {attempts} attempts: {reason}",
    ),
    ("download.cancelled", "download cancelled"),
    (
        "download.corrupt",
        "downloaded file is corrupt (checksum mismatch) and was discarded: {path}",
    ),
    ("hf.unreachable", "cannot reach Hugging Face: {why}"),
    ("hf.not_found", "{what} was not found on Hugging Face"),
    // Web search
    (
        "web.off",
        "web search is off – turn it on under System › Web search",
    ),
    ("web.too_long", "the web search took too long"),
    (
        "web.serper_key",
        "Serper needs a key – add it under System › Web search",
    ),
    (
        "web.unreachable",
        "the search service is not reachable – is this computer online? ({why})",
    ),
    (
        "web.serper_rejected",
        "Serper does not accept this key – copy it again from serper.dev (API key)",
    ),
    (
        "web.serper_busy",
        "Serper is getting too many searches right now – try again in a moment",
    ),
    (
        "web.serper_used_up",
        "your Serper searches are used up – top up at serper.dev or switch to Wikipedia",
    ),
    // Documents
    (
        "doc.unsupported",
        "Ancilo cannot read {name} – it reads PDF, Word (.docx), Excel, CSV and text files",
    ),
    (
        "doc.unsupported_mac",
        "Ancilo cannot read {name} – it reads PDF, Word (.docx), Excel, CSV and text files, and pictures (JPEG, PNG, HEIC)",
    ),
    (
        "doc.too_large",
        "{name} is larger than {mb} MB – Ancilo does not read files this large",
    ),
    (
        "doc.pdf_unreadable",
        "cannot read the PDF {name} ({why}) – it may be damaged or protected by a password",
    ),
    (
        "doc.too_long",
        "reading this file took too long – it may be damaged",
    ),
    // Checking a result before keeping it (FPL-03)
    ("check.unreadable", "the file cannot be read: {why}"),
    ("check.readable", "the file opens and can be read"),
    ("check.empty", "the file is empty"),
    (
        "check.header_only",
        "the table has a header row but no rows below it",
    ),
    (
        "check.missing",
        "not in the file, though the task asks for it: {what}",
    ),
    ("check.has", "has what the task names: {what}"),
    (
        "check.no_total",
        "the task asks for a total, but there is no total row",
    ),
    (
        "check.formula_text",
        "{n} cell(s) hold a formula as text – Ancilo does not calculate formulas; check these values yourself",
    ),
    (
        "check.numbers_ok",
        "{n} total(s) and amount(s) checked – they add up",
    ),
    (
        "check.numbers_none",
        "no totals or amounts that could be checked",
    ),
    (
        "check.total_wrong",
        "the total of “{what}” says {shown}, but the rows above add up to {sum}",
    ),
    (
        "check.product_wrong",
        "the amount {amount} does not match quantity × price ({qty} × {price} = {want})",
    ),
    // Tasks and sessions
    (
        "task.too_wide",
        "{folder} is too wide for a task – choose a folder of your documents, like Documents or a folder in it (not the home folder, Library or a system folder)",
    ),
    (
        "session.turn_running",
        "a turn is still running – wait or cancel it",
    ),
    ("session.busy", "the session is busy – try again"),
    // Connecting Claude Code and Codex
    (
        "connect.not_installed",
        "{product} is not installed on this computer – the `{command}` command was not found. Install it ({site}), then connect again",
    ),
    // Removing Ancilo
    (
        "remove.not_disconnected",
        "{client} could not be disconnected: {why}",
    ),
    (
        "remove.key_left",
        "the key '{name}' could not be removed from the keychain: {why}",
    ),
    (
        "remove.keychain_unreadable",
        "cannot read the keychain: {why}",
    ),
    (
        "remove.app_left",
        "{path} could not be deleted ({why}) – move it to the Trash",
    ),
    (
        "remove.app_elsewhere",
        "{path} is not in a folder Ancilo can delete from – move it to the Trash",
    ),
    // Chats
    (
        "chat.documents_local",
        "this conversation holds your documents – they stay with the AI on this computer; choose a local model",
    ),
];

/// The message `key` with its placeholders filled. An unknown key or a
/// missing value is a bug: tests catch it, a release shows the template.
pub fn msg(key: &str, params: &[(&str, &dyn Display)]) -> String {
    let Some((_, template)) = MESSAGES.iter().find(|(k, _)| *k == key) else {
        debug_assert!(false, "unknown message key {key}");
        return key.to_string();
    };
    let mut out = template.to_string();
    for (name, value) in params {
        let slot = format!("{{{name}}}");
        debug_assert!(out.contains(&slot), "{key} has no {{{name}}}");
        out = out.replace(&slot, &value.to_string());
    }
    debug_assert!(
        !placeholders(&out)
            .iter()
            .any(|p| placeholders(template).contains(p)),
        "{key}: a placeholder was left empty"
    );
    out
}

/// The names of a template's placeholders, in order.
pub fn placeholders(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            names.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    names
}

/// The templates as a TypeScript module for the app (`just app-api`).
pub fn typescript() -> String {
    let mut out = String::from(
        "// Generated from crates/core/src/messages.rs by `just app-api` – do not edit.\n\n/** English templates of the messages people see, by key (`msg.<key>`). */\nexport const messagesEn = {\n",
    );
    for (key, template) in MESSAGES {
        out.push_str(&format!(
            "  {}: {},\n",
            serde_json::to_string(&format!("msg.{key}")).unwrap(),
            serde_json::to_string(template).unwrap()
        ));
    }
    out.push_str("} as const;\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_placeholders_named_once() {
        let mut keys = std::collections::HashSet::new();
        for (key, template) in MESSAGES {
            assert!(keys.insert(key), "{key} twice");
            let names = placeholders(template);
            let unique: std::collections::HashSet<_> = names.iter().collect();
            assert_eq!(unique.len(), names.len(), "{key}: a placeholder twice");
        }
    }

    #[test]
    fn a_message_is_its_template_filled() {
        assert_eq!(
            msg("memory.need", &[("need", &"41.6 GB"), ("room", &"33.0 GB")]),
            "this model needs about 41.6 GB of memory, but only about 33.0 GB are free right now – close some programs or choose a smaller model"
        );
        assert_eq!(
            msg("failed", &[("why", &msg("agent.no_answer", &[]))]),
            "(failed: the model ended without an answer)"
        );
    }

    /// The app's copy of the templates is the current one.
    #[test]
    fn the_apps_templates_are_up_to_date() {
        let file = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../app/src/i18n/messages.gen.ts"
        );
        let Ok(have) = std::fs::read_to_string(file) else {
            return; // a build without the app
        };
        assert_eq!(have, typescript(), "run `just app-api`");
    }
}
