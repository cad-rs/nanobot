//! nanobot telemetry seam: span and event contracts with a no-op default.
//!
//! The agent loop, the LLM runtime and the tool pipeline emit spans at their
//! request boundaries. Instrumentation is a *choice*: this crate is a
//! dependency-free leaf, so a consumer that wants no observability pays
//! nothing — [`NOOP_TELEMETRY_CONTEXT`] does nothing and allocates nothing.
//!
//! A consumer that does want observability implements [`TelemetryContext`]
//! (bridging into its own tracing stack) and hands it to the harness. Tests
//! use the `testing` feature's `RecordingTelemetryContext` to assert on the
//! emitted spans **without a global subscriber** — the reason this seam is a
//! trait rather than a global.
//!
//! ## Why a whole crate
//!
//! The runtime crates call into telemetry at request boundaries. Putting those
//! calls behind a trait in a dependency-free leaf means:
//!
//! - no tracing backend, exporter or OpenTelemetry dependency reaches a user
//!   who does not want one;
//! - the default is monomorphic and free;
//! - a product can assert on emitted spans in its own tests.
//!
//! ## Emitting a span
//!
//! ```
//! use nanobot_telemetry::{SpanKind, SpanOptions, TelemetryContext, NOOP_TELEMETRY_CONTEXT};
//!
//! // The default: nothing is recorded, and nothing is allocated.
//! let span = NOOP_TELEMETRY_CONTEXT.start_span(
//!     SpanOptions::new("provider.request", SpanKind::ProviderRequest),
//! );
//! // Dropping the guard ends the span.
//! drop(span);
//! ```

/// What a span measures.
///
/// The framework emits these at its request boundaries; a product's own
/// context may record them under different names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpanKind {
    /// One turn of the agent loop.
    AgentTurn,
    /// One step (one model request and its tool dispatch).
    AgentStep,
    /// One tool call.
    Tool,
    /// One provider request, from the first chunk to the last.
    ProviderRequest,
}

/// The identity of a span to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanOptions {
    /// Stable span name (`agent.turn`, `tool.execute`, ...).
    pub name: String,
    /// What the span measures.
    pub kind: SpanKind,
}

impl SpanOptions {
    /// Describe a span to start.
    pub fn new(name: impl Into<String>, kind: SpanKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }
}

/// The RAII handle for an open span. Dropping it ends the span.
///
/// A context implementation builds one with [`SpanGuard::new`]; the no-op
/// default uses [`SpanGuard::noop`]. The guard owns its end callback so it can
/// be returned from `start_span(&self)` without borrowing the context — the
/// context is usually shared as `Arc<dyn TelemetryContext>` and outlives the
/// span.
pub struct SpanGuard {
    end: Option<Box<dyn FnOnce() + Send>>,
}

impl SpanGuard {
    /// A guard that does nothing on drop.
    pub fn noop() -> Self {
        Self { end: None }
    }

    /// A guard that runs `end` exactly once, when dropped.
    pub fn new(end: impl FnOnce() + Send + 'static) -> Self {
        Self {
            end: Some(Box::new(end)),
        }
    }

    /// Detach the guard: leave the span open. The caller now owns ending it,
    /// which is the escape hatch for a span that must outlive the scope that
    /// started it.
    pub fn detach(mut self) {
        self.end = None;
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        if let Some(end) = self.end.take() {
            end();
        }
    }
}

/// The seam a consumer implements to receive spans and events.
///
/// Implementations are `Send + Sync` and are shared as
/// `Arc<dyn TelemetryContext>`. The default is [`NOOP_TELEMETRY_CONTEXT`].
pub trait TelemetryContext: Send + Sync {
    /// Start a span. The returned guard ends the span on drop.
    fn start_span(&self, options: SpanOptions) -> SpanGuard;

    /// Record an instantaneous event (no duration).
    fn record_event(&self, name: &str, kind: SpanKind);
}

/// The default context: records nothing and allocates nothing.
pub struct NoopTelemetryContext;

impl TelemetryContext for NoopTelemetryContext {
    fn start_span(&self, _options: SpanOptions) -> SpanGuard {
        SpanGuard::noop()
    }

    fn record_event(&self, _name: &str, _kind: SpanKind) {}
}

/// The default context as a value, for `Arc::new(NOOP_TELEMETRY_CONTEXT)`.
pub static NOOP_TELEMETRY_CONTEXT: NoopTelemetryContext = NoopTelemetryContext;

/// An in-memory recorder for tests.
///
/// Enable with the `testing` feature. A test asserts on the spans the loop
/// emitted rather than mocking a tracing subscriber.
#[cfg(feature = "testing")]
pub mod testing {
    use super::{SpanGuard, SpanKind, SpanOptions, TelemetryContext};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// Which part of a span's life a [`Record`] describes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Phase {
        /// The span started.
        Start,
        /// The span ended (its guard dropped).
        End,
        /// An instantaneous event (not a span).
        Event,
    }

    /// One recorded thing: a span start, a span end, or an event.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Record {
        /// The span id; `None` for an event.
        pub id: Option<u64>,
        /// Stable name.
        pub name: String,
        /// What it measures.
        pub kind: SpanKind,
        /// Which part of the life this is.
        pub phase: Phase,
    }

    /// A [`TelemetryContext`] that records everything into memory.
    ///
    /// Cheap to clone (the records are shared), so a test can hold one handle
    /// and hand another to the harness.
    #[derive(Clone)]
    pub struct RecordingTelemetryContext {
        records: Arc<Mutex<Vec<Record>>>,
        next_id: Arc<AtomicU64>,
    }

    impl RecordingTelemetryContext {
        /// An empty recorder.
        pub fn new() -> Self {
            Self {
                records: Arc::new(Mutex::new(Vec::new())),
                next_id: Arc::new(AtomicU64::new(1)),
            }
        }

        /// Everything recorded so far, in order.
        pub fn records(&self) -> Vec<Record> {
            self.records.lock().unwrap().clone()
        }

        /// The names of the spans that started, in order.
        pub fn span_names(&self) -> Vec<String> {
            self.records()
                .into_iter()
                .filter(|r| r.phase == Phase::Start)
                .map(|r| r.name)
                .collect()
        }

        /// Forget everything recorded.
        pub fn clear(&self) {
            self.records.lock().unwrap().clear();
        }
    }

    impl Default for RecordingTelemetryContext {
        fn default() -> Self {
            Self::new()
        }
    }

    impl TelemetryContext for RecordingTelemetryContext {
        fn start_span(&self, options: SpanOptions) -> SpanGuard {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            self.records.lock().unwrap().push(Record {
                id: Some(id),
                name: options.name.clone(),
                kind: options.kind,
                phase: Phase::Start,
            });
            let records = Arc::clone(&self.records);
            SpanGuard::new(move || {
                records.lock().unwrap().push(Record {
                    id: Some(id),
                    name: options.name,
                    kind: options.kind,
                    phase: Phase::End,
                });
            })
        }

        fn record_event(&self, name: &str, kind: SpanKind) {
            self.records.lock().unwrap().push(Record {
                id: None,
                name: name.to_string(),
                kind,
                phase: Phase::Event,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_silent() {
        // The contract is the absence of effect: nothing is recorded and
        // nothing is allocated. A consumer that never installs a context pays
        // nothing, which is what makes instrumentation a choice.
        let span = NOOP_TELEMETRY_CONTEXT.start_span(SpanOptions::new("x", SpanKind::AgentTurn));
        assert!(span.end.is_none());
        drop(span);
        NOOP_TELEMETRY_CONTEXT.record_event("e", SpanKind::Tool);
    }

    #[test]
    fn a_detached_guard_never_ends_the_span() {
        // The escape hatch for a span that must outlive the scope that started
        // it: detaching leaves it open, and the recorder never sees an end.
        let rec = testing_recorder();
        {
            let guard = rec.start_span(SpanOptions::new("open", SpanKind::Tool));
            SpanGuard::detach(guard);
        }
        assert_eq!(rec.span_names(), vec!["open".to_string()]);
        assert_eq!(rec.records().len(), 1);
    }

    #[cfg(feature = "testing")]
    fn testing_recorder() -> testing::RecordingTelemetryContext {
        testing::RecordingTelemetryContext::new()
    }

    #[cfg(feature = "testing")]
    #[test]
    fn the_recorder_pairs_every_start_with_an_end() {
        let rec = testing_recorder();
        drop(rec.start_span(SpanOptions::new("agent.turn", SpanKind::AgentTurn)));
        rec.record_event("custom", SpanKind::Tool);

        let records = rec.records();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].phase, testing::Phase::Start);
        assert_eq!(records[0].kind, SpanKind::AgentTurn);
        assert_eq!(records[1].phase, testing::Phase::End);
        assert_eq!(records[2].phase, testing::Phase::Event);
        assert_eq!(rec.span_names(), vec!["agent.turn".to_string()]);
    }
}
