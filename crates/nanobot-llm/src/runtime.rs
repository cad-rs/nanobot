//! LlmRuntime: provider routing for the LLM adapter layer (design-harness 10.5).
//!
//! The runtime holds a set of named adapters and routes each request to
//! the appropriate adapter based on a provider key. This is the only
//! type the agent loop and session layer interact with — they never
//! see concrete adapter types.

use crate::adapter::{AdapterStream, LlmAdapter, LlmRequest};
use crate::error::LlmError;
use std::collections::HashMap;
use std::sync::Arc;

/// Routes LLM requests to the correct adapter based on a provider key.
///
/// The runtime is cheap to clone (it is `Arc`-based internally). Adapters
/// are stored as `Arc<dyn LlmAdapter>` so the runtime itself is `Send + Sync`.
#[derive(Clone, Default)]
pub struct LlmRuntime {
    adapters: HashMap<String, Arc<dyn LlmAdapter>>,
}

impl LlmRuntime {
    /// Create an empty runtime with no adapters registered.
    pub fn new() -> Self {
        Self::default()
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
    pub fn stream<'a>(&'a self, provider_key: &'a str, request: LlmRequest) -> AdapterStream<'a> {
        match self.adapters.get(provider_key) {
            Some(adapter) => adapter.stream(request),
            None => Box::pin(futures_util::stream::once(async move {
                Err(LlmError::UnknownProvider(provider_key.to_string()))
            })),
        }
    }
}
