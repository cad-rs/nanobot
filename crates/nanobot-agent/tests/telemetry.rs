//! The agent loop's telemetry spans: `agent.turn`, `agent.step`,
//! `tool.execute`, plus the runtime's `provider.request`.
//!
//! The recorder is installed on both the agent and the LLM runtime, so the
//! whole request path is covered. The default context records nothing — these
//! spans exist only because this test opted in, which is the point: telemetry
//! is a choice, not a dependency.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream;
use nanobot_agent::{Agent, ToolExecutor, WriteOutcome};
use nanobot_llm::adapter::AdapterStream;
use nanobot_llm::{LlmAdapter, LlmRequest, LlmRuntime, StreamChunk, ToolCallDelta};
use nanobot_session::Session;
use nanobot_telemetry::testing::{Phase, RecordingTelemetryContext};
use nanobot_tools::{AutoApprovalPolicy, ToolDefinition, ToolPipeline, ToolRegistry};

/// A scripted model: a tool call on the first request when `tool_first`, then
/// text — so the turn ends instead of looping.
struct Scripted {
    calls: AtomicUsize,
    tool_first: bool,
}

impl LlmAdapter for Scripted {
    fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.tool_first && call == 0 {
            return Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCall(ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name: Some("noop".into()),
                arguments_fragment: "{}".into(),
            }))]));
        }
        Box::pin(stream::iter(vec![Ok(StreamChunk::Text("hello".into()))]))
    }
}

#[derive(Default)]
struct NoopExecutor;

impl ToolExecutor for NoopExecutor {
    fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> String {
        "{}".to_string()
    }

    fn apply(&self, name: &str, arguments: &serde_json::Value) -> WriteOutcome {
        WriteOutcome::ungated(self.execute(name, arguments))
    }
}

fn harness(tool_first: bool) -> (Agent, Arc<RecordingTelemetryContext>) {
    let telemetry = Arc::new(RecordingTelemetryContext::new());
    let session = Session::new("sess-telemetry");

    let mut llm = LlmRuntime::new().with_telemetry(telemetry.clone());
    llm.register(
        "default",
        Arc::new(Scripted {
            calls: AtomicUsize::new(0),
            tool_first,
        }),
    );

    let registry = Arc::new(ToolRegistry::new());
    let _ = registry.register(ToolDefinition {
        name: "noop".to_string(),
        description: "does nothing".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
        requires_approval: false,
        contract: nanobot_tools::ToolContract::ReadOnly,
        validate_args: false,
    });

    let agent = Agent::new(session, llm, ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy)));
    agent.set_tool_executor(Arc::new(NoopExecutor));
    agent.set_telemetry(telemetry.clone());
    (agent, telemetry)
}

fn run(agent: &Agent) {
    agent.followup("go".to_string());
    futures_executor::block_on(agent.run());
}

/// The span names recorded at a phase, in order.
fn names(rec: &RecordingTelemetryContext, phase: Phase) -> Vec<String> {
    rec.records()
        .into_iter()
        .filter(|r| r.phase == phase)
        .map(|r| r.name)
        .collect()
}

/// Asserts that every started span also ended, and that the two sets match —
/// order is not asserted (the drop order of the step's locals is an
/// implementation detail).
fn assert_all_spans_closed(rec: &RecordingTelemetryContext) {
    let mut started = names(rec, Phase::Start);
    let mut ended = names(rec, Phase::End);
    started.sort();
    ended.sort();
    assert_eq!(
        started, ended,
        "every started span must end (a leaked span means a guard escaped its scope)"
    );
}

#[test]
fn a_text_turn_emits_turn_step_and_provider_spans() {
    let (agent, rec) = harness(false);
    run(&agent);

    let started = names(&rec, Phase::Start);
    for expected in ["agent.turn", "agent.step", "provider.request"] {
        assert!(
            started.iter().any(|n| n.as_str() == expected),
            "expected a `{expected}` span, got {started:?}"
        );
    }
    assert_all_spans_closed(&rec);
}

#[test]
fn a_tool_call_adds_a_tool_span() {
    let (agent, rec) = harness(true);
    run(&agent);

    let started = names(&rec, Phase::Start);
    assert!(
        started.iter().any(|n| n.as_str() == "tool.execute"),
        "a dispatched tool must emit a `tool.execute` span, got {started:?}"
    );
    // One turn, two steps (the tool call creates tool debt), one tool call.
    assert_eq!(started.iter().filter(|n| n.as_str() == "agent.step").count(), 2);
    assert_eq!(started.iter().filter(|n| n.as_str() == "tool.execute").count(), 1);
    assert_all_spans_closed(&rec);
}

#[test]
fn the_default_context_emits_nothing() {
    // The same harness shape with no recorder installed: the loop still runs
    // and the no-op context drops every span on the floor. This is what keeps
    // instrumentation from being a dependency.
    let agent = {
        let session = Session::new("sess-noop");
        let mut llm = LlmRuntime::new();
        llm.register(
            "default",
            Arc::new(Scripted {
                calls: AtomicUsize::new(0),
                tool_first: false,
            }),
        );
        let registry = Arc::new(ToolRegistry::new());
        let _ = registry.register(ToolDefinition {
            name: "noop".to_string(),
            description: "does nothing".to_string(),
            parameters_schema: serde_json::json!({"type": "object"}),
            requires_approval: false,
            contract: nanobot_tools::ToolContract::ReadOnly,
            validate_args: false,
        });
        let agent = Agent::new(session, llm, ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy)));
        agent.set_tool_executor(Arc::new(NoopExecutor));
        // No set_telemetry: the default no-op context is in place.
        agent
    };
    run(&agent);
    // Nothing to assert beyond "the turn completed": the guarantee is the
    // absence of any recording, and a leaked guard would be invisible anyway.
    // The recorder tests in `nanobot-telemetry` pin the no-op contract.
}
