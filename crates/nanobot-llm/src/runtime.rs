//! LlmRuntime: provider routing for the LLM adapter layer (design-harness 10.5).
//!
//! The runtime holds a set of named adapters and routes each request to
//! the appropriate adapter based on a provider key. This is the only
//! type the agent loop and session layer interact with — they never
//! see concrete adapter types.
//!
//! It also owns the [`TelemetryContext`] the provider request spans are
//! emitted through. The default records nothing, so a consumer that wants no
//! observability pays nothing.

use crate::adapter::{AdapterStream, LlmAdapter, LlmRequest};
use crate::chunk::StreamChunk;
use crate::error::LlmError;
use futures_core::Stream;
use nanobot_telemetry::{NoopTelemetryContext, SpanKind, SpanOptions, TelemetryContext};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Routes LLM requests to the correct adapter based on a provider key.
///
/// The runtime is cheap to clone (it is `Arc`-based internally). Adapters
/// are stored as `Arc<dyn LlmAdapter>` so the runtime itself is `Send + Sync`.
#[derive(Clone)]
pub struct LlmRuntime {
    adapters: HashMap<String, Arc<dyn LlmAdapter>>,
    telemetry: Arc<dyn TelemetryContext>,
}

impl Default for LlmRuntime {
    fn default() -> Self {
        Self {
            adapters: HashMap::new(),
            telemetry: Arc::new(NoopTelemetryContext),
        }
    }
}

impl LlmRuntime {
    /// Create an empty runtime with no adapters registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the telemetry context for provider request spans.
    ///
    /// The default is the no-op context: nothing is recorded, and the
    /// instrumentation is a choice rather than a dependency.
    pub fn with_telemetry(mut self, telemetry: Arc<dyn TelemetryContext>) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// Replace the telemetry context (the assembled form of
    /// [`LlmRuntime::with_telemetry`]).
    pub fn set_telemetry(&mut self, telemetry: Arc<dyn TelemetryContext>) {
        self.telemetry = telemetry;
    }

    /// Register an adapter under the given provider key.
    ///
    /// Returns the previously registered adapter for that key, if any.
    pub fn register(
        &mut self,
        provider: impl Into<String>,
        adapter: Arc<dyn LlmAdapter>,
    ) -> Option<Arc<dyn LlmAdapter>> {
        self.adapters.insert(provider.into(), adapter)
    }

    /// Deregister an adapter by provider key.
    pub fn deregister(&mut self, provider: &str) -> Option<Arc<dyn LlmAdapter>> {
        self.adapters.remove(provider)
    }

    /// Check whether a provider key has a registered adapter.
    pub fn has(&self, provider: &str) -> bool {
        self.adapters.contains_key(provider)
    }

    /// List all registered provider keys.
    pub fn providers(&self) -> impl Iterator<Item = &String> {
        self.adapters.keys()
    }

    /// Stream a completion request through the adapter for `provider`.
    ///
    /// The `provider_key` is matched against registered keys. If no match
    /// is found, an [`LlmError::UnknownProvider`] is yielded as the first
    /// stream item.
    ///
    /// The returned stream is wrapped in a `provider.request` span: the span
    /// ends when the stream is exhausted (or dropped), so its duration is the
    /// request's, not the call that created it.
    pub fn stream<'a>(&'a self, provider_key: &'a str, request: LlmRequest) -> AdapterStream<'a> {
        match self.adapters.get(provider_key) {
            Some(adapter) => {
                let span = self
                    .telemetry
                    .start_span(SpanOptions::new("provider.request", SpanKind::ProviderRequest));
                let inner = adapter.stream(request);
                Box::pin(SpanStream {
                    inner,
                    _span: span,
                })
            }
            None => Box::pin(futures_util::stream::once(async move {
                Err(LlmError::UnknownProvider(provider_key.to_string()))
            })),
        }
    }
}

/// The adapter stream plus the span that must stay open for its whole life.
///
/// The span guard is a field rather than a local so the span ends when the
/// stream does — a `provider.request` span that ended at `stream()` would
/// measure nothing.
struct SpanStream<'a> {
    inner: AdapterStream<'a>,
    _span: nanobot_telemetry::SpanGuard,
}

impl<'a> Stream for SpanStream<'a> {
    type Item = Result<StreamChunk, LlmError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `SpanStream` is `Unpin` (both fields are), so `get_mut` is available
        // and the inner stream can be polled through its own `Pin`.
        let this = self.get_mut();
        this.inner.as_mut().poll_next(cx)
    }
}
