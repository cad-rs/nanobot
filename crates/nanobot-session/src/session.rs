//! Session: event-sourced design session (design-harness 10.5).
//!
//! The event log is the **single source of truth**. Model history is
//! projected from it via `derive_messages()` — never stored separately.
//! "Model-visible means logged."
//!
//! Persistence: JSONL at `.ocad/conversations/<session-id>/events.jsonl`.
//! MVP uses full-snapshot writes (simple, validatable, diffable).

use crate::event::SessionEvent;
use std::sync::{Arc, Mutex};
use web_time::{SystemTime, UNIX_EPOCH};

/// A handle that reverses a subscription when disposed.
pub trait Disposable: Send + Sync {
    fn dispose(&self);
}

/// A no-op disposable.
pub struct NoopDisposable;
impl Disposable for NoopDisposable {
    fn dispose(&self) {}
}

/// A single entry in the event log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    /// Monotonic sequence number (assigned by `Session::append`).
    pub seq: u32,
    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// The event payload.
    pub event: SessionEvent,
}

/// Subscriber callback type.
type Subscriber = Box<dyn Fn(&SessionEvent, u32) + Send + Sync>;

/// An event-sourced design session.
///
/// `Session` is `Send + Sync` (Arc + Mutex internals). Clone is cheap.
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    id: String,
    next_seq: Mutex<u32>,
    events: Mutex<Vec<LogEntry>>,
    subscribers: Mutex<Vec<Subscriber>>,
}

impl Session {
    /// Create a new session with the given ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                id: id.into(),
                next_seq: Mutex::new(0),
                events: Mutex::new(Vec::new()),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Get the session ID.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// Get the next sequence number (for testing/debugging).
    pub fn next_seq(&self) -> u32 {
        *self.inner.next_seq.lock().unwrap()
    }

    /// Append an event to the log, assigning a monotonic seq and timestamp.
    ///
    /// Returns the assigned seq number.
    pub fn append(&self, event: SessionEvent) -> u32 {
        let seq = {
            let mut next_seq = self.inner.next_seq.lock().unwrap();
            let seq = *next_seq;
            *next_seq += 1;
            seq
        };

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let entry = LogEntry {
            seq,
            timestamp_ms,
            event: event.clone(),
        };

        self.inner.events.lock().unwrap().push(entry);

        // Notify subscribers.
        let subscribers = self.inner.subscribers.lock().unwrap();
        for sub in subscribers.iter() {
            sub(&event, seq);
        }

        seq
    }

    /// Get a reference to the event log.
    pub fn events(&self) -> Vec<LogEntry> {
        self.inner.events.lock().unwrap().clone()
    }

    /// Rewind the log to `seq` (inclusive): drop every later event and resume
    /// numbering right after it.
    ///
    /// This is the design's "会话内 undo 到该位置" (design-harness 14.3): a
    /// rejected plan rewinds the session to where it began, so the rolled-back
    /// work leaves the model's projected history too. The turn and step
    /// counters stay monotonic — sequence numbers are never reused.
    ///
    /// Rewinding past the end is a no-op.
    pub fn truncate_to(&self, seq: u32) {
        let mut events = self.inner.events.lock().unwrap();
        events.retain(|entry| entry.seq <= seq);

        let mut next_seq = self.inner.next_seq.lock().unwrap();
        if *next_seq > seq + 1 {
            *next_seq = seq + 1;
        }
    }

    /// Persist the log to `path`, rewriting the whole file after every append
    /// (design-harness 10.5: MVP uses full-snapshot writes).
    ///
    /// Writes the current log immediately, so a session has a file from the
    /// moment it is bound. The callback holds only a weak reference to the
    /// session, so the subscription cannot keep it alive.
    ///
    /// Note: a [`Session::truncate_to`] rewinds the in-memory log but emits no
    /// event, so the file only catches up on the next append — which is why
    /// plan rejection truncates and then appends its `PlanRejected`.
    #[cfg(not(target_family = "wasm"))]
    pub fn persist_to(&self, path: impl Into<std::path::PathBuf>) -> Arc<dyn Disposable> {
        let path = path.into();
        let _ = crate::persistence::save(&path, &self.events());

        let weak = Arc::downgrade(&self.inner);
        self.subscribe(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                let entries = inner.events.lock().unwrap().clone();
                let _ = crate::persistence::save(&path, &entries);
            }
        })
    }

    /// wasm: persist_to via OPFS (async, fire-and-forget).
    #[cfg(target_family = "wasm")]
    pub fn persist_to(&self, path: impl Into<std::path::PathBuf>) -> Arc<dyn Disposable> {
        let path = path.into();
        let weak = Arc::downgrade(&self.inner);
        self.subscribe(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                let entries = inner.events.lock().unwrap().clone();
                let path = path.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = crate::persistence::save_opfs(&path, &entries).await;
                });
            }
        })
    }

    /// Append one audit record per tool call to `path` (design-harness 14.5).
    ///
    /// The audit side log is the project's trail of *what was attempted*: it
    /// carries every call, including the ones that were refused or rolled back
    /// and so left no trace in the document. Each record pairs the call with
    /// its result.
    #[cfg(not(target_family = "wasm"))]
    pub fn audit_to(&self, path: impl Into<std::path::PathBuf>) -> Arc<dyn Disposable> {
        let path = path.into();
        let weak = Arc::downgrade(&self.inner);
        self.subscribe(move |event, _| {
            let SessionEvent::ToolResult {
                id,
                success,
                output,
                ..
            } = event
            else {
                return;
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let entries = inner.events.lock().unwrap().clone();
            let Some((seq, name, arguments)) =
                entries.iter().find_map(|entry| match &entry.event {
                    SessionEvent::ToolCall {
                        id: call_id,
                        name,
                        arguments,
                        seq,
                    } if call_id == id => Some((*seq, name.clone(), arguments.clone())),
                    _ => None,
                })
            else {
                return;
            };
            let record = serde_json::json!({
                "seq": seq,
                "tool": name,
                "arguments": arguments,
                "success": success,
                "output": output,
            });
            let _ = crate::persistence::append_line(&path, &record);
        })
    }

    /// wasm: audit_to via OPFS (async, fire-and-forget).
    #[cfg(target_family = "wasm")]
    pub fn audit_to(&self, path: impl Into<std::path::PathBuf>) -> Arc<dyn Disposable> {
        let path = path.into();
        let weak = Arc::downgrade(&self.inner);
        self.subscribe(move |event, _| {
            let SessionEvent::ToolResult {
                id,
                success,
                output,
                ..
            } = event
            else {
                return;
            };
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let entries = inner.events.lock().unwrap().clone();
            let Some((seq, name, arguments)) =
                entries.iter().find_map(|entry| match &entry.event {
                    SessionEvent::ToolCall {
                        id: call_id,
                        name,
                        arguments,
                        seq,
                    } if call_id == id => Some((*seq, name.clone(), arguments.clone())),
                    _ => None,
                })
            else {
                return;
            };
            let record = serde_json::json!({
                "seq": seq,
                "tool": name,
                "arguments": arguments,
                "success": success,
                "output": output,
            });
            let path = path.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = crate::persistence::append_opfs(&path, &record).await;
            });
        })
    }

    /// Get the number of events in the log.
    pub fn len(&self) -> usize {
        self.inner.events.lock().unwrap().len()
    }

    /// Restore the log from persisted entries (build-time recovery).
    ///
    /// Entries are taken as-is — seq and timestamp preserved — so re-saving a
    /// hydrated log is byte-identical to the file it came from. Subscribers
    /// are not notified: hydration happens before persistence is attached,
    /// when nothing is watching yet.
    pub fn hydrate(&self, entries: Vec<LogEntry>) {
        let mut next_seq = 0u32;
        for entry in &entries {
            next_seq = next_seq.max(entry.seq.saturating_add(1));
        }
        *self.inner.next_seq.lock().unwrap() = next_seq;
        *self.inner.events.lock().unwrap() = entries;
    }

    /// Check whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Subscribe to new events. Returns a disposer handle.
    pub fn subscribe<F>(&self, callback: F) -> Arc<dyn Disposable>
    where
        F: Fn(&SessionEvent, u32) + Send + Sync + 'static,
    {
        let mut subscribers = self.inner.subscribers.lock().unwrap();
        subscribers.push(Box::new(callback));
        // Note: full disposer support requires index tracking; simplified here.
        Arc::new(NoopDisposable)
    }

    /// Project model-visible messages from the event log.
    ///
    /// `user/message`, `assistant/message`, the `tool/call` + `tool/result`
    /// pairs, `plan/rejected` and compaction summaries enter model history.
    /// Everything else serves UI replay and audit only.
    pub fn derive_messages(&self) -> Vec<crate::projection::ProjectedMessage> {
        let events = self.inner.events.lock().unwrap();
        crate::projection::derive_messages(&events)
    }

    /// Rewind this session to `to_seq`, recording the revert on the log.
    ///
    /// Dropped events leave both the log and the projected model history. The
    /// revert is appended as [`SessionEvent::SessionReverted`] so the rollback
    /// is itself auditable — a reviewer can see that the session was rewound and
    /// where to. Appending also drives the persistence subscription, so the
    /// file catches up immediately (a bare [`Session::truncate_to`] would wait
    /// for the next append).
    ///
    /// Returns how many events were dropped.
    pub fn revert_to(&self, to_seq: u32) -> u32 {
        let before = self.len() as u32;
        self.truncate_to(to_seq);
        let reverted = before.saturating_sub(self.len() as u32);
        if reverted > 0 {
            self.append(SessionEvent::SessionReverted { to_seq, reverted });
        }
        reverted
    }
}

// Re-export TurnEndReason so it is accessible from the crate root.
