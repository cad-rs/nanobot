//! Compaction: the model-visible history is bounded by summarizing its older
//! turns, while the log keeps every event.

use std::sync::Arc;

use nanobot_agent::Agent;
use nanobot_llm::{LlmRuntime, ScriptedAdapter, DEFAULT_PROVIDER};
use nanobot_session::{ProjectedMessage, Session, SessionEvent};
use nanobot_tools::{AutoApprovalPolicy, ToolPipeline, ToolRegistry};

/// A session with `turns` turns of `messages_per_turn` user/assistant pairs.
fn session_with_turns(turns: usize, messages_per_turn: usize) -> Session {
    let session = Session::new("sess-compaction");
    for turn in 0..turns {
        session.append(SessionEvent::TurnStart { turn: turn as u32 });
        for index in 0..messages_per_turn {
            session.append(SessionEvent::UserMessage {
                content: format!("turn {turn} message {index}"),
                selection_ref: None,
            });
            session.append(SessionEvent::AssistantMessage {
                content: format!("turn {turn} reply {index}"),
            });
        }
    }
    session
}

fn agent(session: Session, summary: &str) -> Agent {
    let mut runtime = LlmRuntime::new();
    runtime.register(DEFAULT_PROVIDER, Arc::new(ScriptedAdapter::text(summary)));
    let registry = Arc::new(ToolRegistry::new());
    Agent::new(session, runtime, ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy)))
}

#[test]
fn compaction_is_disabled_by_a_zero_threshold() {
    let agent = agent(session_with_turns(8, 2), "summary");
    agent.set_compaction_threshold(0);

    assert!(!futures_executor::block_on(agent.compact_history()));
}

#[test]
fn under_the_threshold_nothing_is_compacted() {
    let agent = agent(session_with_turns(2, 2), "summary");
    agent.set_compaction_threshold(400);

    let before = agent.session().events().len();
    assert!(!futures_executor::block_on(agent.compact_history()));
    assert_eq!(agent.session().events().len(), before);
}

#[test]
fn compaction_summarizes_the_older_turns_and_keeps_the_log() {
    let agent = agent(session_with_turns(6, 2), "a 17-bit counter design");
    agent.set_compaction_threshold(4);

    let events_before = agent.session().events().len();
    assert!(futures_executor::block_on(agent.compact_history()));

    // The log keeps every event, plus one compaction entry: compaction is a
    // projection record, not a rewrite.
    assert_eq!(agent.session().events().len(), events_before + 1);

    let projected = agent.session().derive_messages();
    // The newest turn is always kept whole; everything older is the summary.
    assert!(
        projected.len() <= 5,
        "the projection is bounded, got {} messages: {projected:?}",
        projected.len()
    );
    assert!(
        projected
            .iter()
            .any(|m| matches!(m, ProjectedMessage::System(t) if t.contains("17-bit counter"))),
        "the summary is in the model history: {projected:?}"
    );
    assert!(
        !projected
            .iter()
            .any(|m| matches!(m, ProjectedMessage::User(t) if t.contains("turn 0"))),
        "the covered turns are gone from the model history: {projected:?}"
    );
}

#[test]
fn the_compaction_entry_carries_the_range_it_covered() {
    let agent = agent(session_with_turns(6, 2), "summary");
    agent.set_compaction_threshold(4);
    futures_executor::block_on(agent.compact_history());

    let (from_seq, to_seq) = agent
        .session()
        .events()
        .into_iter()
        .find_map(|entry| match entry.event {
            SessionEvent::CompactionEntry { from_seq, to_seq, .. } => Some((from_seq, to_seq)),
            _ => None,
        })
        .expect("a compaction entry");
    assert!(from_seq < to_seq, "the range covers events");
}
