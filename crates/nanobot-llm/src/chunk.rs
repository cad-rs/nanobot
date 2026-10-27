//! StreamChunk: the streaming delta vocabulary shared by all LLM adapters
//! (design-harness 10.5).
//!
//! Each adapter converts its provider-specific streaming format into a
//! sequence of `StreamChunk` values. The `BlockAssembler` on the consumer
//! side reassembles these into complete response blocks.

use serde::{Deserialize, Serialize};

use crate::adapter::Usage;

/// A single streaming delta from an LLM provider.
///
/// The variants cover the standard streaming content types: reasoning
/// (chain-of-thought), plain text, tool-call invocations, and the token
/// accounting a provider reports at the end of a response. Usage rides the same
/// stream as content so a consumer that assembles the response also gets the
/// numbers to bill and budget with.
///
/// [`StreamChunk::Stop`] carries the provider's `finish_reason`. It is what
/// lets a consumer tell a complete answer from one the provider cut off at the
/// token limit — and refuse to act on the truncated one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamChunk {
    /// Incremental reasoning / chain-of-thought text.
    Reasoning(String),
    /// Incremental response text content.
    Text(String),
    /// A tool-call delta (index, id, name, arguments fragment).
    ToolCall(ToolCallDelta),
    /// Token accounting for the response so far (providers send it last).
    Usage(Usage),
    /// The response ended, and here is why (the provider's `finish_reason`).
    Stop(StopReason),
}

/// Why a response stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The model stopped on its own with a complete answer.
    Stop,
    /// The model requested tools and stopped.
    ToolUse,
    /// The provider cut the response off at the token limit: the text, and any
    /// tool arguments, are incomplete.
    Length,
    /// The provider stopped on a content policy.
    ContentFilter,
    /// The provider stopped for a reason this build does not name.
    Other,
    /// The stream was cut by the client.
    Aborted,
}

impl StopReason {
    /// Map a provider's `finish_reason` onto this vocabulary.
    ///
    /// Unknown values become [`StopReason::Other`] rather than an error: a
    /// provider inventing a new reason must not break the stream.
    pub fn from_wire(finish_reason: &str) -> Self {
        match finish_reason {
            "stop" => StopReason::Stop,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::Length,
            "content_filter" => StopReason::ContentFilter,
            _ => StopReason::Other,
        }
    }

    /// Whether the response was cut off before the model finished.
    pub fn is_truncated(self) -> bool {
        matches!(self, StopReason::Length)
    }
}

/// Incremental update for a single tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallDelta {
    /// Index of the tool call within the response.
    pub index: u32,
    /// Tool call identifier (present in the first delta for a given index).
    pub id: Option<String>,
    /// Tool/function name (present in the first or second delta).
    pub name: Option<String>,
    /// Fragment of the JSON-encoded arguments string.
    pub arguments_fragment: String,
}
