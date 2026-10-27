//! Crash-recovery analysis of the session log.
//!
//! A crash between a tool call and its result leaves the log with a **dangling
//! call**: a `tool_call` that no `tool_result` answers. That is an invalid turn
//! for every provider — the next request would carry a call the model never got
//! an answer to, and the provider rejects the history. So recovery has to
//! resolve every dangling call before the session is used again: replay it when
//! it is safe, or record its outcome as unknown.
//!
//! This module only *reads* the log. Deciding what is safe to replay, and
//! driving the replay, belong to the harness (it owns the registry and the
//! executor).

use crate::event::SessionEvent;
use crate::session::LogEntry;
use serde_json::Value;

/// A tool call the log records but never answered.
#[derive(Debug, Clone, PartialEq)]
pub struct DanglingCall {
    /// The call id the matching `ToolResult` would carry.
    pub id: String,
    /// The tool the call named.
    pub name: String,
    /// The arguments the model sent.
    pub arguments: Value,
    /// The log position of the call.
    pub seq: u32,
    /// The turn and step the call was made in.
    pub turn: u32,
    pub step: u32,
}

/// Find every tool call in the log that has no matching result.
///
/// A clean log has none: the loop answers every call it logs, including the
/// ones it refuses. A crash mid-call is the case this is for.
pub fn dangling_tool_calls(entries: &[LogEntry]) -> Vec<DanglingCall> {
    let mut open: Vec<DanglingCall> = Vec::new();
    let mut turn = 0u32;
    let mut step = 0u32;

    for entry in entries {
        match &entry.event {
            SessionEvent::StepStart {
                turn: current_turn,
                step: current_step,
            } => {
                turn = *current_turn;
                step = *current_step;
            }
            SessionEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => open.push(DanglingCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                seq: entry.seq,
                turn,
                step,
            }),
            SessionEvent::ToolResult { id, .. } => {
                open.retain(|call| &call.id != id);
            }
            _ => {}
        }
    }

    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn entries(session: &Session) -> Vec<LogEntry> {
        session.events()
    }

    #[test]
    fn a_clean_log_has_no_dangling_calls() {
        let session = Session::new("sess-clean");
        session.append(SessionEvent::StepStart { turn: 0, step: 0 });
        session.append(SessionEvent::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
            seq: session.next_seq(),
        });
        session.append(SessionEvent::ToolResult {
            id: "c1".into(),
            success: true,
            output: "{}".into(),
        });
        assert!(dangling_tool_calls(&entries(&session)).is_empty());
    }

    #[test]
    fn a_call_without_a_result_is_dangling() {
        let session = Session::new("sess-dangling");
        session.append(SessionEvent::StepStart { turn: 2, step: 1 });
        session.append(SessionEvent::ToolCall {
            id: "c9".into(),
            name: "build_sfq".into(),
            arguments: serde_json::json!({"spec": {}}),
            seq: session.next_seq(),
        });

        let dangling = dangling_tool_calls(&entries(&session));
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0].id, "c9");
        assert_eq!(dangling[0].name, "build_sfq");
        assert_eq!(dangling[0].turn, 2, "the call's turn is reported");
        assert_eq!(dangling[0].step, 1);
        assert_eq!(dangling[0].arguments, serde_json::json!({"spec": {}}));
    }

    #[test]
    fn a_result_after_the_call_resolves_it() {
        let session = Session::new("sess-resolved");
        session.append(SessionEvent::ToolCall {
            id: "a".into(),
            name: "t".into(),
            arguments: serde_json::json!({}),
            seq: session.next_seq(),
        });
        session.append(SessionEvent::ToolResult {
            id: "b".into(),
            success: false,
            output: "refused".into(),
        });
        // The refusal is still an answer: `a` stays dangling only if nothing
        // named it, so a differently-named result does not resolve it.
        let dangling = dangling_tool_calls(&entries(&session));
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0].id, "a");
    }

    #[test]
    fn a_refused_call_is_answered_and_not_dangling() {
        // The loop logs refused calls too, so a refused call must not look like
        // an interrupted one.
        let session = Session::new("sess-refused");
        session.append(SessionEvent::ToolCall {
            id: "r".into(),
            name: "t".into(),
            arguments: serde_json::json!({}),
            seq: session.next_seq(),
        });
        session.append(SessionEvent::ToolResult {
            id: "r".into(),
            success: false,
            output: "{\"ok\":false}".into(),
        });
        assert!(dangling_tool_calls(&entries(&session)).is_empty());
    }

    #[test]
    fn a_revert_removes_calls_it_rewound_past() {
        // `revert_to(seq)` keeps events up to `seq` and appends the marker. A
        // call rewound past is gone from the log rather than left dangling.
        let session = Session::new("sess-revert");
        session.append(SessionEvent::StepStart { turn: 0, step: 0 });
        session.append(SessionEvent::ToolCall {
            id: "old".into(),
            name: "t".into(),
            arguments: serde_json::json!({}),
            seq: session.next_seq(),
        });
        session.revert_to(0);
        assert!(dangling_tool_calls(&entries(&session)).is_empty());
    }
}
