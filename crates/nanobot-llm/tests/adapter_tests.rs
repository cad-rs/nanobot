//! Integration tests for nanobot-llm: ScriptedAdapter, BlockAssembler, LlmRuntime.

use futures_util::StreamExt;
use nanobot_llm::{BlockAssembler, LlmError, LlmRuntime, Message, Role, StreamChunk, ToolCallDelta};

#[test]
fn scripted_adapter_yields_text() {
    let adapter = nanobot_llm::ScriptedAdapter::text("hello world");
    let mut runtime = LlmRuntime::new();
    runtime.register("scripted", std::sync::Arc::new(adapter));

    let mut stream = runtime.stream(
        "scripted",
        nanobot_llm::LlmRequest {
            model: "test".into(),
            messages: vec![Message::new(Role::User, "hi")],
            tools: vec![],
            temperature: None,
            max_tokens: None,
        },
    );

    let mut collected = Vec::new();
    futures_executor::block_on(async {
        while let Some(chunk) = stream.next().await {
            if let Ok(c) = chunk {
                collected.push(c);
            }
        }
    });
    assert_eq!(collected, vec![StreamChunk::Text("hello world".into())]);
}

#[test]
fn unknown_provider_yields_error() {
    let runtime = LlmRuntime::new();
    let mut stream = runtime.stream(
        "nope",
        nanobot_llm::LlmRequest {
            model: "test".into(),
            messages: vec![],
            tools: vec![],
            temperature: None,
            max_tokens: None,
        },
    );

    let mut got_err = false;
    futures_executor::block_on(async {
        while let Some(chunk) = stream.next().await {
            if let Err(LlmError::UnknownProvider(_)) = chunk {
                got_err = true;
            }
        }
    });
    assert!(got_err);
}

#[test]
fn block_assembler_reassembles_fragments() {
    let mut asm = BlockAssembler::new();
    let chunks = vec![
        StreamChunk::Text("Hello, ".into()),
        StreamChunk::Text("world!".into()),
    ];

    let mut result = None;
    for c in chunks {
        if let Some(block) = asm.push(c) {
            result = Some(block);
        }
    }
    // No completion signal yet.
    assert!(result.is_none());

    // Finish the block.
    let block = asm.finish().unwrap();
    assert_eq!(block.text, "Hello, world!");
    assert!(block.reasoning.is_empty());
    assert!(block.tool_calls.is_empty());
}

#[test]
fn block_assembler_reassembles_tool_calls() {
    let mut asm = BlockAssembler::new();
    asm.push(StreamChunk::ToolCall(ToolCallDelta {
        index: 0,
        id: Some("call_1".into()),
        name: Some("create_box".into()),
        arguments_fragment: String::from("{\"size\":"),
    }));
    asm.push(StreamChunk::ToolCall(ToolCallDelta {
        index: 0,
        id: None,
        name: None,
        arguments_fragment: String::from("10}"),
    }));

    let block = asm.finish().unwrap();
    assert_eq!(block.tool_calls.len(), 1);
    assert_eq!(block.tool_calls[0].id, "call_1");
    assert_eq!(block.tool_calls[0].name, "create_box");
    assert_eq!(block.tool_calls[0].arguments, "{\"size\":10}");
}
