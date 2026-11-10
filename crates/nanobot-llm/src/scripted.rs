//! ScriptedAdapter: replays a scripted sequence of chunk streams for CI and
//! unit tests (design-harness 10.5).
//!
//! Zero network, zero model. The adapter owns a queue of *responses*; each
//! call to [`LlmAdapter::stream`] pops the next response and yields its
//! chunks in order. Once the queue is drained the stream ends immediately
//! (yields no chunks), which is what lets the agent loop's step cycle
//! terminate instead of replaying the same tool call forever.

use crate::adapter::{AdapterStream, LlmAdapter, LlmRequest};
use crate::chunk::StreamChunk;
use crate::error::LlmError;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// One scripted LLM response: the chunks it streams, in order.
pub type ScriptedResponse = Vec<Result<StreamChunk, LlmError>>;

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
