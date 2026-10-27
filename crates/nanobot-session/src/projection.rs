//! Projection: derive model-visible messages from the event log
//! (design-harness 10.5).
//!
//! "Model-visible means logged." Only these event types enter model history:
//! - `user/message`
//! - `assistant/message`
//! - `tool/call` + `tool/result` pairs
//! - `plan/rejected` (model needs to know a plan was rejected and why)
//!
//! All other events (`plan/*` except rejected, `check/*`, `approval/*`,
//! `turn/*`, `step/*`, `geometry/*`, `request/header`) serve UI replay and
//! audit only.
//!
//! ## Compaction
//!
//! A [`SessionEvent::CompactionEntry`] covers a log range: `derive_messages`
//! emits its summary **in place of** the covered events, so a long session
//! stays inside its context window while the log keeps every event. An entry
//! covered by a later one is superseded and emits nothing.

use crate::event::SessionEvent;
use crate::session::LogEntry;

/// A projected message ready for model history.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProjectedMessage {
    System(String),
    User(String),
    Assistant(String),
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        id: String,
        success: bool,
        output: String,
    },
}

/// Project model-visible messages from a slice of log entries.
///
/// This function filters and transforms the event log into the minimal
/// message history the model needs. It is a pure function of the log.
pub fn derive_messages(entries: &[LogEntry]) -> Vec<ProjectedMessage> {
    // The ranges the *live* compaction entries cover. A compaction covered by a
    // later one is superseded: it emits nothing and its range does not hide
    // events the later summary already replaced.
    let compactions: Vec<&LogEntry> = entries
        .iter()
        .filter(|entry| matches!(entry.event, SessionEvent::CompactionEntry { .. }))
        .collect();
    let live: Vec<&LogEntry> = compactions
        .iter()
        .copied()
        .filter(|entry| {
            !compactions.iter().any(|other| {
                other.seq != entry.seq && covers(other, entry.seq)
            })
        })
        .collect();

    let mut messages = Vec::new();
    let mut emitted: Vec<u32> = Vec::new();
    let mut iter = entries.iter().peekable();

    while let Some(entry) = iter.next() {
        // A live compaction starts here: its summary stands in for the whole
        // covered range, at the range's position in the narrative.
        if let Some(compaction) = live.iter().find(|c| from_seq(c) == entry.seq) {
            let to_seq = to_seq(compaction);
            messages.push(ProjectedMessage::System(summary(compaction).to_string()));
            emitted.push(compaction.seq);
            while iter.peek().is_some_and(|next| next.seq <= to_seq) {
                iter.next();
            }
            continue;
        }

        // Covered by a live compaction whose start was never reached (the log
        // was rewound past it): the summary still stands, at the entry's own
        // position.
        if let Some(compaction) = live.iter().find(|c| covers(c, entry.seq)) {
            if !emitted.contains(&compaction.seq) {
                messages.push(ProjectedMessage::System(summary(compaction).to_string()));
                emitted.push(compaction.seq);
            }
            continue;
        }

        match &entry.event {
            SessionEvent::UserMessage { content, .. } => {
                messages.push(ProjectedMessage::User(content.clone()));
            }
            SessionEvent::AssistantMessage { content } => {
                messages.push(ProjectedMessage::Assistant(content.clone()));
            }
            SessionEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                messages.push(ProjectedMessage::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
            SessionEvent::ToolResult {
                id,
                success,
                output,
                ..
            } => {
                messages.push(ProjectedMessage::ToolResult {
                    id: id.clone(),
                    success: *success,
                    output: output.clone(),
                });
            }
            SessionEvent::PlanRejected { reason, .. } => {
                messages.push(ProjectedMessage::System(format!("Plan rejected: {reason}")));
            }
            SessionEvent::CompactionEntry { .. } => {
                // Reached only when it is not live (superseded) or its range
                // was already emitted above.
            }
            _ => {}
        }
    }

    // A live compaction whose start was rewound past: emit it at the end.
    for compaction in &live {
        if !emitted.contains(&compaction.seq) {
            messages.push(ProjectedMessage::System(summary(compaction).to_string()));
        }
    }

    messages
}

/// The first covered log position of a compaction entry.
fn from_seq(entry: &LogEntry) -> u32 {
    match &entry.event {
        SessionEvent::CompactionEntry { from_seq, .. } => *from_seq,
        _ => entry.seq,
    }
}

/// The last covered log position of a compaction entry.
fn to_seq(entry: &LogEntry) -> u32 {
    match &entry.event {
        SessionEvent::CompactionEntry { to_seq, .. } => *to_seq,
        _ => entry.seq,
    }
}

/// The summary a compaction entry carries.
fn summary(entry: &LogEntry) -> &str {
    match &entry.event {
        SessionEvent::CompactionEntry { summary, .. } => summary,
        _ => "",
    }
}

/// Whether `compaction` covers log position `seq`.
fn covers(compaction: &LogEntry, seq: u32) -> bool {
    match &compaction.event {
        SessionEvent::CompactionEntry { from_seq, to_seq, .. } => {
            *from_seq <= seq && seq <= *to_seq
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn messages(session: &Session) -> Vec<ProjectedMessage> {
        derive_messages(&session.events())
    }

    #[test]
    fn a_compaction_entry_replaces_the_events_it_covers() {
        let session = Session::new("sess-compact");
        session.append(SessionEvent::UserMessage {
            content: "old request".to_string(),
        });
        session.append(SessionEvent::AssistantMessage {
            content: "old answer".to_string(),
        });
        let covered_up_to = session.events().last().unwrap().seq;

        session.append(SessionEvent::UserMessage {
            content: "new request".to_string(),
        });
        session.append(SessionEvent::CompactionEntry {
            from_seq: 0,
            to_seq: covered_up_to,
            summary: "earlier: the chip was a 17-bit counter".to_string(),
        });

        let projected = messages(&session);
        assert_eq!(
            projected,
            vec![
                ProjectedMessage::System(
                    "earlier: the chip was a 17-bit counter".to_string()
                ),
                ProjectedMessage::User("new request".to_string()),
            ],
            "the covered events are replaced by the summary"
        );
    }

    #[test]
    fn the_log_keeps_the_events_compaction_replaced() {
        // Compaction is a projection record, not a rewrite: the audit trail
        // keeps everything.
        let session = Session::new("sess-compact-audit");
        session.append(SessionEvent::UserMessage {
            content: "old".to_string(),
        });
        let covered_up_to = session.events().last().unwrap().seq;
        session.append(SessionEvent::CompactionEntry {
            from_seq: 0,
            to_seq: covered_up_to,
            summary: "summary".to_string(),
        });

        assert_eq!(
            session.events().len(),
            2,
            "the covered event stays in the log"
        );
        assert_eq!(session.derive_messages().len(), 1);
    }

    #[test]
    fn a_later_compaction_supersedes_an_earlier_one() {
        let session = Session::new("sess-compact-twice");
        session.append(SessionEvent::UserMessage {
            content: "a".to_string(),
        });
        session.append(SessionEvent::CompactionEntry {
            from_seq: 0,
            to_seq: 0,
            summary: "first summary".to_string(),
        });
        let covered_up_to = session.events().last().unwrap().seq;
        session.append(SessionEvent::CompactionEntry {
            from_seq: 0,
            to_seq: covered_up_to,
            summary: "second summary".to_string(),
        });

        let projected = messages(&session);
        assert_eq!(
            projected,
            vec![ProjectedMessage::System("second summary".to_string())],
            "only the later summary is emitted"
        );
    }

    #[test]
    fn a_log_without_compaction_is_unchanged() {
        let session = Session::new("sess-compact-none");
        session.append(SessionEvent::UserMessage {
            content: "hi".to_string(),
        });
        assert_eq!(
            messages(&session),
            vec![ProjectedMessage::User("hi".to_string())]
        );
    }
}
