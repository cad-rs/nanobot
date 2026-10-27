//! ocad-agent: agent handle and turn/step driver (design-harness 10.5).
//!
//! This crate owns the agent loop driver:
//! - `Agent` handle — `followup` / `steer` / `inject` / `cancel` / `run`
//! - `Inbox` — dual-queue message buffer (next-turn + next-step)
//! - `Plan` — eventized plan state machine
//! - `CancellationToken` — per-turn cancellation
//! - `JobQueue` — work that outlives a turn (multi-fidelity verification)
//!
//! The agent loop drives the turn/step cycle:
//! ```text
//! turn/start -> step/start -> user/message -> assemble system+tools
//!   -> agent/request waterfall -> stream chunks -> assistant/message
//!   -> dispatch tools -> step/end -> (loop or turn/end)
//! ```
//!
//! ## Prompt section assembly
//!
//! The `prompt/assemble` waterfall (in `nanobot-cordis`) allows skill manifests
//! to inject persona sections and step templates. The agent loop calls
//! this waterfall before building the LLM request.
//!
//! ## Plan state machine
//!
//! Plan transitions are eventized: `plan/proposed` -> `plan/accepted` /
//! `plan/rejected` / `plan/contingent`. All transitions land in the session
//! event log, making the log the single source of truth for plan state.

pub mod agent;
pub mod batch;
pub mod cancellation;
pub mod inbox;
pub mod job;
pub mod plan;

pub use agent::{
    Agent, AgentState, PlanVerifier, ReplayOutcome, RestorePoint, ToolExecutor, WriteCheck,
    WriteOutcome,
};
pub use batch::{Axis, BatchOutcome, BatchSpec, Variant, VariantOutcome};
pub use cancellation::CancellationToken;
pub use inbox::Inbox;
pub use job::{Job, JobContext, JobEvent, JobQueue, JobResult, JobRunner, JobSpec, JobState};
pub use plan::{Plan, PlanState};
