//! nanobot-session: the event-sourced session.
//!
//! This crate owns the session event log (single source of truth), monotonic
//! seq assignment, `derive_messages()` projection, JSONL persistence, and
//! time-travel forking.
//!
//! The event log is the sole source of truth for the session. Model history
//! is projected from it via `derive_messages()` — never stored separately.
//! "Model-visible means logged."
//!
//! ## Event vocabulary
//!
//! The base vocabulary covers the harness lifecycle:
//!
//! - Turn/step: `turn/start`, `turn/end`, `step/start`, `step/end`
//! - Messages: `user/message`, `assistant/chunk`, `assistant/message`
//! - Tools: `tool/call`, `tool/result`
//! - Plan: `plan/proposed`, `plan/accepted`, `plan/rejected`,
//!   `plan/contingent`, `plan/clarify`
//! - Verification: `check/run`, `check/result`
//! - Approval: `approval/request`, `approval/decision`
//! - Domain-neutral: `geometry/revision`, `session/forked`, `request/header`
//!
//! Domain-specific events ride in [`SessionEvent::Extension`] — the framework
//! stores and replays them opaquely, and the product projects them into its
//! own types.
//!
//! ## Projection rules
//!
//! `derive_messages()` projects `user/message`, `assistant/message`,
//! `tool/call` + `tool/result` pairs, and `plan/rejected`. Everything else —
//! including `Extension` unless the product registers a projector — serves UI
//! replay and audit only.

pub mod event;
pub mod persistence;
pub mod projection;
pub mod session;

pub use event::{PlanProposal, SessionEvent, TurnEndReason, Usage};
pub use persistence::default_path;

#[cfg(not(target_family = "wasm"))]
pub use persistence::{load, save};
pub use projection::{derive_messages, ProjectedMessage};
pub use session::{Disposable, LogEntry, NoopDisposable, Session};
