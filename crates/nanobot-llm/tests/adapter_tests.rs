//! Integration tests for nanobot-llm: ScriptedAdapter, BlockAssembler, LlmRuntime.

use futures_util::StreamExt;
use nanobot_llm::{
    BlockAssembler, LlmAdapter, LlmError, LlmRequest, LlmRuntime, Message, OpenAiCompatibleAdapter,
    Role, StreamChunk, ToolCallDelta,
};

/// A one-shot HTTP server that answers every request with `status` and `body`.
fn serve_once(status_line: &str, body: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let status_line = status_line.to_string();
    let handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0_u8; 4096];
        let _ = stream.read(&mut buf);
        let response = format!(
            "{status_line}\r\nContent-Type: application/json\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    (format!("http://{addr}/v1/"), handle)
}

/// Drive a future on a current-thread tokio runtime.
///
/// reqwest needs a reactor, so the network tests cannot use a bare
/// `futures_executor::block_on`.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(future)
}

#[test]
fn a_non_2xx_status_is_an_http_error_not_an_empty_turn() {
    // A 404 used to be read as an SSE body: the stream either died with a
    // confusing "missing choices" error or produced a silent empty turn the
    // harness could not tell apart from the model saying nothing.
    let (base_url, server) = serve_once(
        "HTTP/1.1 404 Not Found",
        r#"{"error":{"message":"no such model"}}"#,
    );
    let adapter = OpenAiCompatibleAdapter::new(base_url, "key");
    let mut stream = adapter.stream(LlmRequest {
        model: "m".into(),
        messages: vec![Message::new(Role::User, "hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    });

    let mut first = None;
    block_on(async {
        if let Some(chunk) = stream.next().await {
            first = Some(chunk);
        }
    });
    server.join().unwrap();

    let err = first
        .expect("the request produces exactly one item")
        .expect_err("a 404 is an error, not an empty turn");
    match err {
        LlmError::Http(message) => {
            assert!(message.contains("404"), "{message}");
            assert!(message.contains("no such model"), "{message}");
        }
        other => panic!("expected LlmError::Http, got {other:?}"),
    }
}

#[test]
fn a_success_status_is_still_streamed() {
    // The guard must not reject a real 200: a minimal SSE stream still flows.
    let (base_url, server) = serve_once("HTTP/1.1 200 OK", "data: [DONE]\n\n");
    let adapter = OpenAiCompatibleAdapter::new(base_url, "key");
    let mut stream = adapter.stream(LlmRequest {
        model: "m".into(),
        messages: vec![],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    });

    let mut items = Vec::new();
    block_on(async {
        while let Some(chunk) = stream.next().await {
            items.push(chunk);
        }
    });
    server.join().unwrap();

    // The 200 path yields no error (the [DONE] sentinel ends the stream).
    assert!(
        items.iter().all(|c| c.is_ok()),
        "a 200 must stream, got {items:?}"
    );
}

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
