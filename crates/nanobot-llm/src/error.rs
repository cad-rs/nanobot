//! Error types for the LLM provider adapter layer (design-harness 10.5).

use thiserror::Error;

/// Errors that can occur during LLM streaming operations.
#[derive(Debug, Clone, Error)]
pub enum LlmError {
    /// HTTP request failed (native: hyper; wasm: fetch).
    #[error("HTTP request failed: {0}")]
    Http(String),

    /// SSE stream parsing failed.
    #[error("SSE stream error: {0}")]
    Sse(String),

    /// Failed to parse a provider response chunk as JSON.
    #[error("JSON parse error: {0}")]
    Json(String),

    /// The provider returned an error payload.
    #[error("provider error: {0}")]
    Provider(String),

    /// The requested model or provider is not registered.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    /// No route is declared for the requested model id, and no fallback
    /// provider is registered to carry it.
    #[error("unknown model: {0}")]
    UnknownModel(String),
}

#[cfg(feature = "providers")]
impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        LlmError::Http(e.to_string())
    }
}

impl From<serde_json::Error> for LlmError {
    fn from(e: serde_json::Error) -> Self {
        LlmError::Json(e.to_string())
    }
}
