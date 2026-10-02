//! A session's tools: the ordinary workspace tools, gated by the session's
//! permission. A call above it pauses until the user decides
//! (`session.approval_required` → `approve` / `reject`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ancilo_agent::{Access, ToolOutput, Toolbox, Workspace};
use ancilo_core::{BoxFuture, EventBus};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// What the user decided.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub allow: bool,
    /// Allow this kind of action for the rest of the session.
    pub remember: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Approval {
    pub id: String,
    pub session: String,
    pub tool: String,
    pub arguments: Value,
    /// The permission the action needs.
    pub needs: Access,
    pub created_at: DateTime<Utc>,
}

struct Waiting {
    approval: Approval,
    tx: oneshot::Sender<Decision>,
}

/// Open approvals of all sessions.
#[derive(Clone, Default)]
pub struct Approvals(Arc<Mutex<HashMap<String, Waiting>>>);

impl Approvals {
    pub fn list(&self, session: Option<&str>) -> Vec<Approval> {
        let mut v: Vec<Approval> = self
            .0
            .lock()
            .unwrap()
            .values()
            .filter(|w| session.is_none_or(|s| w.approval.session == s))
            .map(|w| w.approval.clone())
            .collect();
        v.sort_by_key(|a| a.created_at);
        v
    }

    /// Delivers the decision; `None` if no such approval is open.
    pub fn decide(&self, id: &str, d: Decision) -> Option<Approval> {
        let w = self.0.lock().unwrap().remove(id)?;
        let approval = w.approval.clone();
        let _ = w.tx.send(d);
        Some(approval)
    }

    fn wait(&self, approval: Approval) -> oneshot::Receiver<Decision> {
        let (tx, rx) = oneshot::channel();
        self.0
            .lock()
            .unwrap()
            .insert(approval.id.clone(), Waiting { approval, tx });
        rx
    }

    fn drop_id(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }
}

/// The permission a tool call needs.
pub fn needs(tool: &str) -> Access {
    match tool {
        "write_file" | "edit_file" => Access::Edit,
        "bash" => Access::Shell,
        _ => Access::Read,
    }
}

/// Starts the context the agent adds to the first message (hidden in the chat).
pub const CONTEXT_MARK: &str = "(Project root: ";

pub type Transcript = Arc<dyn Fn(&str) + Send + Sync>;

pub struct SessionTools {
    pub ws: Workspace,
    pub session: String,
    pub permission: Arc<Mutex<Access>>,
    pub approvals: Approvals,
    pub bus: EventBus,
    pub cancel: CancellationToken,
    /// Shows the agent's shell commands in the session's terminal.
    pub transcript: Option<Transcript>,
}

impl SessionTools {
    async fn gate(&self, name: &str, args: &Value) -> Result<(), String> {
        let need = needs(name);
        if need <= *self.permission.lock().unwrap() {
            return Ok(());
        }
        let approval = Approval {
            id: format!("ap-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]),
            session: self.session.clone(),
            tool: name.to_string(),
            arguments: args.clone(),
            needs: need,
            created_at: Utc::now(),
        };
        let id = approval.id.clone();
        // Registered before it is announced: a client reacting to the event
        // at once must find it.
        let rx = self.approvals.wait(approval);
        self.bus.emit(
            "session.approval_required",
            Some(&self.session),
            json!({"approval": id, "tool": name, "arguments": args, "needs": need}),
        );
        let decision = tokio::select! {
            d = rx => d.ok(),
            _ = self.cancel.cancelled() => {
                self.approvals.drop_id(&id);
                None
            }
        };
        match decision {
            Some(d) if d.allow => {
                if d.remember {
                    let mut p = self.permission.lock().unwrap();
                    *p = (*p).max(need);
                }
                self.bus.emit(
                    "session.approved",
                    Some(&self.session),
                    json!({"approval": id, "remember": d.remember}),
                );
                Ok(())
            }
            Some(_) => {
                self.bus.emit(
                    "session.rejected",
                    Some(&self.session),
                    json!({"approval": id}),
                );
                Err("The user did not allow this action. Continue without it, or explain what you need.".into())
            }
            None => Err("cancelled".into()),
        }
    }
}

impl Toolbox for SessionTools {
    fn definitions(&self) -> Vec<Value> {
        self.ws.definitions()
    }

    fn execute<'a>(&'a self, name: &'a str, args: &'a Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Err(e) = self.gate(name, args).await {
                return ToolOutput::err(e);
            }
            if name == "bash"
                && let Some(t) = &self.transcript
            {
                t(&format!(
                    "\r\n\x1b[2m[agent] $ {}\x1b[0m\r\n",
                    args["command"].as_str().unwrap_or_default()
                ));
            }
            // A cancelled turn stops its tool at once (a running command's
            // whole process group ends with it).
            let out = tokio::select! {
                out = self.ws.execute(name, args) => out,
                _ = self.cancel.cancelled() => return ToolOutput::err("cancelled"),
            };
            if name == "bash"
                && let Some(t) = &self.transcript
            {
                t(&format!("{}\r\n", out.content.replace('\n', "\r\n")));
            }
            out
        })
    }

    fn changes(&self) -> (Vec<ancilo_agent::FileChange>, String) {
        self.ws.changes()
    }

    fn context(&self) -> Option<String> {
        Some(format!(
            "{CONTEXT_MARK}{} · allowed without asking: {:?})",
            self.ws.shown_root().display(),
            *self.permission.lock().unwrap()
        ))
    }
}
