//! Fault injection for the agent loop: the turn/step driver's failure
//! paths, driven headlessly through the real loop (no network, no window).
//!
//! The guarantees these pin down:
//! - a cancelled turn ends with the `Cancelled` marker and stops;
//! - the step budget cuts a looping model off (`MaxSteps`);
//! - a denied tool is refused, not executed;
//! - a missing executor is recorded, not fatal;
//! - a rejected write rolls back and tells the model why;
//! - a provider failure is a recorded failure, never a silent empty turn.
//!
//! The model is a scripted [`LlmAdapter`] (not an HTTP server): the loop is
//! the thing under test, so the seam is the adapter trait.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::stream;
use futures_util::StreamExt as _;
use nanobot_agent::{Agent, ToolExecutor, WriteCheck, WriteOutcome};
use nanobot_llm::adapter::AdapterStream;
use nanobot_llm::{LlmAdapter, LlmError, LlmRequest, LlmRuntime, StreamChunk, ToolCallDelta};
use nanobot_session::{Session, SessionEvent};
use nanobot_tools::{DenyAllPolicy, ToolDefinition, ToolPipeline, ToolRegistry};

/// What the scripted model does per request.
enum Behavior {
    /// One text chunk, then the stream ends.
    Text,
    /// One tool call for `name`, then the stream ends.
    ToolCall(String),
    /// A tool call for `name` on every request (drives the loop forever).
    AlwaysToolCall(String),
    /// First request: a tool call. Later requests: the stream stays open
    /// until the test releases the gate, then yields one chunk — so a
    /// cancellation is observed mid-stream, not at a step boundary.
    Gated { name: String, gate: std::sync::Mutex<Option<futures_channel::mpsc::Receiver<()>>> },
    /// The stream fails.
    Fail(String),
}

struct Scripted {
    calls: AtomicUsize,
    behavior: Behavior,
}

impl Scripted {
    fn new(behavior: Behavior) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            behavior,
        })
    }
}

impl LlmAdapter for Scripted {
    fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.behavior {
            Behavior::Text => {
                Box::pin(stream::iter(vec![Ok(StreamChunk::Text("hello".into()))]))
            }
            // One-shot: only the first request carries the call, so the turn
            // can end once it is handled.
            Behavior::ToolCall(name) if call == 0 => {
                Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCall(tool_call_delta(name)))]))
            }
            Behavior::AlwaysToolCall(name) => {
                Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCall(tool_call_delta(name)))]))
            }
            Behavior::ToolCall(_) => Box::pin(stream::iter(Vec::new())),
            Behavior::Gated { name, gate } => {
                if call == 0 {
                    return Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCall(
                        tool_call_delta(name),
                    ))]));
                }
                // Hold the stream open: the loop is parked in `next().await`
                // until the test releases the gate.
                let rx = gate.lock().unwrap().take();
                Box::pin(stream::once(async move {
                    if let Some(mut rx) = rx {
                        let _ = rx.next().await;
                    }
                    Ok::<_, LlmError>(StreamChunk::Text(String::new()))
                }))
            }
            Behavior::Fail(detail) => {
                Box::pin(stream::iter(vec![Err(LlmError::Sse(detail.clone()))]))
            }
        }
    }
}

fn tool_call_delta(name: &str) -> ToolCallDelta {
    ToolCallDelta {
        index: 0,
        id: Some("call-1".into()),
        name: Some(name.to_string()),
        arguments_fragment: "{}".into(),
    }
}

/// An executor that counts invocations, for the refusal assertions.
#[derive(Default)]
struct CountingExecutor {
    calls: AtomicUsize,
    reject: bool,
}

impl ToolExecutor for CountingExecutor {
    fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> String {
        self.calls.fetch_add(1, Ordering::SeqCst);
        "{}".to_string()
    }

    fn apply(&self, _name: &str, _arguments: &serde_json::Value) -> WriteOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.reject {
            WriteOutcome {
                reply: "{}".to_string(),
                accepted: false,
                checks: vec![WriteCheck {
                    invariant: "adds_one_feature".to_string(),
                    passed: false,
                    detail: "the feature was not added".to_string(),
                }],
            }
        } else {
            WriteOutcome::ungated("{}".to_string())
        }
    }
}

/// Assembles the real loop: session + scripted model + registry with `noop`
/// + pipeline. The executor and approval policy are the knobs.
struct Harness {
    agent: Agent,
    model: Arc<Scripted>,
    executor: Arc<CountingExecutor>,
}

fn harness(
    behavior: Behavior,
    policy: Arc<dyn nanobot_tools::ApprovalPolicy>,
    executor: Option<Arc<CountingExecutor>>,
) -> Harness {
    harness_with_approval(behavior, policy, executor, false)
}

/// `requires_approval` is the tool's own flag: the approval policy is only
/// consulted for tools that declare it, so the denial path needs it set.
fn harness_with_approval(
    behavior: Behavior,
    policy: Arc<dyn nanobot_tools::ApprovalPolicy>,
    executor: Option<Arc<CountingExecutor>>,
    requires_approval: bool,
) -> Harness {
    let session = Session::new("sess-faults");
    let mut llm = LlmRuntime::new();
    let model = Scripted::new(behavior);
    llm.register("default", model.clone());

    let registry = Arc::new(ToolRegistry::new());
    let _ = registry.register(ToolDefinition {
        name: "noop".to_string(),
        description: "does nothing".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
        requires_approval,
        contract: nanobot_tools::ToolContract::ReadOnly,
        validate_args: false,
    });

    let executor = executor.unwrap_or_else(|| Arc::new(CountingExecutor::default()));
    let agent = Agent::new(session, llm, ToolPipeline::new(registry, policy));
    if executor.calls.load(Ordering::SeqCst) == 0 {
        agent.set_tool_executor(executor.clone());
    }
    Harness { agent, model, executor }
}

fn run_turn(agent: &Agent) {
    agent.followup("go".to_string());
    futures_executor::block_on(agent.run());
}

fn events(agent: &Agent) -> Vec<SessionEvent> {
    agent.session().events().into_iter().map(|entry| entry.event).collect()
}

fn count(events: &[SessionEvent], predicate: fn(&SessionEvent) -> bool) -> usize {
    events.iter().filter(|e| predicate(e)).count()
}

#[test]
fn cancelling_before_a_turn_leaves_no_trace() {
    let h = harness(Behavior::Text, Arc::new(nanobot_tools::AutoApprovalPolicy), None);
    h.agent.cancel();
    run_turn(&h.agent);
    assert!(
        count(&events(&h.agent), |e| matches!(e, SessionEvent::TurnStart { .. })) == 0,
        "a cancelled agent must not start a turn"
    );
}

#[test]
fn a_cancelled_turn_ends_with_the_cancelled_marker_and_stops() {
    let (mut tx, rx) = futures_channel::mpsc::channel::<()>(1);
    let h = harness(
        Behavior::Gated {
            name: "noop".to_string(),
            gate: std::sync::Mutex::new(Some(rx)),
        },
        Arc::new(nanobot_tools::AutoApprovalPolicy),
        None,
    );

    // Drive the loop on its own thread: the second step parks in the gated
    // stream, so the main thread can cancel mid-turn deterministically.
    let agent = h.agent.clone();
    let driver = std::thread::spawn(move || {
        agent.followup("go".to_string());
        futures_executor::block_on(agent.run());
    });

    // Wait for the second step to begin (the first step's tool call ran).
    let session = h.agent.session().clone();
    for _ in 0..10_000 {
        let started = session
            .events()
            .iter()
            .any(|e| matches!(&e.event, SessionEvent::StepStart { step: 1, .. }));
        if started {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    h.agent.cancel();
    let _ = tx.try_send(());
    driver.join().expect("the loop returns once cancelled");

    let events = events(&h.agent);
    let cancelled = count(&events, |e| {
        matches!(e, SessionEvent::TurnEnd { reason: nanobot_session::TurnEndReason::Cancelled, .. })
    });
    assert_eq!(cancelled, 1, "the turn ends with the Cancelled marker");
    assert!(
        count(&events, |e| matches!(e, SessionEvent::TurnStart { .. })) == 1,
        "and no second turn starts"
    );
    assert_eq!(h.model.calls.load(Ordering::SeqCst), 2, "the loop stopped at the held stream");
}

#[test]
fn the_step_budget_ends_a_looping_turn_with_max_steps() {
    let h = harness(
        Behavior::AlwaysToolCall("noop".to_string()),
        Arc::new(nanobot_tools::AutoApprovalPolicy),
        None,
    );
    run_turn(&h.agent);

    let events = events(&h.agent);
    let reason = events.iter().find_map(|e| match e {
        SessionEvent::TurnEnd { reason, .. } => Some(reason),
        _ => None,
    });
    assert!(
        matches!(reason, Some(nanobot_session::TurnEndReason::MaxSteps)),
        "a model that always calls a tool is cut off at the step budget, got {reason:?}"
    );
    let steps = count(&events, |e| matches!(e, SessionEvent::StepStart { .. }));
    assert_eq!(steps, 50, "the budget is the documented MAX_STEPS");
    assert_eq!(h.executor.calls.load(Ordering::SeqCst), 50, "one execution per step");
}

#[test]
fn a_denied_tool_is_refused_and_never_executed() {
    let h = harness_with_approval(
        Behavior::ToolCall("noop".to_string()),
        Arc::new(DenyAllPolicy),
        None,
        true,
    );
    run_turn(&h.agent);

    let events = events(&h.agent);
    let denied = events.iter().find_map(|e| match e {
        SessionEvent::ToolResult { success, output, .. } if !success => Some(output.clone()),
        _ => None,
    });
    assert!(
        denied.as_deref().is_some_and(|o| o.contains("tool_denied")),
        "the refusal is recorded on the log, got {denied:?}"
    );
    assert_eq!(
        h.executor.calls.load(Ordering::SeqCst),
        0,
        "a denied call never reaches the executor"
    );
}

#[test]
fn a_missing_executor_is_recorded_not_fatal() {
    // No executor wired: the call is announced, then refused.
    let session = Session::new("sess-faults-noexec");
    let mut llm = LlmRuntime::new();
    llm.register("default", Scripted::new(Behavior::ToolCall("noop".to_string())));
    let registry = Arc::new(ToolRegistry::new());
    let _ = registry.register(ToolDefinition {
        name: "noop".to_string(),
        description: "does nothing".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
        requires_approval: false,
        contract: nanobot_tools::ToolContract::ReadOnly,
        validate_args: false,
    });
    let agent = Agent::new(
        session,
        llm,
        ToolPipeline::new(registry, Arc::new(nanobot_tools::AutoApprovalPolicy)),
    );

    run_turn(&agent);
    let events = events(&agent);
    let refused = events.iter().find_map(|e| match e {
        SessionEvent::ToolResult { success, output, .. } if !success => Some(output.clone()),
        _ => None,
    });
    assert!(
        refused.as_deref().is_some_and(|o| o.contains("no_executor")),
        "the missing backend is recorded, got {refused:?}"
    );
    assert!(
        matches!(
            events.iter().find_map(|e| match e {
                SessionEvent::TurnEnd { reason, .. } => Some(reason),
                _ => None,
            }),
            Some(nanobot_session::TurnEndReason::Completed)
        ),
        "and the turn still completes"
    );
}

#[test]
fn a_rejected_write_rolls_back_and_tells_the_model_why() {
    let h = harness(
        Behavior::ToolCall("noop".to_string()),
        Arc::new(nanobot_tools::AutoApprovalPolicy),
        Some(Arc::new(CountingExecutor {
            calls: AtomicUsize::new(0),
            reject: true,
        })),
    );
    run_turn(&h.agent);

    let events = events(&h.agent);
    let result = events.iter().find_map(|e| match e {
        SessionEvent::ToolResult { success, output, .. } if !success => Some(output.clone()),
        _ => None,
    });
    assert!(
        result.as_deref().is_some_and(|o| o.contains("write_rejected")),
        "the model is told the write was rejected, got {result:?}"
    );
    assert!(
        result.as_deref().is_some_and(|o| o.contains("adds_one_feature")),
        "and why: the failing invariant is in the reply, got {result:?}"
    );
    let check = events.iter().find_map(|e| match e {
        SessionEvent::CheckResult { check_id, passed, .. } => Some((check_id.clone(), *passed)),
        _ => None,
    });
    assert_eq!(
        check,
        Some(("write:noop".to_string(), false)),
        "the verdict lands in the check panel"
    );
}

#[test]
fn a_provider_failure_is_recorded_not_swallowed() {
    let h = harness(
        Behavior::Fail("connection reset".to_string()),
        Arc::new(nanobot_tools::AutoApprovalPolicy),
        None,
    );
    run_turn(&h.agent);

    let events = events(&h.agent);
    let check = events.iter().find_map(|e| match e {
        SessionEvent::CheckResult { check_id, passed, .. } if check_id == "llm:stream" => {
            Some(*passed)
        }
        _ => None,
    });
    assert_eq!(check, Some(false), "the stream failure is a recorded check");
    assert!(
        !events.iter().any(|e| matches!(e, SessionEvent::AssistantMessage { .. })),
        "and nothing is presented as an answer"
    );
    assert!(
        matches!(
            events.iter().find_map(|e| match e {
                SessionEvent::TurnEnd { reason, .. } => Some(reason),
                _ => None,
            }),
            Some(nanobot_session::TurnEndReason::Error)
        ),
        "the turn ends with Error, not Completed"
    );
}
