//! Terminals: a PTY per terminal, living in the daemon (they survive closing
//! or reloading the window). The app connects over a WebSocket with a
//! one-time ticket – browsers cannot send an `Authorization` header there,
//! and a long-lived token must not appear in URLs.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ancilo_core::{Error, EventBus, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;

const SCROLLBACK: usize = 64 * 1024;
const TICKET_TTL: Duration = Duration::from_secs(60);

struct Term {
    id: String,
    cwd: PathBuf,
    session: Option<String>,
    /// Input goes through a writer thread: a program that reads nothing
    /// cannot block the daemon's runtime.
    input: std::sync::mpsc::SyncSender<Vec<u8>>,
    /// Ends connections when the terminal closes.
    closed: tokio_util::sync::CancellationToken,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    scrollback: Mutex<VecDeque<u8>>,
    tx: broadcast::Sender<Vec<u8>>,
    created_at: DateTime<Utc>,
    exited: Mutex<bool>,
}

impl Term {
    fn push(&self, bytes: &[u8]) {
        let mut sb = self.scrollback.lock().unwrap();
        sb.extend(bytes);
        while sb.len() > SCROLLBACK {
            sb.pop_front();
        }
        let _ = self.tx.send(bytes.to_vec());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TerminalView {
    pub id: String,
    pub cwd: PathBuf,
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
    pub exited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Ticket {
    pub terminal: String,
    /// One-time, valid for a minute: `/api/v1/pty/<terminal>?ticket=<ticket>`.
    pub ticket: String,
    pub path: String,
}

#[derive(Clone, Default)]
pub struct Terminals {
    terms: Arc<Mutex<HashMap<String, Arc<Term>>>>,
    tickets: Arc<Mutex<HashMap<String, (String, Instant)>>>,
    bus: Option<EventBus>,
}

fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| Path::new(s).exists())
        .unwrap_or_else(|| {
            if Path::new("/bin/zsh").exists() {
                "/bin/zsh".into()
            } else {
                "/bin/sh".into()
            }
        })
}

impl Terminals {
    pub fn new(bus: EventBus) -> Self {
        Self {
            bus: Some(bus),
            ..Default::default()
        }
    }

    pub fn open(
        &self,
        cwd: &Path,
        session: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalView> {
        if !cwd.is_dir() {
            return Err(Error::invalid(format!(
                "no such directory: {}",
                cwd.display()
            )));
        }
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(2),
                cols: cols.max(10),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::internal(format!("cannot open a terminal: {e}")))?;
        let mut cmd = CommandBuilder::new(shell());
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::internal(format!("cannot start the shell: {e}")))?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::internal(e.to_string()))?;
        let (input, inbox) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
        std::thread::spawn(move || {
            while let Ok(bytes) = inbox.recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        let (tx, _) = broadcast::channel(256);
        let id = format!("pty-{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let term = Arc::new(Term {
            id: id.clone(),
            cwd: cwd.to_path_buf(),
            session,
            input,
            closed: tokio_util::sync::CancellationToken::new(),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            scrollback: Mutex::new(VecDeque::new()),
            tx,
            created_at: Utc::now(),
            exited: Mutex::new(false),
        });
        let t2 = term.clone();
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => t2.push(&buf[..n]),
                }
            }
            *t2.exited.lock().unwrap() = true;
            // Reap the shell (no zombie).
            let _ = t2.child.lock().unwrap().wait();
            t2.push(b"\r\n[process exited]\r\n");
            if let Some(b) = bus {
                b.emit("terminal.exited", Some(&t2.id), json!({}));
            }
        });
        self.terms.lock().unwrap().insert(id.clone(), term.clone());
        if let Some(b) = &self.bus {
            b.emit("terminal.opened", Some(&id), json!({"cwd": cwd}));
        }
        Ok(view(&term))
    }

    pub fn list(&self) -> Vec<TerminalView> {
        let mut v: Vec<TerminalView> = self
            .terms
            .lock()
            .unwrap()
            .values()
            .map(|t| view(t))
            .collect();
        v.sort_by_key(|t| t.created_at);
        v
    }

    pub fn ticket(&self, id: &str) -> Result<Ticket> {
        if !self.terms.lock().unwrap().contains_key(id) {
            return Err(Error::not_found(format!("no terminal '{id}'")));
        }
        let ticket = uuid::Uuid::new_v4().simple().to_string();
        let mut tickets = self.tickets.lock().unwrap();
        tickets.retain(|_, (_, at)| at.elapsed() < TICKET_TTL);
        tickets.insert(ticket.clone(), (id.to_string(), Instant::now()));
        Ok(Ticket {
            terminal: id.to_string(),
            path: format!("/api/v1/pty/{id}?ticket={ticket}"),
            ticket,
        })
    }

    fn redeem(&self, id: &str, ticket: &str) -> Option<Arc<Term>> {
        let (tid, at) = self.tickets.lock().unwrap().remove(ticket)?;
        if tid != id || at.elapsed() > TICKET_TTL {
            return None;
        }
        self.terms.lock().unwrap().get(id).cloned()
    }

    pub fn close(&self, id: &str) -> Result<()> {
        let t = self
            .terms
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(|| Error::not_found(format!("no terminal '{id}'")))?;
        Self::end(&t);
        if let Some(b) = &self.bus {
            b.emit("terminal.closed", Some(id), json!({}));
        }
        Ok(())
    }

    /// Kills the shell, ends its connections and tickets (the reader thread reaps it).
    fn end(t: &Term) {
        let _ = t.child.lock().unwrap().kill();
        t.closed.cancel();
    }

    /// Closes the terminals of a session (it is being deleted).
    pub fn close_session(&self, session: &str) {
        let ids: Vec<String> = self
            .terms
            .lock()
            .unwrap()
            .values()
            .filter(|t| t.session.as_deref() == Some(session))
            .map(|t| t.id.clone())
            .collect();
        for id in ids {
            self.close(&id).ok();
        }
        self.tickets
            .lock()
            .unwrap()
            .retain(|_, (tid, _)| self.terms.lock().unwrap().contains_key(tid));
    }

    /// Writes text into the terminals of a session (display only).
    pub fn show(&self, session: &str, text: &str) {
        for t in self.terms.lock().unwrap().values() {
            if t.session.as_deref() == Some(session) {
                t.push(text.as_bytes());
            }
        }
    }

    pub fn shutdown(&self) {
        for (_, t) in self.terms.lock().unwrap().drain() {
            Self::end(&t);
        }
    }
}

fn view(t: &Term) -> TerminalView {
    TerminalView {
        id: t.id.clone(),
        cwd: t.cwd.clone(),
        session: t.session.clone(),
        created_at: t.created_at,
        exited: *t.exited.lock().unwrap(),
    }
}

#[derive(Deserialize)]
pub struct TicketQuery {
    ticket: String,
}

#[derive(Deserialize)]
struct Control {
    resize: Option<(u16, u16)>,
}

/// `GET /api/v1/pty/{id}?ticket=…` – binary frames carry terminal bytes both
/// ways; text frames carry control messages (`{"resize": [cols, rows]}`).
pub async fn websocket(
    State(terms): State<Terminals>,
    AxPath(id): AxPath<String>,
    Query(q): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(term) = terms.redeem(&id, &q.ticket) else {
        return (StatusCode::UNAUTHORIZED, "invalid or expired ticket").into_response();
    };
    ws.on_upgrade(move |socket| serve(socket, term))
}

async fn serve(socket: WebSocket, term: Arc<Term>) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = term.tx.subscribe();
    let backlog: Vec<u8> = term.scrollback.lock().unwrap().iter().copied().collect();
    if !backlog.is_empty() && sink.send(Message::Binary(backlog.into())).await.is_err() {
        return;
    }
    let out = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(bytes) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    loop {
        let msg = tokio::select! {
            m = stream.next() => match m {
                Some(Ok(m)) => m,
                _ => break,
            },
            _ = term.closed.cancelled() => break,
        };
        match msg {
            Message::Binary(b) => {
                let _ = term.input.try_send(b.to_vec());
            }
            Message::Text(t) => {
                if let Ok(c) = serde_json::from_str::<Control>(&t) {
                    if let Some((cols, rows)) = c.resize {
                        let _ = term.master.lock().unwrap().resize(PtySize {
                            rows: rows.max(2),
                            cols: cols.max(10),
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                } else {
                    let _ = term.input.try_send(t.as_bytes().to_vec());
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    out.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_runs_commands_keeps_scrollback_and_tickets_are_single_use() {
        let terms = Terminals::default();
        let dir = tempfile::tempdir().unwrap();
        let t = terms.open(dir.path(), Some("s1".into()), 80, 24).unwrap();
        let term = terms.terms.lock().unwrap().get(&t.id).cloned().unwrap();
        term.input.send(b"echo hello-from-pty\r".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let sb: Vec<u8> = term.scrollback.lock().unwrap().iter().copied().collect();
            if String::from_utf8_lossy(&sb)
                .matches("hello-from-pty")
                .count()
                >= 2
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no output: {}",
                String::from_utf8_lossy(&sb)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        terms.show("s1", "[agent] $ ls\r\n");
        let sb: Vec<u8> = term.scrollback.lock().unwrap().iter().copied().collect();
        assert!(String::from_utf8_lossy(&sb).contains("[agent] $ ls"));
        let ticket = terms.ticket(&t.id).unwrap();
        assert!(terms.redeem(&t.id, &ticket.ticket).is_some());
        assert!(
            terms.redeem(&t.id, &ticket.ticket).is_none(),
            "tickets are single use"
        );
        let other = terms.ticket(&t.id).unwrap();
        assert!(terms.redeem("pty-other", &other.ticket).is_none());
        terms.close(&t.id).unwrap();
        assert!(terms.list().is_empty());
        assert!(
            term.closed.is_cancelled(),
            "connections end with the terminal"
        );
        // A session's terminals close with it – tickets included.
        let a = terms.open(dir.path(), Some("s2".into()), 80, 24).unwrap();
        let b = terms.open(dir.path(), None, 80, 24).unwrap();
        let ticket = terms.ticket(&a.id).unwrap();
        terms.close_session("s2");
        let left: Vec<String> = terms.list().into_iter().map(|x| x.id).collect();
        assert_eq!(left, std::slice::from_ref(&b.id));
        assert!(terms.redeem(&a.id, &ticket.ticket).is_none());
        terms.close(&b.id).unwrap();
    }
}
