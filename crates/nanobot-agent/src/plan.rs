//! Plan state machine: eventized plan lifecycle (design-harness 10.5).
//!
//! All states from the main design doc 14.3 land in the event log:
//! `plan/proposed` -> `plan/accepted` / `plan/rejected` / `plan/contingent`.
//!
//! The plan state machine is eventized: each transition appends a
//! `SessionEvent::Plan*` to the session log, ensuring the log is the
//! single source of truth for plan state.

// The intent is an opaque JSON value: the product parses it into whatever its verifier needs.
use serde::{Deserialize, Serialize};

/// The lifecycle state of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    /// AI is assembling the plan (UI-only state; agent loop starts at Proposed).
    Drafting,
    /// Plan has been proposed by the model, awaiting decision.
    Proposed,
    /// Plan was accepted (by user or auto-policy), tools are executing.
    Executing,
    /// Tools have finished; intent checks are running.
    Verifying,
    /// Plan completed successfully (intent satisfied or no intent declared).
    Settled,
    /// Plan was rejected (model needs to know to avoid re-proposing).
    Rejected,
    /// Plan is contingent on an external condition (intent check failed).
    Contingent,
    /// AI needs more information before proposing (drafting-stage clarification).
    Clarify,
}

/// A plan with its current state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Unique plan identifier.
    pub plan_id: String,
    /// Human-readable plan summary.
    pub summary: String,
    /// JSON-encoded plan steps.
    pub steps: serde_json::Value,
    /// Current lifecycle state.
    pub state: PlanState,
    /// Plan-level acceptance criteria (design-harness 22.2.4).
    /// The agent's declared intent, opaque to the framework (the product's
    /// verifier parses it).
    pub intent: Option<serde_json::Value>,
}

impl Plan {
    /// Create a new plan in the `Proposed` state.
    pub fn new(
        plan_id: impl Into<String>,
        summary: impl Into<String>,
        steps: serde_json::Value,
    ) -> Self {
        Self {
            plan_id: plan_id.into(),
            summary: summary.into(),
            steps,
            state: PlanState::Proposed,
            intent: None,
        }
    }

    /// Create a new plan with a declared intent.
    pub fn with_intent(
        plan_id: impl Into<String>,
        summary: impl Into<String>,
        steps: serde_json::Value,
        intent: serde_json::Value,
    ) -> Self {
        Self {
            plan_id: plan_id.into(),
            summary: summary.into(),
            steps,
            state: PlanState::Proposed,
            intent: Some(intent),
        }
    }

    /// Transition to `Executing` (plan accepted, tools running).
    pub fn begin_execution(&mut self) {
        self.state = PlanState::Executing;
    }


    /// Transition to `Settled` (terminal — intent satisfied or no intent).
    pub fn settle(&mut self) {
        self.state = PlanState::Settled;
    }


    /// Send the plan back to `Drafting`, keeping its context so the model can
    /// revise it (design-harness 14.3, the decision bar's Modify).
    pub fn restart_drafting(&mut self) {
        self.state = PlanState::Drafting;
    }

    /// Transition to `Contingent` (intent check failed).
    pub fn mark_contingent(&mut self) {
        self.state = PlanState::Contingent;
    }

    /// Transition to `Clarify` (AI needs more information before proposing).
    pub fn request_clarification(&mut self) {
        self.state = PlanState::Clarify;
    }



}
