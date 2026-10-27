//! OpenAI-compatible adapter: streams chat completions over SSE using
//! `reqwest` + `eventsource-stream` (design-harness 10.5).
//!
//! The same code path serves native (hyper) and wasm (browser fetch via
//! `wasm-streams`) — no platform-conditional compilation.

use crate::adapter::{AdapterStream, LlmAdapter, LlmRequest, Role, Usage};
use crate::chunk::{StopReason, StreamChunk, ToolCallDelta};
use crate::retry::RetryPolicy;
use crate::error::LlmError;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::sync::Arc;

/// The byte stream returned by `reqwest::Response::bytes_stream()`.
///
/// On native this is `BoxStream` (Send). On wasm the underlying stream
/// is not Send, so the bound is dropped.
#[cfg(not(target_family = "wasm"))]
type ByteStream<'a> = futures_core::stream::BoxStream<'a, Result<bytes::Bytes, reqwest::Error>>;

#[cfg(target_family = "wasm")]
type ByteStream<'a> = std::pin::Pin<
    Box<dyn futures_core::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + 'a>,
>;

/// OpenAI-compatible chat completions adapter.
///
/// Works with any API that follows the OpenAI chat completions format:
/// OpenAI, DeepSeek, Ollama (OpenAI-compatible mode), vLLM, etc.
pub struct OpenAiCompatibleAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    retry: RetryPolicy,
    /// Sleeps for a retry backoff. Injected because the kernel carries no
    /// timer: the default blocks the current thread, which is right for a CLI
    /// and a test and wrong for a UI host.
    sleep: Arc<dyn Fn(std::time::Duration) + Send + Sync>,
}

/// The default backoff sleep.
///
/// Blocks the current thread. A host that drives the loop on its own executor
/// injects a real timer with [`OpenAiCompatibleAdapter::with_sleep`].
fn blocking_sleep(duration: std::time::Duration) {
    #[cfg(not(target_family = "wasm"))]
    std::thread::sleep(duration);
    #[cfg(target_family = "wasm")]
    let _ = duration;
}

impl OpenAiCompatibleAdapter {
    /// Create a new adapter targeting `{base_url}` with the given API key.
    ///
    /// `base_url` should end with a `/` (e.g. `"https://api.openai.com/v1/"`).
    /// Retries are off by default; [`Self::with_retry`] opts in.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            retry: RetryPolicy::default(),
            sleep: Arc::new(blocking_sleep),
        }
    }

    /// Set the retry policy.
    ///
    /// The policy's `timeout` bounds each attempt, not just the chain, so a
    /// single stalled request cannot eat the budget. On native it is applied to
    /// the HTTP client; on wasm the browser's fetch owns the abort, so the host
    /// enforces it instead and the field is carried for the record.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        #[cfg(not(target_family = "wasm"))]
        if let Some(timeout) = retry.timeout {
            self.client = reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default();
        }
        self.retry = retry;
        self
    }

    /// Inject the backoff timer.
    pub fn with_sleep(mut self, sleep: Arc<dyn Fn(std::time::Duration) + Send + Sync>) -> Self {
        self.sleep = sleep;
        self
    }

    /// Build the OpenAI-compatible request body.
    ///
    /// Tool turns need their own shapes: an assistant message that requested
    /// calls carries `tool_calls` (and a null content when it has no prose),
    /// and each result is a `tool` message naming the call it answers. Sent as
    /// plain role+content, the model would never see what the tools returned.
    fn build_body(&self, request: &LlmRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| match m.role {
                Role::Tool => serde_json::json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }),
                Role::Assistant if !m.tool_calls.is_empty() => {
                    let content = if m.content.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(m.content.clone())
                    };
                    let calls: Vec<serde_json::Value> = m
                        .tool_calls
                        .iter()
                        .map(|call| {
                            serde_json::json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments,
                                }
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": calls,
                    })
                }
                _ => {
                    let role = match m.role {
                        Role::System => "system",
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => unreachable!("handled above"),
                    };
                    serde_json::json!({"role": role, "content": m.content})
                }
            })
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
            // Ask for the token accounting: without it the response carries no
            // usage and a harness cannot bill or budget. Providers that do not
            // know the field ignore it.
            "stream_options": { "include_usage": true },
        });

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        body
    }

    /// Parse one SSE `data` payload into the chunks it carries.
    ///
    /// Usually one chunk. A provider may put content and `finish_reason` in the
    /// same event, and both are returned so neither is dropped: the stop reason
    /// is what tells the consumer a response was cut off.
    fn parse_chunks(data: &str) -> Result<Vec<StreamChunk>, LlmError> {
        let value: serde_json::Value = serde_json::from_str(data)?;

        // The accounting chunk carries `usage` and an *empty* choices array, so
        // it must be handled before the choices are required — otherwise a
        // provider that honours `include_usage` breaks the stream.
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            return Ok(vec![StreamChunk::Usage(Usage {
                prompt_tokens: usage
                    .get("prompt_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                completion_tokens: usage
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                total_tokens: usage
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
            })]);
        }

        let choices = value
            .get("choices")
            .and_then(|c| c.as_array())
            .ok_or_else(|| LlmError::Provider("missing choices in chunk".to_string()))?;

        let choice = choices
            .first()
            .ok_or_else(|| LlmError::Provider("empty choices array".to_string()))?;

        // The stop reason rides the chunk the provider ends on. `null` until
        // then, so it is read before the delta is required.
        let stop = choice
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .map(|reason| StreamChunk::Stop(StopReason::from_wire(reason)));

        let delta = choice
            .get("delta")
            .ok_or_else(|| LlmError::Provider("missing delta in choice".to_string()))?;

        // Check for tool calls in this delta.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
            if let Some(tc) = tool_calls.first() {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                let id = tc.get("id").and_then(|i| i.as_str()).map(String::from);
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(String::from);
                let arguments_fragment = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut chunks = vec![StreamChunk::ToolCall(ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_fragment,
                })];
                chunks.extend(stop);
                return Ok(chunks);
            }
        }

        // Otherwise, treat as text content. An empty delta with no stop reason
        // carries nothing (providers emit these between turns of a stream).
        let text = delta.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let mut chunks = Vec::new();
        if !text.is_empty() {
            chunks.push(StreamChunk::Text(text.to_string()));
        }
        chunks.extend(stop);
        Ok(chunks)
    }
}

impl LlmAdapter for OpenAiCompatibleAdapter {
    fn stream<'a>(&'a self, request: LlmRequest) -> AdapterStream<'a> {
        let url = format!("{}chat/completions", self.base_url);
        let body = self.build_body(&request);
        // Owned by the unfold closure: it outlives this call.
        let retry = self.retry.clone();
        let sleep = Arc::clone(&self.sleep);

        Box::pin(futures_util::stream::unfold(
            Stage::Request {
                client: &self.client,
                url,
                body,
                api_key: &self.api_key,
            },
            move |stage| {
                // Cloned per call: the closure owns them and is `FnMut`.
                let retry = retry.clone();
                let sleep = Arc::clone(&sleep);
                async move {
                match stage {
                    Stage::Request {
                        client,
                        url,
                        body,
                        api_key,
                    } => {
                        let mut attempt = 0u32;
                        loop {
                            let response = client
                                .post(&url)
                                .header("Authorization", format!("Bearer {api_key}"))
                                .header("Content-Type", "application/json")
                                .json(&body)
                                .send()
                                .await;

                            match response {
                                Ok(r) if r.status().is_success() => {
                                    let byte_stream: ByteStream<'a> = Box::pin(r.bytes_stream());
                                    let events = byte_stream.eventsource();
                                    return Some((
                                        Ok(StreamChunk::Text(String::new())),
                                        Stage::Streaming {
                                            events,
                                            pending: VecDeque::new(),
                                        },
                                    ));
                                }
                                Ok(r) => {
                                    // A non-2xx is a transport-level failure, not
                                    // an empty turn: without this the body is
                                    // parsed as SSE and a 404/401/429 either
                                    // breaks the stream with a confusing
                                    // "missing choices" error or yields a silent
                                    // empty turn the harness cannot distinguish
                                    // from the model saying nothing.
                                    let status = r.status();
                                    let detail = truncate_body(&r.text().await.unwrap_or_default());
                                    let error = LlmError::Http(format!("HTTP {status}: {detail}"));
                                    // A 4xx is the provider's answer; only 429/5xx
                                    // are transient, and the budget still bounds
                                    // the chain.
                                    if !retry.is_transient_status(status.as_u16())
                                        || !retry.should_retry(attempt)
                                    {
                                        return Some((Err(error), Stage::Done));
                                    }
                                    (sleep)(retry.delay_for(attempt + 1));
                                    attempt += 1;
                                }
                                Err(e) => {
                                    let error = LlmError::Http(e.to_string());
                                    if !retry.should_retry(attempt) {
                                        return Some((Err(error), Stage::Done));
                                    }
                                    (sleep)(retry.delay_for(attempt + 1));
                                    attempt += 1;
                                }
                            }
                        }
                    }
                    Stage::Streaming {
                        mut events,
                        mut pending,
                    } => {
                        // Chunks parsed from an earlier event (content plus its
                        // finish reason) come out first.
                        if let Some(chunk) = pending.pop_front() {
                            return Some((
                                Ok(chunk),
                                Stage::Streaming {
                                    events,
                                    pending,
                                },
                            ));
                        }
                        while let Some(item) = events.next().await {
                            match item {
                                Ok(event) => {
                                    if event.data == "[DONE]" {
                                        return None;
                                    }
                                    match Self::parse_chunks(&event.data) {
                                        Ok(chunks) => {
                                            // Drop empty text deltas, which carry
                                            // nothing between turns of a stream.
                                            let mut chunks: VecDeque<StreamChunk> = chunks
                                                .into_iter()
                                                .filter(|chunk| {
                                                    !matches!(
                                                        chunk,
                                                        StreamChunk::Text(text) if text.is_empty()
                                                    )
                                                })
                                                .collect();
                                            let Some(first) = chunks.pop_front() else {
                                                continue;
                                            };
                                            pending.extend(chunks);
                                            return Some((
                                                Ok(first),
                                                Stage::Streaming {
                                                    events,
                                                    pending,
                                                },
                                            ));
                                        }
                                        Err(e) => {
                                            return Some((Err(e), Stage::Done));
                                        }
                                    }
                                }
                                Err(e) => {
                                    return Some((Err(LlmError::Sse(e.to_string())), Stage::Done));
                                }
                            }
                        }
                        None
                    }
                    Stage::Done => None,
                }
                }
            }
        ))
    }
}

/// Internal state machine for the SSE unfold loop.
enum Stage<'a> {
    Request {
        client: &'a reqwest::Client,
        url: String,
        body: serde_json::Value,
        api_key: &'a str,
    },
    Streaming {
        events: eventsource_stream::EventStream<ByteStream<'a>>,
        /// Chunks parsed from an event that produced more than one, waiting
        /// for the next poll.
        pending: VecDeque<StreamChunk>,
    },
    Done,
}

/// Keep an error payload bounded: the point is the status code and a short
/// excerpt for the log, not a whole provider error page.
fn truncate_body(body: &str) -> String {
    let mut out: String = body.chars().take(200).collect();
    if body.chars().count() > 200 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Message;

    fn body_for(messages: Vec<Message>) -> serde_json::Value {
        let adapter = OpenAiCompatibleAdapter::new("https://example.invalid", "key");
        adapter.build_body(&LlmRequest {
            model: "test".into(),
            messages,
            tools: vec![],
            temperature: None,
            max_tokens: None,
        })
    }

    #[test]
    fn a_usage_chunk_with_empty_choices_is_parsed_not_rejected() {
        // Providers honouring `include_usage` send a final chunk whose choices
        // array is empty. Treating that as a protocol error would break every
        // such provider, so the accounting is read before choices are required.
        let chunk = OpenAiCompatibleAdapter::parse_chunks(
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":5,"total_tokens":16}}"#,
        )
        .expect("a usage-only chunk is valid")
        .into_iter()
        .next()
        .expect("one chunk");
        assert_eq!(
            chunk,
            StreamChunk::Usage(Usage {
                prompt_tokens: 11,
                completion_tokens: 5,
                total_tokens: 16,
            })
        );
    }

    #[test]
    fn the_finish_reason_is_parsed_into_a_stop_chunk() {
        let chunks = OpenAiCompatibleAdapter::parse_chunks(
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#,
        )
        .expect("a finishing chunk is valid");
        assert_eq!(chunks, vec![StreamChunk::Stop(StopReason::Length)]);
    }

    #[test]
    fn content_and_finish_reason_in_one_event_both_survive() {
        // A provider may put the last text delta and the finish reason in the
        // same event. Dropping either would lose the text or the signal.
        let chunks = OpenAiCompatibleAdapter::parse_chunks(
            r#"{"choices":[{"index":0,"delta":{"content":"tail"},"finish_reason":"stop"}]}"#,
        )
        .expect("the event is valid");
        assert_eq!(
            chunks,
            vec![
                StreamChunk::Text("tail".to_string()),
                StreamChunk::Stop(StopReason::Stop),
            ]
        );
    }

    #[test]
    fn the_request_asks_for_usage() {
        let body =
            OpenAiCompatibleAdapter::new("https://example.test/v1/", "k").build_body(&LlmRequest {
                model: "m".to_string(),
                messages: vec![],
                tools: vec![],
                temperature: None,
                max_tokens: None,
            });
        assert_eq!(body["stream_options"]["include_usage"], true, "{body}");
    }

    #[test]
    fn a_tool_call_is_sent_as_the_assistant_tool_calls_payload() {
        let body = body_for(vec![Message::tool_call(
            "call_1",
            "add_feature",
            r#"{"name":"x"}"#,
        )]);
        let message = &body["messages"][0];
        assert_eq!(message["role"], "assistant");
        // A tool-call message carries no prose, so content is null rather than
        // an empty string (the API distinguishes the two).
        assert!(message["content"].is_null());
        assert_eq!(message["tool_calls"][0]["id"], "call_1");
        assert_eq!(message["tool_calls"][0]["type"], "function");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "add_feature");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            r#"{"name":"x"}"#
        );
    }

    #[test]
    fn a_tool_result_is_sent_as_a_tool_message_naming_its_call() {
        let body = body_for(vec![Message::tool_result("call_1", r#"{"ok":true}"#)]);
        let message = &body["messages"][0];
        assert_eq!(message["role"], "tool");
        assert_eq!(message["tool_call_id"], "call_1");
        assert_eq!(message["content"], r#"{"ok":true}"#);
    }

    #[test]
    fn plain_messages_keep_the_role_content_shape() {
        let body = body_for(vec![Message::system("be terse"), Message::user("hello")]);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be terse");
        assert!(body["messages"][0].get("tool_calls").is_none());
        assert_eq!(body["messages"][1]["role"], "user");
    }
}
