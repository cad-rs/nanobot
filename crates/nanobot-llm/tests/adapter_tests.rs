//! Integration tests for nanobot-llm: ScriptedAdapter, BlockAssembler, LlmRuntime.

use futures_util::StreamExt;
use nanobot_llm::{
    BlockAssembler, LlmAdapter, LlmError, LlmRequest, LlmRuntime, Message, OpenAiCompatibleAdapter,
    RetryPolicy, Role, StreamChunk, ToolCallDelta,
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

/// A server that answers the first `failures` requests with `status_line` and
/// the rest with a 200 carrying `success_body`.
///
/// Returns how many requests it served, so a test can tell "retried" from "not
/// retried" apart. Non-blocking, so a client that stops early does not hang the
/// test.
fn serve_failures_then_success(
    failures: usize,
    status_line: &str,
    success_body: &'static str,
) -> (String, std::thread::JoinHandle<usize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let status_line = status_line.to_string();
    let handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut served = 0usize;
        let mut idle = 0usize;
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    idle = 0;
                    let mut buf = [0_u8; 4096];
                    let _ = stream.read(&mut buf);
                    let (line, body) = if served < failures {
                        (status_line.as_str(), "{}")
                    } else {
                        ("HTTP/1.1 200 OK", success_body)
                    };
                    let response = format!(
                        "{line}\r\nContent-Type: application/json\r\nConnection: close\r\n\
                         Content-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    served += 1;
                }
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    idle += 1;
                    if idle > 40 || served > failures + 4 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        served
    });
    (format!("http://{addr}/v1/"), handle)
}

/// A request that carries one user message.
fn request() -> LlmRequest {
    LlmRequest {
        model: "m".into(),
        messages: vec![Message::new(Role::User, "hi")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    }
}

#[test]
fn a_transient_failure_is_retried_and_then_succeeds() {
    let (base_url, server) = serve_failures_then_success(
        2,
        "HTTP/1.1 503 Service Unavailable",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\
         \"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    let adapter = OpenAiCompatibleAdapter::new(base_url, "key")
        .with_retry(RetryPolicy::transient(3))
        .with_sleep(std::sync::Arc::new(|_| {}));
    let mut stream = adapter.stream(request());

    let chunks = block_on(async {
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.push(chunk);
        }
        out
    });
    let served = server.join().unwrap();

    assert_eq!(served, 3, "two failures, then the retry succeeded");
    assert!(
        chunks
            .iter()
            .any(|chunk| matches!(chunk, Ok(StreamChunk::Text(text)) if text == "ok")),
        "the successful response reached the consumer: {chunks:?}"
    );
}

#[test]
fn a_4xx_is_the_providers_answer_and_is_not_retried() {
    let (base_url, server) =
        serve_failures_then_success(3, "HTTP/1.1 404 Not Found", r#"{"error":{}}"#);
    let adapter = OpenAiCompatibleAdapter::new(base_url, "key")
        .with_retry(RetryPolicy::transient(3))
        .with_sleep(std::sync::Arc::new(|_| {}));
    let mut stream = adapter.stream(request());

    let first = block_on(async { stream.next().await });
    let served = server.join().unwrap();

    assert_eq!(served, 1, "a 404 is not retried");
    match first {
        Some(Err(LlmError::Http(message))) => assert!(message.contains("404"), "{message}"),
        other => panic!("expected an HTTP error, got {other:?}"),
    }
}

#[test]
fn the_retry_budget_bounds_the_attempts() {
    // Every request fails, so the chain stops at the budget.
    let (base_url, server) =
        serve_failures_then_success(99, "HTTP/1.1 500 Internal Server Error", "{}");
    let adapter = OpenAiCompatibleAdapter::new(base_url, "key")
        .with_retry(RetryPolicy::transient(2))
        .with_sleep(std::sync::Arc::new(|_| {}));
    let mut stream = adapter.stream(request());

    let first = block_on(async { stream.next().await });
    let served = server.join().unwrap();

    assert_eq!(served, 3, "one attempt plus two retries");
    assert!(matches!(first, Some(Err(LlmError::Http(_)))), "got {first:?}");
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

    // No chunk marks a boundary: the block comes from `finish`.
    for c in chunks {
        asm.push(c);
    }

    let block = asm.finish().unwrap();
    assert_eq!(block.text, "Hello, world!");
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
