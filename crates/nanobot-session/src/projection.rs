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
    let mut messages = Vec::new();

    for entry in entries {
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
            _ => {}
        }
    }

    messages
}
