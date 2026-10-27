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
use crate::catalog::{ModelCatalog, ModelRoute, DEFAULT_PROVIDER};
use crate::chunk::StreamChunk;
use crate::error::LlmError;
use futures_core::Stream;
use nanobot_telemetry::{NoopTelemetryContext, SpanKind, SpanOptions, TelemetryContext};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Routes LLM requests to the correct adapter based on a model id.
///
/// The runtime is cheap to clone (it is `Arc`-based internally). Adapters
/// are stored as `Arc<dyn LlmAdapter>` so the runtime itself is `Send + Sync`.
///
/// A request names either a **provider key** ([`LlmRuntime::stream`], the
/// low-level routing a direct caller uses) or a **model id**
/// ([`LlmRuntime::stream_model`], which resolves through the [`ModelCatalog`]
/// the agent uses). A harness that declares no routes keeps the pre-catalog
/// behaviour: the model id is the wire model, and it routes to the
/// [`DEFAULT_PROVIDER`] adapter.
#[derive(Clone)]
pub struct LlmRuntime {
    adapters: HashMap<String, Arc<dyn LlmAdapter>>,
    catalog: ModelCatalog,
    telemetry: Arc<dyn TelemetryContext>,
}

impl Default for LlmRuntime {
    fn default() -> Self {
        Self {
            adapters: HashMap::new(),
            catalog: ModelCatalog::new(),
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

    /// Declare that `id` routes to `provider`, asking it for wire model `model`.
    ///
    /// Returns the previous route for `id`, if any.
    pub fn register_model(
        &mut self,
        id: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Option<ModelRoute> {
        self.catalog.register(id, provider, model)
    }

    /// The declared model catalog.
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }

    /// List all registered provider keys.
    pub fn providers(&self) -> impl Iterator<Item = &String> {
        self.adapters.keys()
    }

    /// List the declared model ids.
    pub fn models(&self) -> impl Iterator<Item = &String> {
        self.catalog.ids()
    }

    /// Resolve a model id to the route it declares.
    ///
    /// A catalog entry wins. With no entry, a [`DEFAULT_PROVIDER`] adapter
    /// carries the request with the model id as the wire model — the exact
    /// behaviour a harness had before the catalog existed, so registering an
    /// adapter under `"default"` and setting a model id keeps working.
    /// Otherwise there is nowhere to route it.
    pub fn resolve(&self, model_id: &str) -> Option<ModelRoute> {
        if let Some(route) = self.catalog.get(model_id) {
            return Some(route.clone());
        }
        if self.adapters.contains_key(DEFAULT_PROVIDER) {
            return Some(ModelRoute::new(DEFAULT_PROVIDER, model_id));
        }
        None
    }

    /// Stream a completion request straight through the adapter for
    /// `provider_key`.
    ///
    /// This is the low-level routing: it does not consult the catalog and does
    /// not rewrite the request's model. A product that calls the runtime
    /// directly (rather than going through the agent) uses this, so it keeps
    /// its exact pre-catalog meaning.
    pub fn stream<'a>(&'a self, provider_key: &'a str, request: LlmRequest) -> AdapterStream<'a> {
        match self.adapters.get(provider_key) {
            Some(adapter) => self.dispatch(adapter, request),
            None => Box::pin(futures_util::stream::once(async move {
                Err(LlmError::UnknownProvider(provider_key.to_string()))
            })),
        }
    }

    /// Stream a completion request for `model_id`, resolving it first.
    ///
    /// The catalog resolves the model id to a provider and a wire model; the
    /// adapter is selected by that provider key and asked for that wire model
    /// (the route's wire model wins over whatever the request carried, since
    /// the route is the authority). A model id with no route yields
    /// [`LlmError::UnknownModel`] as the first stream item; a route naming an
    /// unregistered provider yields [`LlmError::UnknownProvider`].
    ///
    /// This is what the agent loop uses, so a harness can route several model
    /// ids to several providers (and offer aliases whose wire id differs from
    /// the id the agent was pointed at).
    pub fn stream_model<'a>(&'a self, model_id: &'a str, request: LlmRequest) -> AdapterStream<'a> {
        let route = match self.resolve(model_id) {
            Some(route) => route,
            None => {
                return Box::pin(futures_util::stream::once(async move {
                    Err(LlmError::UnknownModel(model_id.to_string()))
                }))
            }
        };
        match self.adapters.get(&route.provider) {
            Some(adapter) => {
                let mut request = request;
                request.model = route.model;
                self.dispatch(adapter, request)
            }
            None => Box::pin(futures_util::stream::once(async move {
                Err(LlmError::UnknownProvider(route.provider.clone()))
            })),
        }
    }

    /// Drive one adapter request, wrapping it in the `provider.request` span.
    fn dispatch<'a>(
        &'a self,
        adapter: &'a Arc<dyn LlmAdapter>,
        request: LlmRequest,
    ) -> AdapterStream<'a> {
        let span = self
            .telemetry
            .start_span(SpanOptions::new("provider.request", SpanKind::ProviderRequest));
        let inner = adapter.stream(request);
        Box::pin(SpanStream {
            inner,
            _span: span,
        })
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
