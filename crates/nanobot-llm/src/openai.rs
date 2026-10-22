//! OpenAI-compatible adapter: streams chat completions over SSE using
//! `reqwest` + `eventsource-stream` (design-harness 10.5).
//!
//! The same code path serves native (hyper) and wasm (browser fetch via
//! `wasm-streams`) — no platform-conditional compilation.

use crate::adapter::{AdapterStream, LlmAdapter, LlmRequest, Role, Usage};
use crate::chunk::{StreamChunk, ToolCallDelta};
use crate::error::LlmError;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;

/// The byte stream returned by `reqwest::Response::bytes_stream()`.
///
/// On native this is `BoxStream` (Send). On wasm the underlying stream
/// is not Send, so the bound is dropped.
#[cfg(not(target_family = "wasm"))]
type ByteStream<'a> = futures_core::stream::BoxStream<'a, Result<bytes::Bytes, reqwest::Error>>;

#[cfg(target_family = "wasm")]
type ByteStream<'a> =
    std::pin::Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + 'a>>;

/// OpenAI-compatible chat completions adapter.
///
/// Works with any API that follows the OpenAI chat completions format:
/// OpenAI, DeepSeek, Ollama (OpenAI-compatible mode), vLLM, etc.
pub struct OpenAiCompatibleAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAiCompatibleAdapter {
    /// Create a new adapter targeting `{base_url}` with the given API key.
    ///
    /// `base_url` should end with a `/` (e.g. `"https://api.openai.com/v1/"`).
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
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

    /// Parse one SSE `data` payload into a `StreamChunk`.
    fn parse_chunk(data: &str) -> Result<StreamChunk, LlmError> {
        let value: serde_json::Value = serde_json::from_str(data)?;

        // The accounting chunk carries `usage` and an *empty* choices array, so
        // it must be handled before the choices are required — otherwise a
        // provider that honours `include_usage` breaks the stream.
        if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
            return Ok(StreamChunk::Usage(Usage {
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
            }));
        }

        let choices = value
            .get("choices")
            .and_then(|c| c.as_array())
            .ok_or_else(|| LlmError::Provider("missing choices in chunk".to_string()))?;

        let choice = choices
            .first()
            .ok_or_else(|| LlmError::Provider("empty choices array".to_string()))?;

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
                return Ok(StreamChunk::ToolCall(ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_fragment,
                }));
            }
        }

        // Otherwise, treat as text content.
        let text = delta.get("content").and_then(|c| c.as_str()).unwrap_or("");
        Ok(StreamChunk::Text(text.to_string()))
    }
}

impl LlmAdapter for OpenAiCompatibleAdapter {
    fn stream<'a>(&'a self, request: LlmRequest) -> AdapterStream<'a> {
        let url = format!("{}chat/completions", self.base_url);
        let body = self.build_body(&request);

        Box::pin(futures_util::stream::unfold(
            Stage::Request {
                client: &self.client,
                url,
                body,
                api_key: &self.api_key,
            },
            |stage| async move {
                match stage {
                    Stage::Request {
                        client,
                        url,
                        body,
                        api_key,
                    } => {
                        let resp = client
                            .post(&url)
                            .header("Authorization", format!("Bearer {api_key}"))
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                            .await;
                        match resp {
                            Ok(r) => {
                                // A non-2xx response is a transport-level
                                // failure, not an empty turn: without this the
                                // body is parsed as SSE and a 404/401/429 either
                                // breaks the stream with a confusing
                                // "missing choices" error or yields a silent
                                // empty turn the harness cannot distinguish from
                                // the model saying nothing.
                                let status = r.status();
                                if !status.is_success() {
                                    let body = r.text().await.unwrap_or_default();
                                    return Some((
                                        Err(LlmError::Http(format!(
                                            "HTTP {status}: {}",
                                            truncate_body(&body)
                                        ))),
                                        Stage::Done,
                                    ));
                                }
                                let byte_stream: ByteStream<'a> = Box::pin(r.bytes_stream());
                                let es = byte_stream.eventsource();
                                Some((Ok(StreamChunk::Text(String::new())), Stage::Streaming(es)))
                            }
                            Err(e) => Some((Err(LlmError::Http(e.to_string())), Stage::Done)),
                        }
                    }
                    Stage::Streaming(mut es) => {
                        while let Some(item) = es.next().await {
                            match item {
                                Ok(event) => {
                                    if event.data == "[DONE]" {
                                        return None;
                                    }
                                    match Self::parse_chunk(&event.data) {
                                        Ok(chunk) => {
                                            if let StreamChunk::Text(ref t) = chunk {
                                                if t.is_empty() {
                                                    continue;
                                                }
                                            }
                                            return Some((Ok(chunk), Stage::Streaming(es)));
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
            },
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
    Streaming(eventsource_stream::EventStream<ByteStream<'a>>),
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
        let chunk = OpenAiCompatibleAdapter::parse_chunk(
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":5,"total_tokens":16}}"#,
        )
        .expect("a usage-only chunk is valid");
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
