//! Assistant operations – REST, CLI and MCP (`ask`).

use ancilo_core::{NoInput, OpBuilder, Registry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{AnswerWebProposal, AskInput, Assistant};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionRef {
    /// Id of a proposed action (`a-…`).
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Rejected {
    pub rejected: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    /// Id of the conversation (`c-…`).
    pub conversation: String,
    /// The mark in the answer, e.g. `D3`.
    pub mark: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationRef {
    /// Id of a conversation (`c-…`).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameInput {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Deleted {
    pub deleted: bool,
}

pub const ASK_DESCRIPTION: &str = "Ask Ancilo's assistant in natural language – it looks things up and operates Ancilo (models, roles, comparisons, search, diagnosis). Actions that change something are returned as `pending` and run only after `confirm_action`.";

pub fn register(registry: &mut Registry, assistant: Assistant) {
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("ask")
            .summary("Ask Ancilo's assistant (natural language)")
            .description(ASK_DESCRIPTION)
            .handler(move |_ctx, i: AskInput| {
                let a = a.clone();
                async move { a.ask(i).await }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("answer_web_proposal")
            .summary("Answer a proposed web search: search (with the query as shown or changed) or answer without the web")
            .description("When web search asks first, a chat answer can be a proposal: the search query that would go to the provider. `search: true` sends it (or `query` instead) and answers from the sources found; `false` answers without the web.")
            .handler(move |_ctx, i: AnswerWebProposal| {
                let a = a.clone();
                async move { a.answer_web_proposal(i).await }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("confirm_action")
            .summary("Carry out an action the assistant proposed")
            .manage()
            .consequential()
            .handler(move |_ctx, i: ActionRef| {
                let a = a.clone();
                async move { a.confirm(&i.id).await }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("reject_action")
            .summary("Drop an action the assistant proposed")
            .manage()
            .handler(move |_ctx, i: ActionRef| {
                let a = a.clone();
                async move { a.reject(&i.id).map(|_| Rejected { rejected: true }) }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("list_conversations")
            .summary("Conversations with the assistant, newest first")
            .handler(move |_ctx, _i: NoInput| {
                let a = a.clone();
                async move { a.conversations().list() }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("get_conversation")
            .summary("One conversation with the assistant, with all messages")
            .handler(move |_ctx, i: ConversationRef| {
                let a = a.clone();
                async move { a.conversations().get(&i.id) }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("open_evidence")
            .summary("A source of an answer from documents ([D3]): the passage the answer had, and whether its document changed since")
            .handler(move |_ctx, i: EvidenceRef| {
                let a = a.clone();
                async move { a.open_evidence(&i.conversation, &i.mark) }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("rename_conversation")
            .summary("Give a conversation another title")
            .manage()
            .handler(move |_ctx, i: RenameInput| {
                let a = a.clone();
                async move { a.conversations().rename(&i.id, &i.title) }
            }),
    );
    let a = assistant.clone();
    registry.register(
        OpBuilder::new("delete_conversation")
            .summary("Delete a conversation with the assistant")
            .manage()
            .handler(move |_ctx, i: ConversationRef| {
                let a = a.clone();
                async move {
                    // The documents' text goes with the conversation.
                    if let Some(d) = a.documents() {
                        d.forget_conversation(&i.id)?;
                    }
                    a.conversations()
                        .delete(&i.id)
                        .map(|_| Deleted { deleted: true })
                }
            }),
    );
    let a = assistant;
    registry.register(
        OpBuilder::new("pending_actions")
            .summary("Actions the assistant proposed that wait for confirmation")
            .handler(move |_ctx, _i: NoInput| {
                let a = a.clone();
                async move { Ok(a.pending()) }
            }),
    );
}
