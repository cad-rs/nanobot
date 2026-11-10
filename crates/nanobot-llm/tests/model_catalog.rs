//! The model catalog: resolving a model id to a provider and a wire model,
//! and routing a request through it.
//!
//! The guarantee that matters is backward compatibility: a harness that
//! registers one adapter under `default` and sets a model id must keep working
//! exactly as it did before the catalog existed.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt as _;
use nanobot_llm::adapter::AdapterStream;
use nanobot_llm::{
    LlmAdapter, LlmError, LlmRequest, LlmRuntime, ModelRoute, StreamChunk, DEFAULT_PROVIDER,
};

/// An adapter that records the wire model it was asked for, tagged with its
/// own label so a test can tell which provider actually served the request.
struct Recording {
    label: &'static str,
    seen: Arc<Mutex<Vec<String>>>,
}

impl LlmAdapter for Recording {
    fn stream(&self, request: LlmRequest) -> AdapterStream<'_> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.label, request.model));
        Box::pin(futures_util::stream::iter(
            Vec::<Result<StreamChunk, LlmError>>::new(),
        ))
    }
}

fn recording(label: &'static str) -> (Arc<Recording>, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::new(Recording {
            label,
            seen: Arc::clone(&seen),
        }),
        seen,
    )
}

fn drain(stream: nanobot_llm::adapter::AdapterStream<'_>) -> Vec<Result<StreamChunk, LlmError>> {
    futures_executor::block_on(stream.collect())
}

#[test]
fn a_route_selects_the_provider_and_the_wire_model() {
    let mut runtime = LlmRuntime::new();
    let (openai, openai_seen) = recording("openai");
    let (anthropic, _) = recording("anthropic");
    runtime.register("openai", openai);
    runtime.register("anthropic", anthropic);

    runtime.register_model("fast", "openai", "gpt-6-mini");

    assert_eq!(
        runtime.resolve("fast"),
        Some(ModelRoute::new("openai", "gpt-6-mini")),
        "the alias routes to its provider and its own wire model"
    );

    let _ = drain(runtime.stream_model("fast", LlmRequest {
        model: "ignored".to_string(),
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
    }));
    assert_eq!(*openai_seen.lock().unwrap(), vec!["openai:gpt-6-mini"]);
}

#[test]
fn without_a_route_the_default_adapter_carries_the_model_id() {
    let mut runtime = LlmRuntime::new();
    let (default, seen) = recording("default");
    runtime.register(DEFAULT_PROVIDER, default);

    assert_eq!(
        runtime.resolve("any-model-id"),
        Some(ModelRoute::new(DEFAULT_PROVIDER, "any-model-id")),
        "the pre-catalog behaviour: the id is the wire model, the default adapter serves it"
    );

    let _ = drain(runtime.stream_model("any-model-id", LlmRequest {
        model: "ignored".to_string(),
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
    }));
    assert_eq!(*seen.lock().unwrap(), vec!["default:any-model-id"]);
}

#[test]
fn an_unknown_model_with_no_default_adapter_is_an_error() {
    let mut runtime = LlmRuntime::new();
    let (openai, _) = recording("openai");
    runtime.register("openai", openai);

    assert_eq!(
        runtime.resolve("nope"),
        None,
        "no route and no default adapter means nowhere to route it"
    );

    let items = drain(runtime.stream_model("nope", LlmRequest {
        model: "ignored".to_string(),
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
    }));
    assert!(matches!(
        items.first(),
        Some(Err(LlmError::UnknownModel(id))) if id == "nope"
    ));
}

#[test]
fn a_route_naming_an_unregistered_provider_is_an_error() {
    let mut runtime = LlmRuntime::new();
    runtime.register_model("x", "ghost", "m");

    let items = drain(runtime.stream_model("x", LlmRequest {
        model: "ignored".to_string(),
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
    }));
    assert!(matches!(
        items.first(),
        Some(Err(LlmError::UnknownProvider(key))) if key == "ghost"
    ));
}

#[test]
fn the_catalog_lists_what_was_declared() {
    let mut runtime = LlmRuntime::new();
    runtime.register_model("fast", "openai", "gpt-6-mini");
    runtime.register_model("slow", "anthropic", "claude-sonnet-4");

    let mut ids: Vec<&String> = runtime.models().collect();
    ids.sort();
    assert_eq!(ids, vec!["fast", "slow"]);
    assert_eq!(runtime.catalog().len(), 2);
}

#[test]
fn the_low_level_stream_bypasses_the_catalog() {
    // `stream` is the direct routing a caller that does not use the agent
    // has always had: it names a provider key and does not consult the
    // catalog. Registering a route for that key must not change it.
    let mut runtime = LlmRuntime::new();
    let (scripted, seen) = recording("scripted");
    runtime.register("scripted", scripted);
    runtime.register_model("scripted", "openai", "gpt-6-mini");

    let request = LlmRequest {
        model: "as-given".to_string(),
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
    };
    let _ = drain(runtime.stream("scripted", request));
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["scripted:as-given"],
        "`stream` routes by provider key and leaves the request's model alone"
    );
}
