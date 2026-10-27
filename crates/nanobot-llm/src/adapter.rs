//! LlmAdapter trait: the provider-agnostic streaming seam (design-harness 10.5).
//!
//! Each provider (OpenAI-compatible, Anthropic, local ollama, scripted)
//! implements this trait. The agent loop and session layer depend only on
//! this trait — never on a concrete provider type.

use crate::chunk::StreamChunk;
use crate::error::LlmError;
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// A tool call an assistant message requested, in the shape providers carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRef {
    /// Call id; the matching tool result names it.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// JSON-encoded arguments, as the wire format wants them.
    pub arguments: String,
}

/// A single message in a conversation.
///
/// A tool-using turn is three messages, not one: the assistant message that
/// requests the calls ([`Message::tool_call`]), and one result per call
/// ([`Message::tool_result`]). Dropping the calls or the results leaves the
/// model unable to act on what a tool returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls this assistant message requested.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRef>,
    /// For [`Role::Tool`]: the call id this message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// A plain message with no tool payload.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    /// A user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    /// A plain assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    /// An assistant message requesting one tool call.
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCallRef {
                id: id.into(),
                name: name.into(),
                arguments: arguments.into(),
            }],
            tool_call_id: None,
        }
    }

    /// The result of a tool call, answering `id`.
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(id.into()),
        }
    }
}

/// Role of a message sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    System,
    User,
    Assistant,
    /// The result of a tool call the assistant requested.
    Tool,
}

/// A tool definition exposed to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters_schema: serde_json::Value,
}

/// A complete LLM request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Model identifier (e.g. "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,
    /// Conversation history.
    pub messages: Vec<Message>,
    /// Available tools (empty if none).
    pub tools: Vec<ToolDefinition>,
    /// Sampling temperature (provider may ignore).
    pub temperature: Option<f32>,
    /// Maximum tokens to generate (provider may ignore).
    pub max_tokens: Option<u32>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// The stream type returned by `LlmAdapter::stream`.
///
/// On native it is `Send` (the adapter is shared across threads). On
/// wasm the adapter runs in a single-threaded Worker, so the stream
/// does not need `Send` — and reqwest-on-wasm cannot guarantee it.
#[cfg(not(target_family = "wasm"))]
pub type AdapterStream<'a> = Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send + 'a>>;

/// wasm: no `Send` bound (single-threaded Worker, reqwest-on-wasm stream
/// is not `Send`).
#[cfg(target_family = "wasm")]
pub type AdapterStream<'a> = Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + 'a>>;

/// The provider-agnostic streaming adapter trait.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// threads on desktop and used within a single-threaded wasm Worker.
pub trait LlmAdapter: Send + Sync {
    /// Stream a completion request, yielding chunks as they arrive.
    fn stream<'a>(&'a self, request: LlmRequest) -> AdapterStream<'a>;
}
