//! `SessionEvent`: the immutable discriminated union that forms the session
//! event log — the framework's base vocabulary, plus one open variant for
//! product-defined events.
//!
//! The event log is the **single source of truth** for the session. Model
//! history is projected from it via `derive_messages()` — never stored
//! separately. Events that are not model-visible (`plan/*` except
//! `rejected`, `check/*`, `approval/*`, `Extension`) serve UI replay and audit
//! only.
//!
//! The base vocabulary covers the harness lifecycle (turn/step, messages,
//! tools, plan, approval, verification, revision, session). Anything
//! domain-specific rides in [`SessionEvent::Extension`], which keeps the log
//! self-describing without making the framework know the product's nouns.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Reason a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Error,
    MaxSteps,
}

/// Token usage statistics for a model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// A plan proposal from the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanProposal {
    pub plan_id: String,
    /// Human-readable plan summary.
    pub summary: String,
    /// JSON-encoded plan steps.
    pub steps: serde_json::Value,
}


/// The session event: the framework's base vocabulary plus the
/// [`SessionEvent::Extension`] variant for product-defined events.
///
/// Each event is immutable once appended to the log. The `seq` field is
/// assigned by `Session::append` and is not part of the variant data — it
/// is stored in the `LogEntry` wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    // -- turn / step lifecycle --
    /// A new turn has started.
    TurnStart { turn: u32 },
    /// The current turn has ended.
    TurnEnd { turn: u32, reason: TurnEndReason },
    /// A new step within the current turn has started.
    StepStart { turn: u32, step: u32 },
    /// The current step has ended.
    StepEnd { turn: u32, step: u32 },

    // -- user / assistant messages --
    /// User input message.
    UserMessage {
        content: String,
        /// Optional selection reference (design-harness F2 check-item spatialization).
        selection_ref: Option<String>,
    },
    /// Incremental assistant text chunk (streaming delta).
    AssistantChunk { text: String },
    /// Complete assistant message.
    AssistantMessage { content: String },

    // -- tool calls --
    /// A tool call was dispatched.
    ToolCall {
        /// Unique call identifier.
        id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded arguments.
        arguments: serde_json::Value,
        /// Command-stream position (history entry ordinal).
        seq: u32,
    },
    /// A tool call result was received.
    ToolResult {
        /// Matches the `id` from the corresponding `ToolCall`.
        id: String,
        success: bool,
        /// Tool output (text or error message).
        output: String,
        /// Topology signature for post-replay verification (AR1).
        topology_signature: Option<String>,
    },

    // -- plan state machine --
    /// Model proposed a plan.
    PlanProposed { plan: PlanProposal },
    /// Plan was accepted by the user or auto-policy.
    PlanAccepted { plan_id: String },
    /// Plan was rejected (model needs to know to avoid re-proposing).
    PlanRejected { plan_id: String, reason: String },
    /// Plan is contingent on an external condition.
    PlanContingent { plan_id: String, condition: String },
    /// AI needs more information before proposing (drafting-stage clarification).
    PlanClarify {
        plan_id: String,
        questions: Vec<String>,
    },

    // -- check / verification --
    /// A verifier check was initiated.
    CheckRun { check_id: String },
    /// A verifier check completed. The raw measurements ride the event (the
    /// regrade surface: numbers, not just prose), stamped with the scorer
    /// version that graded them when the checks carry a versioned scoring
    /// contract.
    CheckResult {
        check_id: String,
        passed: bool,
        details: String,
        #[serde(default)]
        measurements: BTreeMap<String, f64>,
        #[serde(default)]
        scorer_version: Option<String>,
    },

    // -- approval --
    /// An approval request was raised (tool-level Ask).
    ApprovalRequest {
        tool_call_id: String,
        reason: String,
    },
    /// An approval decision was made.
    ApprovalDecision {
        tool_call_id: String,
        approved: bool,
    },

    // -- geometry --
    /// Engine-side geometry revision notification.
    GeometryRevision { revision: u32 },

    // -- session management --
    /// A new session was forked from this session's log prefix.
    SessionForked {
        parent_session: String,
        parent_seq: u32,
    },

    /// Per-model-request header (model, usage, cache info).
    RequestHeader { model: String, usage: Option<Usage> },

    // -- extension point --
    /// A **product-defined** event.
    ///
    /// `domain` namespaces the product (`"sketch"`, `"pcb"`, …); `kind` and
    /// `payload` are whatever that product defines. The framework stores and
    /// replays the event opaquely — it never interprets it — so a harness can
    /// record its own domain events without the framework growing a variant
    /// per product.
    ///
    /// The log stays self-describing: a reader sees which product emitted the
    /// event. The cost is that the payload is not statically typed here; the
    /// emitting product deserializes it back into its own type.
    ///
    /// Projection: `Extension` is model-visible only when the product
    /// registers a projector (see `derive_messages`); otherwise it serves UI
    /// replay and audit, like the other non-model events.
    Extension {
        /// Product namespace, e.g. `"sketch"`.
        domain: String,
        /// Product-defined event kind, e.g. `"proposal.created"`.
        kind: String,
        /// Product-defined payload (the product's own serde type).
        payload: serde_json::Value,
    },
}

impl SessionEvent {
    /// Get the event type name as a string slice.
    pub fn type_name(&self) -> &'static str {
        match self {
            SessionEvent::TurnStart { .. } => "turn_start",
            SessionEvent::TurnEnd { .. } => "turn_end",
            SessionEvent::StepStart { .. } => "step_start",
            SessionEvent::StepEnd { .. } => "step_end",
            SessionEvent::UserMessage { .. } => "user_message",
            SessionEvent::AssistantChunk { .. } => "assistant_chunk",
            SessionEvent::AssistantMessage { .. } => "assistant_message",
            SessionEvent::ToolCall { .. } => "tool_call",
            SessionEvent::ToolResult { .. } => "tool_result",
            SessionEvent::PlanProposed { .. } => "plan_proposed",
            SessionEvent::PlanAccepted { .. } => "plan_accepted",
            SessionEvent::PlanRejected { .. } => "plan_rejected",
            SessionEvent::PlanContingent { .. } => "plan_contingent",
            SessionEvent::PlanClarify { .. } => "plan_clarify",
            SessionEvent::CheckRun { .. } => "check_run",
            SessionEvent::CheckResult { .. } => "check_result",
            SessionEvent::ApprovalRequest { .. } => "approval_request",
            SessionEvent::ApprovalDecision { .. } => "approval_decision",
            SessionEvent::GeometryRevision { .. } => "geometry_revision",
            SessionEvent::SessionForked { .. } => "session_forked",
            SessionEvent::RequestHeader { .. } => "request_header",
            // The product's own kind lives in the payload; the framework only
            // reports that this was an extension event.
            SessionEvent::Extension { .. } => "extension",
        }
    }
}
