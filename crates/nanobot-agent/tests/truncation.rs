//! Truncation safety: a response the provider cut off at the token limit is an
//! incomplete message, so the tool calls it names are failed rather than run.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream;
use nanobot_agent::{Agent, ToolExecutor, WriteOutcome};
use nanobot_llm::adapter::AdapterStream;
use nanobot_llm::{LlmAdapter, LlmRequest, LlmRuntime, StopReason, StreamChunk, ToolCallDelta};
use nanobot_session::Session;
use nanobot_tools::{AutoApprovalPolicy, ToolContract, ToolDefinition, ToolPipeline, ToolRegistry};

/// Captures the arguments the executor was handed, and how many times it ran.
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

/// Emits one tool call, tagged with `stop`, on the first request; text after.
struct OneCall {
    calls: AtomicUsize,
    stop: StopReason,
}

impl LlmAdapter for OneCall {
    fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Box::pin(stream::iter(vec![
                Ok(StreamChunk::ToolCall(ToolCallDelta {
                    index: 0,
                    id: Some("call-1".into()),
                    name: Some("measure".into()),
                    arguments_fragment: "{}".into(),
                })),
                Ok(StreamChunk::Stop(self.stop)),
            ]))
        } else {
            Box::pin(stream::iter(vec![Ok(StreamChunk::Text("done".into()))]))
        }
    }
}

fn harness(stop: StopReason) -> (Agent, Arc<Capturing>) {
    let session = Session::new("sess-truncation");
    let mut runtime = LlmRuntime::new();
    runtime.register(
        nanobot_llm::DEFAULT_PROVIDER,
        Arc::new(OneCall {
            calls: AtomicUsize::new(0),
            stop,
        }),
    );

    let registry = Arc::new(ToolRegistry::new());
    let _ = registry.register(ToolDefinition {
        name: "measure".to_string(),
        description: "measures something".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
        requires_approval: false,
        contract: ToolContract::ReadOnly,
        validate_args: false,
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

fn tool_results(agent: &Agent) -> Vec<(bool, String)> {
    agent
        .session()
        .events()
        .into_iter()
        .filter_map(|entry| match entry.event {
            nanobot_session::SessionEvent::ToolResult { success, output, .. } => {
                Some((success, output))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_truncated_response_refuses_its_tool_calls() {
    let (agent, executor) = harness(StopReason::Length);
    run(&agent);

    assert_eq!(
        executor.calls.load(Ordering::SeqCst),
        0,
        "a call from a cut-off message must not run"
    );

    let results = tool_results(&agent);
    assert_eq!(results.len(), 1, "the refusal is still logged as a call");
    assert!(!results[0].0, "it is logged as a failure");
    assert!(
        results[0].1.contains("truncated"),
        "the reason is named, got {}",
        results[0].1
    );
}

#[test]
fn the_truncation_is_visible_in_the_check_panel() {
    let (agent, _executor) = harness(StopReason::Length);
    run(&agent);

    let checks: Vec<_> = agent
        .session()
        .events()
        .into_iter()
        .filter_map(|entry| match entry.event {
            nanobot_session::SessionEvent::CheckResult { check_id, passed, .. } => {
                Some((check_id, passed))
            }
            _ => None,
        })
        .collect();
    assert!(
        checks.iter().any(|(id, passed)| id == "llm:truncated" && !*passed),
        "the truncation lands in the check vocabulary, got {checks:?}"
    );
}

#[test]
fn a_complete_response_runs_its_tool_calls() {
    // The control: only `Length` is truncated. A `tool_calls` stop means the
    // model finished its message and the calls are safe to run.
    let (agent, executor) = harness(StopReason::ToolUse);
    run(&agent);

    assert_eq!(executor.calls.load(Ordering::SeqCst), 1, "the call ran");
    assert_eq!(*executor.seen.lock().unwrap(), vec![serde_json::json!({})]);
}

#[test]
fn an_unknown_stop_reason_runs_the_calls() {
    // A provider that does not report a finish reason leaves `stop_reason`
    // empty, which is not truncation: the call runs.
    let (agent, executor) = harness(StopReason::Other);
    run(&agent);
    assert_eq!(
        executor.calls.load(Ordering::SeqCst),
        1,
        "an unnamed stop reason is not a truncation"
    );
}
