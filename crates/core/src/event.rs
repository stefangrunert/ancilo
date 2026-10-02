use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::Result;

/// Something that happened. Every long-running activity (downloads, model
/// processes, tasks, …) reports progress exclusively through events: they are
/// persisted, streamed to clients via SSE and asserted on in tests.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Event {
    /// Monotonic sequence number, unique per Ancilo home.
    pub seq: i64,
    pub ts: DateTime<Utc>,
    /// Dotted kind, e.g. `download.progress`, `instance.ready`.
    pub kind: String,
    /// What the event is about (model id, task id, …).
    pub subject: Option<String>,
    pub data: Value,
}

/// Persists events. Implemented by the storage crate.
pub trait EventSink: Send + Sync {
    fn persist(&self, event: &Event) -> Result<()>;
}

/// Fan-out of events to live subscribers plus optional persistence.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Inner>,
}

struct Inner {
    tx: broadcast::Sender<Event>,
    seq: AtomicI64,
    sink: Option<Arc<dyn EventSink>>,
}

impl EventBus {
    /// Creates a bus. `last_seq` continues numbering after a restart.
    pub fn new(sink: Option<Arc<dyn EventSink>>, last_seq: i64) -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self {
            inner: Arc::new(Inner {
                tx,
                seq: AtomicI64::new(last_seq),
                sink,
            }),
        }
    }

    /// A bus without persistence, for tests and tools.
    pub fn in_memory() -> Self {
        Self::new(None, 0)
    }

    pub fn emit(&self, kind: &str, subject: Option<&str>, data: Value) -> Event {
        let event = Event {
            seq: self.inner.seq.fetch_add(1, Ordering::SeqCst) + 1,
            ts: Utc::now(),
            kind: kind.to_string(),
            subject: subject.map(str::to_string),
            data,
        };
        if let Some(sink) = &self.inner.sink
            && let Err(e) = sink.persist(&event)
        {
            tracing::warn!(error = %e, kind, "failed to persist event");
        }
        tracing::debug!(kind, subject = ?event.subject, "event");
        // No subscribers is fine.
        let _ = self.inner.tx.send(event.clone());
        event
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.tx.subscribe()
    }

    pub fn last_seq(&self) -> i64 {
        self.inner.seq.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn emits_to_subscribers_with_increasing_seq() {
        let bus = EventBus::new(None, 41);
        let mut rx = bus.subscribe();
        bus.emit("a.b", Some("x"), json!({"n": 1}));
        bus.emit("a.c", None, json!({}));
        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert_eq!(first.seq, 42);
        assert_eq!(second.seq, 43);
        assert_eq!(first.subject.as_deref(), Some("x"));
    }
}
