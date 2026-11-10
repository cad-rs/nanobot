//! Error types for the tool pipeline (design-harness 10.5).

use thiserror::Error;

/// Errors that can occur during tool registration or execution.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The requested tool is not registered. Includes the available tool names.
    #[error("unknown tool: {0}. Available: {1:?}")]
    UnknownTool(String, Vec<String>),
    /// Attempted to register a tool with a duplicate name.
    #[error("duplicate tool: {0}")]
    DuplicateTool(String),
    /// Approval was denied by the user or policy.
    #[error("approval denied")]
    ApprovalDenied,
    /// The tool pipeline was cancelled mid-execution.
    #[error("cancelled")]
    Cancelled,
}
