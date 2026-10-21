//! StreamChunk: the streaming delta vocabulary shared by all LLM adapters
//! (design-harness 10.5).
//!
//! Each adapter converts its provider-specific streaming format into a
//! sequence of `StreamChunk` values. The `BlockAssembler` on the consumer
//! side reassembles these into complete response blocks.

use serde::{Deserialize, Serialize};

/// A single streaming delta from an LLM provider.
///
/// The three variants cover the standard streaming content types:
/// reasoning (chain-of-thought), plain text, and tool-call invocations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamChunk {
    /// Incremental reasoning / chain-of-thought text.
    Reasoning(String),
    /// Incremental response text content.
    Text(String),
    /// A tool-call delta (index, id, name, arguments fragment).
    ToolCall(ToolCallDelta),
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
