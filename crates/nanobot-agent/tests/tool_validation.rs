//! The pipeline's validation and coercion reaching the executor: a near-miss
//! from the model is fixed before the tool runs, and a real violation is
//! refused at the gate.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream;
use nanobot_agent::{Agent, ToolExecutor, WriteOutcome};
use nanobot_llm::adapter::AdapterStream;
use nanobot_llm::{
    LlmAdapter, LlmRequest, LlmRuntime, StreamChunk, ToolCallDelta, DEFAULT_PROVIDER,
};
use nanobot_session::Session;
use nanobot_tools::{AutoApprovalPolicy, ToolContract, ToolDefinition, ToolPipeline, ToolRegistry};

/// Captures the arguments the executor was actually handed.
#[derive(Default)]
struct Capturing {
    seen: std::sync::Mutex<Vec<serde_json::Value>>,
    calls: AtomicUsize,
}

impl ToolExecutor for Capturing {
    fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> String {
        "{}".to_string()
    }

    fn apply(&self, name: &str, arguments: &serde_json::Value) -> WriteOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(arguments.clone());
        WriteOutcome::ungated(self.execute(name, arguments))
    }
}

/// Emits one tool call (with `arguments` as its JSON arguments) on the first
/// request, then text — so the turn ends instead of looping.
struct OneCall {
    calls: AtomicUsize,
    name: String,
    arguments: String,
}

impl LlmAdapter for OneCall {
    fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCall(ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some(self.name.clone()),
                arguments_fragment: self.arguments.clone(),
            }))]))
        } else {
            Box::pin(stream::iter(vec![Ok(StreamChunk::Text("done".into()))]))
        }
    }
}

fn harness(
    schema: serde_json::Value,
    arguments: &str,
    validate_args: bool,
) -> (Agent, Arc<Capturing>) {
    let session = Session::new("sess-tool-validation");
    let mut runtime = LlmRuntime::new();
    runtime.register(
        DEFAULT_PROVIDER,
        Arc::new(OneCall {
            calls: AtomicUsize::new(0),
            name: "measure".to_string(),
            arguments: arguments.to_string(),
        }),
    );

    let registry = Arc::new(ToolRegistry::new());
    let _ = registry.register(ToolDefinition {
        name: "measure".to_string(),
        description: "measures something".to_string(),
        parameters_schema: schema,
        requires_approval: false,
        contract: ToolContract::ReadOnly,
        validate_args,
    });

    let executor = Arc::new(Capturing::default());
    let agent = Agent::new(session, runtime, ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy)));
    agent.set_tool_executor(executor.clone());
    (agent, executor)
}

fn run(agent: &Agent) {
    agent.followup("go".to_string());
    futures_executor::block_on(agent.run());
}

#[test]
fn the_executor_receives_the_coerced_arguments() {
    // Coercion is always on: the tool does not need to opt into validation for
    // the model's numeric string to arrive as a number.
    let (agent, executor) = harness(
        serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"]
        }),
        r#"{"count": "17"}"#,
        false,
    );
    run(&agent);

    assert_eq!(executor.calls.load(Ordering::SeqCst), 1, "the tool ran");
    assert_eq!(
        *executor.seen.lock().unwrap(),
        vec![serde_json::json!({"count": 17})],
        "the executor got the coerced integer, not the model's string"
    );
}

#[test]
fn a_schema_violation_never_reaches_the_executor() {
    let (agent, executor) = harness(
        serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"]
        }),
        r#"{"count": true}"#,
        true,
    );
    run(&agent);

    assert_eq!(
        executor.calls.load(Ordering::SeqCst),
        0,
        "a wrong type is refused at the gate, before any executor runs"
    );
    assert!(executor.seen.lock().unwrap().is_empty());
}

#[test]
fn a_tool_that_does_not_opt_in_coerces_but_never_refuses() {
    // The opt-out is the safe default: a schema that under-describes the
    // accepted input must not refuse a call the product would accept.
    let (agent, executor) = harness(
        serde_json::json!({
            "type": "object",
            "required": ["instance"],
            "properties": {"instance": {"type": "string"}}
        }),
        r#"{"instance": "q0", "unexpected": true}"#,
        false,
    );
    run(&agent);

    assert_eq!(
        executor.calls.load(Ordering::SeqCst),
        1,
        "without the opt-in the pipeline coerces and lets the product decide"
    );
}

#[test]
fn a_missing_required_property_is_refused_and_recorded() {
    let (agent, executor) = harness(
        serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"]
        }),
        r#"{"other": 1}"#,
        true,
    );
    run(&agent);

    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    // The refusal is on the session log as a failed tool result.
    let results: Vec<_> = agent
        .session()
        .events()
        .into_iter()
        .filter_map(|entry| match entry.event {
            nanobot_session::SessionEvent::ToolResult { success, output, .. } => {
                Some((success, output))
            }
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert!(!results[0].0, "the refused call is logged as a failure");
    assert!(
        results[0].1.contains("invalid_input"),
        "the structured refusal names the code, got {}",
        results[0].1
    );
}
