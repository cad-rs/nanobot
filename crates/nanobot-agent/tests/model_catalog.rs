//! The agent's model routing: `set_model` names a model id, the runtime's
//! catalog resolves it to a provider and a wire model, and `RequestHeader`
//! records the wire model.

use std::sync::{Arc, Mutex};

use nanobot_agent::Agent;
use nanobot_llm::adapter::AdapterStream;
use nanobot_llm::{LlmAdapter, LlmRequest, LlmRuntime, StreamChunk};
use nanobot_session::{Session, SessionEvent};
use nanobot_tools::{AutoApprovalPolicy, ToolPipeline, ToolRegistry};

/// An adapter that records the wire model it was asked for.
struct Recording {
    seen: Arc<Mutex<Vec<String>>>,
}

impl LlmAdapter for Recording {
    fn stream(&self, request: LlmRequest) -> AdapterStream<'_> {
        self.seen.lock().unwrap().push(request.model.clone());
        // A text chunk so the turn produces a `RequestHeader` (the wire model
        // is only recorded once the assembler has a block to close).
        Box::pin(futures_util::stream::iter(vec![Ok(StreamChunk::Text(
            "ok".into(),
        ))]))
    }
}

fn harness(adapter_key: &str) -> (Agent, Arc<Mutex<Vec<String>>>) {
    let session = Session::new("sess-model-catalog");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = LlmRuntime::new();
    runtime.register(adapter_key, Arc::new(Recording { seen: Arc::clone(&seen) }));

    let registry = Arc::new(ToolRegistry::new());
    let agent = Agent::new(session, runtime, ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy)));
    (agent, seen)
}

fn run(agent: &Agent) {
    agent.followup("go".to_string());
    futures_executor::block_on(agent.run());
}

/// The model recorded on `RequestHeader` for the turn.
fn wire_model_recorded(agent: &Agent) -> Option<String> {
    agent
        .session()
        .events()
        .into_iter()
        .find_map(|entry| match entry.event {
            SessionEvent::RequestHeader { model, .. } => Some(model),
            _ => None,
        })
}

#[test]
fn a_catalog_route_sends_the_wire_model_to_the_routed_provider() {
    // The route must be declared before the agent takes ownership of the
    // runtime (the builder registers routes in production; here the runtime is
    // assembled directly).
    let session = Session::new("sess-model-catalog-route");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = LlmRuntime::new();
    runtime.register("openai", Arc::new(Recording { seen: Arc::clone(&seen) }));
    runtime.register_model("fast", "openai", "gpt-6-mini");
    let registry = Arc::new(ToolRegistry::new());
    let agent = Agent::new(session, runtime, ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy)));

    agent.set_model("fast");
    run(&agent);

    assert_eq!(
        wire_model_recorded(&agent),
        Some("gpt-6-mini".to_string()),
        "`RequestHeader` records the wire model the provider was asked for, not the alias"
    );
    assert_eq!(*seen.lock().unwrap(), vec!["gpt-6-mini"]);
}

#[test]
fn without_a_route_the_default_adapter_carries_the_model_id() {
    let (agent, seen) = harness(nanobot_llm::DEFAULT_PROVIDER);
    agent.set_model("legacy-model");
    run(&agent);

    assert_eq!(wire_model_recorded(&agent), Some("legacy-model".to_string()));
    assert_eq!(*seen.lock().unwrap(), vec!["legacy-model"]);
}

#[test]
fn an_unknown_model_id_fails_the_stream_and_records_the_turn() {
    // No `default` adapter and no route: the model id has nowhere to go.
    let (agent, seen) = harness("openai");
    agent.set_model("nope");
    run(&agent);

    assert!(
        seen.lock().unwrap().is_empty(),
        "an unknown model must not reach any provider"
    );
    // The failure is recorded as a `llm:stream` check failure, not swallowed.
    let checks: Vec<_> = agent
        .session()
        .events()
        .into_iter()
        .filter_map(|entry| match entry.event {
            SessionEvent::CheckResult { check_id, passed, .. } => Some((check_id, passed)),
            _ => None,
        })
        .collect();
    assert!(
        checks.iter().any(|(id, passed)| id == "llm:stream" && !*passed),
        "an unroutable model is a recorded stream failure, got {checks:?}"
    );
}
