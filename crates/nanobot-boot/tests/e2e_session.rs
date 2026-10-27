//! End-to-end session tests: the whole public surface in one flow.
//!
//! The unit tests cover each seam and the fault tests cover single-turn
//! failures, but nothing drove a session through assembly, several turns, tool
//! execution, the log, persistence, recovery and compaction as one path. These
//! do, using only what a product uses — `Harness::builder`, `run_one_turn`,
//! and the session log on disk.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_executor::block_on;
use futures_util::stream;
use nanobot_boot::{CLEAN_MARKER, Harness, ProjectPaths, run_one_turn};
use nanobot_llm::adapter::AdapterStream;
use nanobot_agent::{ToolExecutor, WriteOutcome};
use nanobot_llm::{
    LlmAdapter, LlmError, LlmRequest, StopReason, StreamChunk, ToolCallDelta,
};
use nanobot_session::{LogEntry, SessionEvent, dangling_tool_calls};
use nanobot_tools::{ToolContract, ToolDefinition};

// -- fakes ------------------------------------------------------------------

/// A model that answers the first request with a tool call and every later one
/// with text, so a turn can run and a session can continue.
struct Scripted {
    calls: AtomicUsize,
    stop: StopReason,
}

impl Scripted {
    fn new(stop: StopReason) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            stop,
        }
    }
}

impl LlmAdapter for Scripted {
    fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
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

/// A model that answers every request with `text` — what a summarizer needs.
struct TextModel(&'static str);

impl LlmAdapter for TextModel {
    fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
        Box::pin(stream::iter(vec![Ok(StreamChunk::Text(self.0.to_string()))]))
    }
}

/// Records what the executor was asked to do.
#[derive(Default)]
struct RecordingExecutor {
    calls: AtomicUsize,
    arguments: Mutex<Vec<serde_json::Value>>,
}

impl ToolExecutor for RecordingExecutor {
    fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> String {
        "{}".to_string()
    }

    fn apply(&self, name: &str, arguments: &serde_json::Value) -> WriteOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.arguments.lock().unwrap().push(arguments.clone());
        WriteOutcome::ungated(self.execute(name, arguments))
    }
}

// -- helpers ----------------------------------------------------------------

fn scratch(label: &str) -> ProjectPaths {
    let root = std::env::temp_dir().join(format!("nb-e2e-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    ProjectPaths::new(&root)
}

fn tool(name: &str, contract: ToolContract, requires_approval: bool) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: "a test tool".to_string(),
        parameters_schema: serde_json::json!({"type": "object"}),
        requires_approval,
        contract,
        validate_args: false,
    }
}

/// A harness with the scripted model, one tool, and project paths so the log
/// persists.
fn harness(
    paths: &ProjectPaths,
    session_id: &str,
    definitions: Vec<ToolDefinition>,
    executor: Option<Arc<RecordingExecutor>>,
) -> Harness {
    let model = Arc::new(Scripted::new(StopReason::ToolUse));
    let mut builder = Harness::builder()
        .with_session_id(session_id)
        .with_project_paths(paths.clone())
        .with_llm_adapter("scripted", model)
        // Declared through the catalog: the production routing path.
        .with_model_route("scripted", "scripted", "wire-model")
        .with_model("scripted")
        .with_tool_definitions(definitions);
    if let Some(executor) = executor {
        builder = builder.with_tool_executor(executor);
    }
    builder.build().expect("assembly")
}

/// Write a log by hand — the shape a crashed or compacted run leaves.
fn write_log(dir: &std::path::Path, events: Vec<SessionEvent>) -> Vec<LogEntry> {
    let entries: Vec<LogEntry> = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| LogEntry {
            seq: index as u32,
            timestamp_ms: 0,
            event,
        })
        .collect();
    std::fs::create_dir_all(dir).expect("the conversation directory");
    nanobot_session::save(&dir.join("events.jsonl"), &entries).expect("write the log");
    entries
}

fn turns(mut events: Vec<SessionEvent>, count: usize) -> Vec<SessionEvent> {
    for turn in 0..count {
        events.push(SessionEvent::TurnStart { turn: turn as u32 });
        events.push(SessionEvent::UserMessage {
            content: format!("turn {turn} request"),
            selection_ref: None,
        });
        events.push(SessionEvent::AssistantMessage {
            content: format!("turn {turn} reply"),
        });
    }
    events
}

fn tool_results(harness: &Harness) -> Vec<String> {
    harness
        .agent
        .session()
        .events()
        .into_iter()
        .filter_map(|entry| match entry.event {
            SessionEvent::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .collect()
}

// -- the flow ---------------------------------------------------------------

#[test]
fn a_two_turn_session_round_trips_through_the_log() {
    let paths = scratch("round-trip");
    let session_id = "sess-e2e-round-trip";
    let executor = Arc::new(RecordingExecutor::default());

    {
        let harness = harness(
            &paths,
            session_id,
            vec![tool("measure", ToolContract::ReadOnly, false)],
            Some(executor.clone()),
        );

        run_one_turn(&harness, "measure the chip");
        run_one_turn(&harness, "now report");
    }
    // The block ends: `Drop` writes the clean marker.

    let events_path = paths.conversation(session_id).join("events.jsonl");
    let entries = nanobot_session::load(&events_path).expect("the log round-trips");

    // Both turns are on disk, in order.
    let turn_starts: Vec<_> = entries
        .iter()
        .filter(|entry| matches!(entry.event, SessionEvent::TurnStart { .. }))
        .collect();
    assert_eq!(turn_starts.len(), 2, "both turns persisted");

    // The tool call the model made has its result: the log is a valid turn.
    assert!(
        dangling_tool_calls(&entries).is_empty(),
        "no call is left unanswered"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry.event, SessionEvent::ToolCall { .. }))
            .count(),
        1
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1, "the tool ran once");

    // A rebuild with the marker present is not a recovery.
    let rebuilt = Harness::builder()
        .with_session_id(session_id)
        .with_project_paths(paths)
        .build()
        .expect("rebuild");
    assert!(!rebuilt.crash_recovered(), "a clean exit is not a crash");
}

#[test]
fn a_crash_mid_call_is_repaired_and_the_session_stays_usable() {
    let paths = scratch("crash");
    let session_id = "sess-e2e-crash";

    // A crashed run: a turn, a tool call, and no result. No clean marker.
    write_log(
        &paths.conversation(session_id),
        vec![
            SessionEvent::TurnStart { turn: 0 },
            SessionEvent::StepStart { turn: 0, step: 0 },
            SessionEvent::UserMessage {
                content: "build the counter".to_string(),
                selection_ref: None,
            },
            SessionEvent::ToolCall {
                id: "c1".into(),
                name: "build_sfq".into(),
                arguments: serde_json::json!({"spec": {}}),
                seq: 3,
            },
        ],
    );

    let recovered = harness(
        &paths,
        session_id,
        // Mutating and gated: the call must not be replayed.
        vec![tool("build_sfq", ToolContract::Mutating, true)],
        None,
    );

    assert!(recovered.crash_recovered(), "no marker means the last run crashed");
    assert!(
        recovered.repaired_calls().len() == 1,
        "the dangling call was resolved"
    );

    // The repair made the log a valid turn again.
    let events_path = paths.conversation(session_id).join("events.jsonl");
    let entries = nanobot_session::load(&events_path).expect("the repaired log");
    assert!(dangling_tool_calls(&entries).is_empty());
    assert!(
        tool_results(&recovered)[0].contains("interrupted"),
        "the model can see the call never returned"
    );

    // And the session still works: the next turn produces a valid history.
    let executor = Arc::new(RecordingExecutor::default());
    let usable = harness(
        &paths,
        session_id,
        vec![tool("measure", ToolContract::ReadOnly, false)],
        Some(executor),
    );
    run_one_turn(&usable, "measure it now");
    assert!(
        dangling_tool_calls(&usable.agent.session().events()).is_empty(),
        "the next turn left a valid history"
    );
}

#[test]
fn a_read_only_interrupted_call_is_replayed_on_resume() {
    let paths = scratch("resume");
    let session_id = "sess-e2e-resume";

    write_log(
        &paths.conversation(session_id),
        vec![
            SessionEvent::TurnStart { turn: 0 },
            SessionEvent::StepStart { turn: 0, step: 0 },
            SessionEvent::ToolCall {
                id: "c1".into(),
                name: "measure".into(),
                arguments: serde_json::json!({}),
                seq: 2,
            },
        ],
    );

    let executor = Arc::new(RecordingExecutor::default());
    let harness = harness(
        &paths,
        session_id,
        vec![tool("measure", ToolContract::ReadOnly, false)],
        Some(executor.clone()),
    );

    assert!(harness.has_pending_resume(), "a read-only call waits to be replayed");
    assert_eq!(harness.resume(), 1, "the replay ran");
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    assert!(
        dangling_tool_calls(&harness.agent.session().events()).is_empty(),
        "the replay answered it"
    );
}

#[test]
fn compaction_bounds_a_long_session_and_keeps_the_log() {
    let paths = scratch("compaction");
    let session_id = "sess-e2e-compaction";

    // A long session, written the way a real one would be left.
    let entries = write_log(
        &paths.conversation(session_id),
        turns(Vec::new(), 6),
    );
    let original_event_count = entries.len();

    // A tight threshold so the test does not need a huge session.
    let harness = harness_compacted(&paths, session_id);

    let compacted = block_on(harness.agent.compact_history());
    assert!(compacted, "the history was over the threshold");

    // One entry appended; nothing was deleted.
    assert_eq!(
        harness.agent.session().events().len(),
        original_event_count + 1,
        "the log keeps every event and gains the compaction entry"
    );

    let projected = harness.agent.session().derive_messages();
    assert!(
        projected.len() < original_event_count,
        "the model history is bounded, got {} from {original_event_count}",
        projected.len()
    );
    assert!(
        projected.iter().any(|message| matches!(
            message,
            nanobot_session::ProjectedMessage::System(text) if text.contains("design intent")
        )),
        "the summary is in the model history: {projected:?}"
    );

    // The clean marker discipline still holds for a compacted session.
    drop(harness);
    assert!(paths.conversation(session_id).join(CLEAN_MARKER).exists());
}

/// The same harness with a tight compaction threshold.
fn harness_compacted(paths: &ProjectPaths, session_id: &str) -> Harness {
    let model: Arc<dyn LlmAdapter> = Arc::new(TextModel(
        "the design intent so far: a 17-bit counter, no open questions",
    ));
    Harness::builder()
        .with_session_id(session_id)
        .with_project_paths(paths.clone())
        .with_llm_adapter("scripted", model)
        .with_model_route("scripted", "scripted", "wire-model")
        .with_model("scripted")
        .with_compaction_threshold(2)
        .with_tool_definitions(vec![tool("measure", ToolContract::ReadOnly, false)])
        .build()
        .expect("assembly")
}

#[test]
fn the_executor_receives_the_arguments_as_the_model_sent_them() {
    // End to end through the pipeline: the model writes a numeric string, the
    // pipeline coerces it, the executor sees a number.
    struct LaxModel {
        calls: AtomicUsize,
    }

    impl LlmAdapter for LaxModel {
        fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
            let chunks = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    Ok(StreamChunk::ToolCall(ToolCallDelta {
                        index: 0,
                        id: Some("call-1".into()),
                        name: Some("measure".into()),
                        arguments_fragment: r#"{"count": "17"}"#.into(),
                    })),
                    Ok(StreamChunk::Stop(StopReason::ToolUse)),
                ]
            } else {
                vec![Ok(StreamChunk::Text("done".into()))]
            };
            Box::pin(stream::iter(chunks))
        }
    }

    let paths = scratch("coercion");
    let session_id = "sess-e2e-coercion";
    let executor = Arc::new(RecordingExecutor::default());

    let definition = ToolDefinition {
        parameters_schema: serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"]
        }),
        ..tool("measure", ToolContract::ReadOnly, false)
    };

    let model: Arc<dyn LlmAdapter> = Arc::new(LaxModel { calls: AtomicUsize::new(0) });
    let harness = Harness::builder()
        .with_session_id(session_id)
        .with_project_paths(paths)
        .with_llm_adapter("scripted", model)
        .with_model_route("scripted", "scripted", "wire-model")
        .with_model("scripted")
        .with_tool_definitions(vec![definition])
        .with_tool_executor(executor.clone())
        .build()
        .expect("assembly");

    run_one_turn(&harness, "measure 17 things");

    assert_eq!(
        executor.arguments.lock().unwrap().as_slice(),
        &[serde_json::json!({"count": 17})],
        "the executor got the coerced integer, not the model's string"
    );
}

#[test]
fn a_model_script_runs_out_and_the_turn_still_completes() {
    // A model that answers nothing: the turn must end cleanly rather than hang
    // or loop.
    struct Silent;

    impl LlmAdapter for Silent {
        fn stream(&self, _request: LlmRequest) -> AdapterStream<'_> {
            Box::pin(stream::iter(Vec::<Result<StreamChunk, LlmError>>::new()))
        }
    }

    let paths = scratch("silent");
    let session_id = "sess-e2e-silent";
    let model = Arc::new(Silent);

    let harness = Harness::builder()
        .with_session_id(session_id)
        .with_project_paths(paths)
        .with_llm_adapter("scripted", model)
        .with_model_route("scripted", "scripted", "wire-model")
        .with_model("scripted")
        .with_tool_definitions(vec![tool("measure", ToolContract::ReadOnly, false)])
        .build()
        .expect("assembly");

    run_one_turn(&harness, "say something");

    let reasons: Vec<_> = harness
        .agent
        .session()
        .events()
        .into_iter()
        .filter_map(|entry| match entry.event {
            SessionEvent::TurnEnd { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "the turn ended");
}
