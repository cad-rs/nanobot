//! ScriptedAdapter: replays a scripted sequence of chunk streams for CI and
//! unit tests (design-harness 10.5).
//!
//! Zero network, zero model. The adapter owns a queue of *responses*; each
//! call to [`LlmAdapter::stream`] pops the next response and yields its
//! chunks in order. Once the queue is drained the stream ends immediately
//! (yields no chunks), which is what lets the agent loop's step cycle
//! terminate instead of replaying the same tool call forever.

use crate::adapter::{AdapterStream, LlmAdapter, LlmRequest};
use crate::chunk::{StreamChunk, ToolCallDelta};
use crate::error::LlmError;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// One scripted LLM response: the chunks it streams, in order.
pub type ScriptedResponse = Vec<Result<StreamChunk, LlmError>>;

/// The chunk sequence for a single tool call: an id+name delta followed by an
/// arguments delta.
fn tool_call_chunks(name: impl Into<String>, arguments: Value) -> ScriptedResponse {
    let name = name.into();
    let args_json = serde_json::to_string(&arguments).unwrap_or_default();
    vec![
        // First delta: id + name
        Ok(StreamChunk::ToolCall(ToolCallDelta {
            index: 0,
            id: Some("call_1".to_string()),
            name: Some(name),
            arguments_fragment: String::new(),
        })),
        // Second delta: arguments
        Ok(StreamChunk::ToolCall(ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments_fragment: args_json,
        })),
    ]
}

pub struct ScriptedAdapter {
    responses: Arc<Mutex<VecDeque<ScriptedResponse>>>,
}

impl ScriptedAdapter {
    /// Create a new scripted adapter from a single response's chunk vector.
    pub fn new(chunks: Vec<Result<StreamChunk, LlmError>>) -> Self {
        Self::from_responses(vec![chunks])
    }

    /// Create a new scripted adapter that replays `responses` in order, one
    /// response per [`LlmAdapter::stream`] call.
    pub fn from_responses(responses: Vec<ScriptedResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }

    /// Create a simple text-only script that yields the given text as a
    /// single `StreamChunk::Text`.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![Ok(StreamChunk::Text(text.into()))])
    }

    /// Create a script that yields the given chunks in order, then finishes.
    pub fn from_text_fragments(fragments: Vec<&str>) -> Self {
        let chunks = fragments
            .into_iter()
            .map(|f| Ok(StreamChunk::Text(f.to_string())))
            .collect();
        Self::new(chunks)
    }

    /// Create a script that emits a single tool call with the given name and
    /// arguments, then finishes. This is the primary building block for E2E
    /// tests: the scripted LLM emits tool calls that the agent dispatches
    /// through the engine.
    pub fn tool_call(name: impl Into<String>, arguments: Value) -> Self {
        Self::new(tool_call_chunks(name, arguments))
    }

    /// Create a script that emits multiple tool calls within a single
    /// response, then finishes. Each tool call is emitted as a pair of deltas
    /// (id+name, then arguments).
    pub fn tool_calls(calls: Vec<(String, Value)>) -> Self {
        let mut chunks = Vec::new();
        for (i, (name, arguments)) in calls.into_iter().enumerate() {
            let index = i as u32;
            let args_json = serde_json::to_string(&arguments).unwrap_or_default();
            chunks.push(Ok(StreamChunk::ToolCall(ToolCallDelta {
                index,
                id: Some(format!("call_{}", index + 1)),
                name: Some(name),
                arguments_fragment: String::new(),
            })));
            chunks.push(Ok(StreamChunk::ToolCall(ToolCallDelta {
                index,
                id: None,
                name: None,
                arguments_fragment: args_json,
            })));
        }
        Self::new(chunks)
    }

    /// Create a script that emits text followed by a tool call.
    pub fn text_then_tool_call(
        text: impl Into<String>,
        name: impl Into<String>,
        arguments: Value,
    ) -> Self {
        let mut chunks = vec![Ok(StreamChunk::Text(text.into()))];
        chunks.extend(tool_call_chunks(name, arguments));
        Self::new(chunks)
    }
}

impl LlmAdapter for ScriptedAdapter {
    fn stream<'a>(&'a self, _request: LlmRequest) -> AdapterStream<'a> {
        // Pop the next scripted response. An empty queue yields an empty
        // stream, so the consumer's step loop terminates.
        let chunks = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        Box::pin(futures_util::stream::iter(chunks))
    }
}
