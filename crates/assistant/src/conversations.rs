//! Conversations with the assistant, kept like chats in a chat app: the app
//! lists them, a follow-up question continues one, and proposed actions keep
//! their outcome.

use std::path::PathBuf;

use ancilo_core::{Error, Result};
use ancilo_storage::Db;
use ancilo_storage::rusqlite::{OptionalExtension, params};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{OperationCall, PendingAction};

/// Earlier messages that go along with a follow-up (the newest ones).
const HISTORY_MESSAGES: usize = 20;
/// An earlier answer is shortened to this in the history.
const HISTORY_CHARS: usize = 4000;
const TITLE_CHARS: usize = 60;

/// What a conversation was started for. Only Ancilo's own conversations
/// ("setup") – and requests that are about Ancilo – get Ancilo's tools;
/// everything else is a plain chat with the local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    /// A plain chat.
    Chat,
    /// About Ancilo itself: setting it up, models, speed, connections.
    #[default]
    Setup,
    /// Writing texts.
    Write,
    /// Having something explained.
    Explain,
    /// Summarising a text.
    Summarize,
}

impl ChatKind {
    fn as_str(self) -> &'static str {
        match self {
            ChatKind::Chat => "chat",
            ChatKind::Setup => "setup",
            ChatKind::Write => "write",
            ChatKind::Explain => "explain",
            ChatKind::Summarize => "summarize",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "chat" => ChatKind::Chat,
            "write" => ChatKind::Write,
            "explain" => ChatKind::Explain,
            "summarize" => ChatKind::Summarize,
            _ => ChatKind::Setup,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversationMessage {
    /// `user` or `assistant`
    pub role: String,
    pub text: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Operations the assistant ran or proposed for this answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<OperationCall>,
    /// Actions proposed with this answer (with their outcome once decided).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingAction>,
    /// The web search behind this message: proposed, done (with sources), …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebNote>,
    /// Documents sent with this message (its text stays on this computer).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ancilo_docs::AttachmentView>,
    /// The answer drew on the user's documents.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub documents: bool,
    /// The passages the answer was given, each with its mark (`[D3]` in the
    /// text) and where it stands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ancilo_docs::evidence::Evidence>,
    /// Marks the model made up (no such passage): taken out of the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped_marks: Vec<String>,
}

/// What became of a web search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebState {
    /// Ancilo proposes to search with this query; nothing went out yet.
    Proposed,
    /// The user agreed (the answer follows in the next message).
    Accepted,
    /// The user said no (the answer follows without the web).
    Declined,
    /// Answered with what the search found.
    Searched,
    /// The search failed; answered without the web.
    Failed,
    /// Web search is off; the answer may need it – offer to set it up.
    Offer,
}

/// A web search, as a message shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WebNote {
    pub state: WebState,
    /// The search query (what goes, or went, to the provider).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lang: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ancilo_web::Provider>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ancilo_web::Source>,
    /// Why the search failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl WebNote {
    pub fn new(state: WebState) -> Self {
        Self {
            state,
            query: String::new(),
            topic: None,
            lang: String::new(),
            provider: None,
            sources: Vec::new(),
            error: None,
        }
    }
}

impl ConversationMessage {
    pub fn user(text: &str) -> Self {
        Self {
            role: "user".into(),
            text: text.into(),
            at: Utc::now(),
            model: None,
            operations: Vec::new(),
            pending: Vec::new(),
            web: None,
            attachments: Vec::new(),
            documents: false,
            evidence: Vec::new(),
            dropped_marks: Vec::new(),
        }
    }

    /// Ancilo's own words (e.g. the greeting a conversation starts with).
    pub fn assistant(text: &str) -> Self {
        Self {
            role: "assistant".into(),
            ..Self::user(text)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConversationInfo {
    pub id: String,
    pub title: String,
    pub kind: ChatKind,
    /// The chat project (a folder Ancilo reads) it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub kind: ChatKind,
    /// The chat project (a folder Ancilo reads) it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<ConversationMessage>,
}

impl Conversation {
    pub fn new(first_prompt: &str, kind: ChatKind) -> Self {
        let now = Utc::now();
        Self {
            id: format!("c-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            kind,
            title: title_of(first_prompt),
            folder: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        }
    }

    /// The earlier exchange as model messages (the newest ones, long
    /// answers shortened).
    pub fn history(&self) -> Vec<Value> {
        let skip = self.messages.len().saturating_sub(HISTORY_MESSAGES);
        self.messages
            .iter()
            .skip(skip)
            .filter(|m| !m.text.trim().is_empty())
            .map(|m| {
                let text = crate::clip(&m.text, HISTORY_CHARS);
                json!({"role": if m.role == "user" { "user" } else { "assistant" }, "content": text})
            })
            .collect()
    }

    /// Whether text from the web went into this conversation – it then never
    /// gets Ancilo's tools (a page could try to give orders).
    /// The conversation saw the user's documents: it stays with the AI on
    /// this computer – no cloud model, no tools.
    pub fn has_documents(&self) -> bool {
        self.messages
            .iter()
            .any(|m| m.documents || !m.attachments.is_empty())
            || self.folder.is_some()
    }

    /// The documents attached anywhere in the conversation.
    pub fn attachment_ids(&self) -> Vec<String> {
        self.messages
            .iter()
            .flat_map(|m| m.attachments.iter().map(|a| a.id.clone()))
            .collect()
    }

    pub fn used_web(&self) -> bool {
        self.messages.iter().any(|m| {
            m.web
                .as_ref()
                .is_some_and(|w| w.state == WebState::Searched)
        })
    }

    /// The last thing the user asked before now (helps to pick the
    /// operations for a short follow-up like "yes, do that").
    pub fn last_prompt(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.text.as_str())
    }
}

fn time(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// The first line of the first request, shortened.
pub fn title_of(prompt: &str) -> String {
    let line = prompt.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() <= TITLE_CHARS {
        line.to_string()
    } else {
        format!(
            "{}…",
            line.chars()
                .take(TITLE_CHARS)
                .collect::<String>()
                .trim_end()
        )
    }
}

/// The conversations in the database.
#[derive(Clone)]
pub struct Conversations {
    db: Db,
}

impl Conversations {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn list(&self) -> Result<Vec<ConversationInfo>> {
        type Row = (String, String, String, String, i64, String, Option<String>);
        let rows: Vec<Row> = self.db.with(|c| {
            let mut s = c.prepare(
                "SELECT id, title, created_at, updated_at, json_array_length(messages), kind, folder
                 FROM conversations ORDER BY updated_at DESC",
            )?;
            s.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect()
        })?;
        Ok(rows
            .into_iter()
            .map(
                |(id, title, created, updated, n, kind, folder)| ConversationInfo {
                    id,
                    title,
                    kind: ChatKind::parse(&kind),
                    folder: folder.map(PathBuf::from),
                    created_at: time(&created),
                    updated_at: time(&updated),
                    messages: n.max(0) as usize,
                },
            )
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<Conversation> {
        type Row = (String, String, String, String, String, Option<String>);
        let row: Option<Row> = self.db.with(|c| {
            c.query_row(
                "SELECT title, created_at, updated_at, messages, kind, folder FROM conversations WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
        })?;
        let (title, created, updated, messages, kind, folder) =
            row.ok_or_else(|| Error::not_found(format!("no conversation '{id}'")))?;
        Ok(Conversation {
            id: id.to_string(),
            title,
            kind: ChatKind::parse(&kind),
            folder: folder.map(PathBuf::from),
            created_at: time(&created),
            updated_at: time(&updated),
            messages: serde_json::from_str(&messages).unwrap_or_default(),
        })
    }

    pub fn save(&self, c: &Conversation) -> Result<()> {
        self.db.with(|db| {
            db.execute(
                "INSERT INTO conversations(id, created_at, updated_at, title, messages, kind, folder) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET updated_at = excluded.updated_at, title = excluded.title, messages = excluded.messages",
                params![
                    c.id,
                    c.created_at.to_rfc3339(),
                    c.updated_at.to_rfc3339(),
                    c.title,
                    serde_json::to_string(&c.messages).unwrap_or_else(|_| "[]".into()),
                    c.kind.as_str(),
                    c.folder.as_ref().map(|f| f.display().to_string())
                ],
            )
            .map(|_| ())
        })
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<ConversationInfo> {
        let title = title.trim();
        if title.is_empty() {
            return Err(Error::invalid("the title is empty"));
        }
        let mut c = self.get(id)?;
        c.title = title.chars().take(200).collect();
        self.save(&c)?;
        Ok(ConversationInfo {
            messages: c.messages.len(),
            kind: c.kind,
            folder: c.folder,
            id: c.id,
            title: c.title,
            created_at: c.created_at,
            updated_at: c.updated_at,
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM conversations WHERE id = ?1", params![id]))?;
        if n == 0 {
            return Err(Error::not_found(format!("no conversation '{id}'")));
        }
        Ok(())
    }

    /// Records what became of a proposed action.
    pub fn decided(&self, conversation: &str, action: &str, outcome: &str) -> Result<()> {
        let mut c = self.get(conversation)?;
        let mut found = false;
        for m in &mut c.messages {
            for p in &mut m.pending {
                if p.id == action {
                    p.outcome = Some(outcome.to_string());
                    found = true;
                }
            }
        }
        if found {
            self.save(&c)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Conversations {
        Conversations::new(Db::in_memory().unwrap())
    }

    #[test]
    fn titles_are_the_first_line_shortened() {
        assert_eq!(
            title_of("  Which models do I have?\nmore"),
            "Which models do I have?"
        );
        let long = "a".repeat(100);
        assert_eq!(title_of(&long).chars().count(), TITLE_CHARS + 1);
    }

    // covers: M6-AC-11
    #[test]
    fn conversations_are_kept_listed_renamed_and_deleted() {
        let s = store();
        let mut c = Conversation::new("Which models do I have?", ChatKind::Setup);
        c.messages
            .push(ConversationMessage::user("Which models do I have?"));
        c.messages.push(ConversationMessage {
            role: "assistant".into(),
            text: "Qwen3.5-4B is running.".into(),
            ..ConversationMessage::user("")
        });
        s.save(&c).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Which models do I have?");
        assert_eq!(list[0].messages, 2);
        let got = s.get(&c.id).unwrap();
        assert_eq!(got.messages[1].text, "Qwen3.5-4B is running.");
        assert_eq!(
            got.history(),
            vec![
                json!({"role": "user", "content": "Which models do I have?"}),
                json!({"role": "assistant", "content": "Qwen3.5-4B is running."}),
            ]
        );
        assert_eq!(got.last_prompt(), Some("Which models do I have?"));
        assert_eq!(s.rename(&c.id, " Models ").unwrap().title, "Models");
        assert!(s.rename(&c.id, " ").is_err());
        s.delete(&c.id).unwrap();
        assert!(s.get(&c.id).is_err());
        assert!(s.delete(&c.id).is_err());
    }

    #[test]
    fn the_history_keeps_the_newest_messages() {
        let mut c = Conversation::new("x", ChatKind::Chat);
        for i in 0..30 {
            c.messages.push(ConversationMessage::user(&format!("m{i}")));
        }
        let h = c.history();
        assert_eq!(h.len(), HISTORY_MESSAGES);
        assert_eq!(h.last().unwrap()["content"], "m29");
    }
}
