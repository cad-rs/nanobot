//! BlockAssembler: reassembles a StreamChunk stream into complete response
//! blocks (design-harness 10.5).
//!
//! The accumulator is fed chunks one at a time (e.g. from a
//! `Stream<Item = Result<StreamChunk, LlmError>>`).
//!
//! The chunk vocabulary carries no completion signal, so a block boundary is
//! the **end of the stream**: [`BlockAssembler::push`] always absorbs, and
//! [`BlockAssembler::finish`] yields the one block. Supporting several blocks
//! per stream would need a completion chunk first (a provider that emits more
//! than one message per stream).

use crate::adapter::Usage;
use crate::chunk::{StreamChunk, ToolCallDelta};
use serde::{Deserialize, Serialize};

/// A fully assembled response block, ready for the agent loop to consume.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResponseBlock {
    /// Concatenated reasoning text (empty if the provider does not stream reasoning).
    pub reasoning: String,
    /// Concatenated response text.
    pub text: String,
    /// Assembled tool calls (index-ordered).
    pub tool_calls: Vec<AssembledToolCall>,
    /// Token accounting the provider reported for this response, if any.
    ///
    /// Providers that support it send it in the final chunk; a consumer uses it
    /// to bill, budget and record the request (see the `RequestHeader` session
    /// event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Whether this block represents the end of a logical response unit.
    pub is_complete: bool,
}

/// A fully assembled tool call with complete arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssembledToolCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    /// Fully assembled JSON-encoded arguments string.
    pub arguments: String,
}

/// Accumulates `StreamChunk` deltas into `ResponseBlock` values.
///
/// Feed chunks via [`BlockAssembler::push`]. When the assembler detects a
/// block boundary it returns the completed block; otherwise it returns
/// `None` (the chunk was absorbed into the current block).
#[derive(Debug, Default)]
pub struct BlockAssembler {
    reasoning: String,
    text: String,
    tool_calls: Vec<AssembledToolCall>,
    /// Token accounting reported by the provider, when it reports any.
    usage: Option<Usage>,
    /// Whether the last chunk signalled completion.
    complete: bool,
}

impl BlockAssembler {
    /// Create a new, empty assembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk into the accumulator.
    ///
    /// Always returns `None`: no chunk in the vocabulary marks a boundary, so
    /// the block is yielded by [`BlockAssembler::finish`] at end of stream.
    /// The `Option` return is kept for a future completion chunk.
    pub fn push(&mut self, chunk: StreamChunk) -> Option<ResponseBlock> {
        match chunk {
            StreamChunk::Reasoning(delta) => {
                self.reasoning.push_str(&delta);
            }
            StreamChunk::Text(delta) => {
                self.text.push_str(&delta);
            }
            StreamChunk::ToolCall(delta) => {
                self.push_tool_call(delta);
            }
            StreamChunk::Usage(usage) => {
                self.usage = Some(usage);
            }
        }

        if self.complete {
            Some(self.take_block())
        } else {
            None
        }
    }

    /// Signal end-of-stream: yield whatever has been accumulated.
    ///
    /// A stream that carried only usage still yields a block: the accounting is
    /// the point of that response, and dropping it would lose the numbers.
    pub fn finish(mut self) -> Option<ResponseBlock> {
        if self.reasoning.is_empty()
            && self.text.is_empty()
            && self.tool_calls.is_empty()
            && self.usage.is_none()
        {
            None
        } else {
            self.complete = true;
            Some(self.take_block())
        }
    }

    // -- internal --

    fn push_tool_call(&mut self, delta: ToolCallDelta) {
        // Find or create the tool call at the given index.
        if let Some(tc) = self
            .tool_calls
            .iter_mut()
            .find(|tc| tc.index == delta.index)
        {
            if let Some(id) = delta.id {
                tc.id = id;
            }
            if let Some(name) = delta.name {
                tc.name = name;
            }
            tc.arguments.push_str(&delta.arguments_fragment);
        } else {
            self.tool_calls.push(AssembledToolCall {
                index: delta.index,
                id: delta.id.unwrap_or_default(),
                name: delta.name.unwrap_or_default(),
                arguments: delta.arguments_fragment,
            });
        }
        // Keep tool_calls sorted by index.
        self.tool_calls.sort_by_key(|tc| tc.index);
    }

    fn take_block(&mut self) -> ResponseBlock {
        let block = ResponseBlock {
            reasoning: std::mem::take(&mut self.reasoning),
            text: std::mem::take(&mut self.text),
            tool_calls: std::mem::take(&mut self.tool_calls),
            usage: self.usage.take(),
            is_complete: self.complete,
        };
        self.complete = false;
        block
    }
}

// Re-export ToolCallDelta so consumers can reference it via assembler.
pub use crate::chunk::ToolCallDelta as ToolCallDeltaAlias;

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn usage_is_collected_into_the_block() {
        let mut assembler = BlockAssembler::new();
        assembler.push(StreamChunk::Text("hi".to_string()));
        assembler.push(StreamChunk::Usage(Usage {
            prompt_tokens: 3,
            completion_tokens: 4,
            total_tokens: 7,
        }));
        let block = assembler.finish().expect("a block");
        assert_eq!(block.text, "hi");
        assert!(block.usage.is_some());
    }
}
