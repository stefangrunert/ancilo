//! Per-model admission with priorities.
//!
//! A local GPU serves few requests at a time. When a model is busy, waiting
//! requests are admitted by priority (interactive before synchronous
//! delegation before background work before comparisons), then by arrival.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// A person is waiting (chat, assistant, external model API).
    Interactive = 0,
    /// A synchronous delegation (the caller waits).
    Sync = 1,
    /// Background delegation.
    Background = 2,
    /// Model comparisons and evals.
    Compare = 3,
}

impl Priority {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "interactive" => Some(Self::Interactive),
            "sync" => Some(Self::Sync),
            "background" => Some(Self::Background),
            "compare" => Some(Self::Compare),
            _ => None,
        }
    }
}

struct Waiter {
    priority: Priority,
    seq: u64,
    tx: oneshot::Sender<()>,
}

impl PartialEq for Waiter {
    fn eq(&self, o: &Self) -> bool {
        self.priority == o.priority && self.seq == o.seq
    }
}
impl Eq for Waiter {}
impl PartialOrd for Waiter {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Waiter {
    /// BinaryHeap is a max-heap: "greater" = served first.
    fn cmp(&self, o: &Self) -> Ordering {
        o.priority.cmp(&self.priority).then(o.seq.cmp(&self.seq))
    }
}

#[derive(Default)]
struct Queue {
    active: usize,
    waiting: BinaryHeap<Waiter>,
}

struct Inner {
    capacity: usize,
    seq: u64,
    queues: HashMap<String, Queue>,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Mutex<Inner>>,
}

/// Admission to a model; releases the slot when dropped.
pub struct Permit {
    inner: Arc<Mutex<Inner>>,
    model: String,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        let q = g.queues.entry(self.model.clone()).or_default();
        // Hand the slot to the next waiter that is still waiting.
        while let Some(w) = q.waiting.pop() {
            if w.tx.send(()).is_ok() {
                return; // slot transferred, `active` unchanged
            }
        }
        q.active = q.active.saturating_sub(1);
    }
}

impl Scheduler {
    /// `capacity`: concurrent requests per model.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                capacity: capacity.max(1),
                seq: 0,
                queues: HashMap::new(),
            })),
        }
    }

    pub async fn acquire(&self, model: &str, priority: Priority) -> Permit {
        let rx = {
            let mut g = self.inner.lock().unwrap();
            let capacity = g.capacity;
            g.seq += 1;
            let seq = g.seq;
            let q = g.queues.entry(model.to_string()).or_default();
            if q.active < capacity {
                q.active += 1;
                None
            } else {
                let (tx, rx) = oneshot::channel();
                q.waiting.push(Waiter { priority, seq, tx });
                Some(rx)
            }
        };
        if let Some(rx) = rx {
            // The sender is only dropped after handing over the slot.
            let _ = rx.await;
        }
        Permit {
            inner: self.inner.clone(),
            model: model.to_string(),
        }
    }

    pub fn waiting(&self, model: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .queues
            .get(model)
            .map_or(0, |q| q.waiting.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // covers: M2-AC-08
    #[tokio::test]
    async fn serves_by_priority_then_arrival_when_busy() {
        let s = Scheduler::new(1);
        let first = s.acquire("m", Priority::Background).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for (i, p) in [
            (1, Priority::Compare),
            (2, Priority::Background),
            (3, Priority::Interactive),
            (4, Priority::Sync),
            (5, Priority::Interactive),
        ] {
            let s = s.clone();
            let order = order.clone();
            handles.push(tokio::spawn(async move {
                let _permit = s.acquire("m", p).await;
                order.lock().unwrap().push(i);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(s.waiting("m"), 5);
        drop(first);
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![3, 5, 4, 2, 1]);
    }

    // covers: M2-AC-08
    #[tokio::test]
    async fn models_do_not_block_each_other_and_capacity_is_respected() {
        let s = Scheduler::new(2);
        let _a1 = s.acquire("a", Priority::Interactive).await;
        let _a2 = s.acquire("a", Priority::Interactive).await;
        // Model b is independent of a.
        let b = tokio::time::timeout(
            Duration::from_millis(100),
            s.acquire("b", Priority::Compare),
        )
        .await;
        assert!(b.is_ok());
        // A third request for a must wait.
        let a3 = tokio::time::timeout(
            Duration::from_millis(50),
            s.acquire("a", Priority::Interactive),
        )
        .await;
        assert!(a3.is_err());
        // A cancelled waiter does not leak the slot.
        drop(_a1);
        let a4 = tokio::time::timeout(
            Duration::from_millis(100),
            s.acquire("a", Priority::Interactive),
        )
        .await;
        assert!(a4.is_ok());
    }
}
