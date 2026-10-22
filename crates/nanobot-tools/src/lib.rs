//! nanobot-tools: the tool registry and the guarded dispatch pipeline.
//!
//! This crate owns:
//! - [`ToolRegistry`] — name + JSON Schema + approval requirement + contract
//! - [`ToolPipeline`] — the three waterfalls (`pre_execute` / `execute` /
//!   `post_execute`) around a caller-supplied async body
//! - [`ApprovalPolicy`] — the pluggable allow / deny / ask decision
//!
//! The pipeline never runs a tool itself: `execute` takes the tool body as a
//! closure, so the execution backend stays the product's (an in-process
//! engine, a worker bridge, a remote call — the framework does not care).
//!
//! Argument validation against the declared JSON Schema is the product's
//! concern: the pipeline only checks that the arguments are a JSON object.
//! (The hook point is `pre_execute` step 2.)
//!
//! Provenance: extracted from the ocad design harness (see
//! `docs/nanobot-architecture.md` in rcad-pro). The CAD tool bodies and the
//! tool-name -> contract map moved back to the product.

pub mod approval;
pub mod contract;
pub mod error;
pub mod registry;
pub mod sandbox;
pub mod waterfall;

pub use approval::{ApprovalPolicy, AutoApprovalPolicy, DenyAllPolicy, UiApprovalPolicy};
pub use contract::ToolContract;
pub use error::ToolError;
pub use registry::{ToolDefinition, ToolRegistry};
pub use sandbox::{PathSandbox, SandboxDecision, SandboxMode};
pub use waterfall::{PostExecuteResult, PreExecuteResult, ToolPipeline};
