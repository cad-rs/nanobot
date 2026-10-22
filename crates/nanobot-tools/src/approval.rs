//! ApprovalPolicy: plugin for tool-level approval (design-harness 10.5).
//!
//! Two implementations are provided:
//! - `UiApprovalPolicy`: bridges approval requests to the main thread via
//!   a callback (the main thread presents the approval UI and resolves
//!   the future).
//! - `AutoApprovalPolicy`: always approves (headless / CI).
//!
//! Tool-level `Ask` (fine-grained, e.g. destructive imports) and plan-level
//! `Proposed -> Accept` (coarse-grained) coexist. When a tool-level approval
//! request appears mid-plan-execution, the session enters "awaiting review"
//! state.

use std::sync::Arc;

/// Trait for tool-level approval decisions.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// threads on desktop and used within a single-threaded wasm Worker.
pub trait ApprovalPolicy: Send + Sync {
    /// Request approval for a tool call.
    ///
    /// Returns `true` if the tool call is approved, `false` if denied.
    fn request_approval(&self, tool_call_id: &str, reason: &str) -> bool;
}

/// Approval policy that bridges to the main thread via a callback.
///
/// The callback receives `(tool_call_id, reason)` and returns `true` if
/// approved. The callback is expected to send an `approval/request` event
/// to the main thread and block until the user responds.
pub struct UiApprovalPolicy {
    callback: Arc<dyn Fn(&str, &str) -> bool + Send + Sync>,
}

impl UiApprovalPolicy {
    /// Create a new UI-backed approval policy.
    pub fn new(callback: impl Fn(&str, &str) -> bool + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }
}

impl ApprovalPolicy for UiApprovalPolicy {
    fn request_approval(&self, tool_call_id: &str, reason: &str) -> bool {
        (self.callback)(tool_call_id, reason)
    }
}

/// Approval policy that always approves (headless / CI).
pub struct AutoApprovalPolicy;

impl ApprovalPolicy for AutoApprovalPolicy {
    fn request_approval(&self, _tool_call_id: &str, _reason: &str) -> bool {
        true
    }
}

/// Approval policy that always denies (for testing).
pub struct DenyAllPolicy;

impl ApprovalPolicy for DenyAllPolicy {
    fn request_approval(&self, _tool_call_id: &str, _reason: &str) -> bool {
        false
    }
}
