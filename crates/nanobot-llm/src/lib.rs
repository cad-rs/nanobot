//! nanobot-llm: the LLM provider adapter seam.
//!
//! This crate owns the streaming vocabulary (`StreamChunk`, `BlockAssembler`),
//! the `LlmAdapter` trait, and the built-in adapter implementations
//! (OpenAI-compatible, Scripted). The agent loop and session layer depend
//! only on the `LlmAdapter` trait and `LlmRuntime` — never on concrete
//! provider types.
//!
//! ## Dual-form same-source
//!
//! The provider base uses `reqwest` (default-features=false, stream) +
//! `eventsource-stream` (default-features=false) + `futures-core`.
//! The same code compiles for native (hyper) and wasm (browser fetch via
//! `wasm-streams`) — no platform-conditional compilation.
//!
//! ## Tokio discipline
//!
//! This crate has no tokio runtime entry. The async runtime is provided
//! by the host (desktop gpui / web `wasm_bindgen_futures::spawn_local`).
//! Only `futures-core` (the `Stream` trait) is used directly.
//!
//! Provenance: extracted from the ocad design harness (see
//! `docs/nanobot-architecture.md` in rcad-pro).

pub mod adapter;
pub mod assembler;
pub mod catalog;
pub mod chunk;
pub mod cost;
pub mod error;
#[cfg(feature = "providers")]
pub mod openai;
pub mod retry;
pub mod runtime;
pub mod scripted;

pub use adapter::{
    LlmAdapter, LlmRequest, LlmResponse, Message, Role, ToolCallRef, ToolDefinition, Usage,
};
pub use assembler::{AssembledToolCall, BlockAssembler, ResponseBlock};
pub use catalog::{DEFAULT_PROVIDER, ModelCatalog, ModelRoute};
pub use chunk::{StopReason, StreamChunk, ToolCallDelta};
pub use cost::{BudgetDecision, ModelAccount, Pricing, UsageAccount};
pub use error::LlmError;
#[cfg(feature = "providers")]
pub use openai::OpenAiCompatibleAdapter;
pub use retry::{RetryOn, RetryPolicy};
pub use runtime::LlmRuntime;
pub use scripted::{ScriptedAdapter, ScriptedResponse};
