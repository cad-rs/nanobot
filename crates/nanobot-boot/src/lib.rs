//! nanobot-boot: harness assembly + identity + CLI/selfcheck + the generic
//! project container (GUI-free).
//!
//! This crate is the framework's front door. It owns:
//!
//! - [`Harness`] / [`HarnessBuilder`] — assemble the kernel crates (session,
//!   llm, tools, agent, skills, cordis) into a live harness. Every `with_*`
//!   input is a product type; a panicking skill cannot sink the assembly.
//! - [`harness_toml`] — the `harness.toml` identity descriptor (the role
//!   `product.toml` plays for workbench-rs). Missing or malformed is never
//!   fatal.
//! - [`cli`] / [`selfcheck`] — the shared CLI modes (`--selfcheck`,
//!   `--selfcheck-crash`, `--agent-smoke`, `--agent-run`, `--smoke`) and the
//!   probe-style headless selfcheck with the three-phase crash-recovery
//!   probe.
//! - [`container`] — the generic project container: [`ProjectPaths`], the
//!   [`Manifest`] contract, and [`Project`] (open / create / save / session
//!   index with canonical-CRC32 sealing). The manifest type is the
//!   product's; the container is the framework's.
//!
//! Provenance: the assembly was lifted from the ocad design harness
//! (`apps/ocad/src/agent_host.rs::assemble`), the container from the ocad
//! project layer. See `docs/design.md` in the nanobot repository.
//!
//! Discipline: this crate stays GUI-free — no `gpui*`, no `wgpu`, no rcad /
//! ocad dependencies — so CLI, tests, and MCP servers can consume the kernel.

pub mod canonical;
pub mod cli;
pub mod container;
pub mod error;
pub mod harness;
pub mod harness_toml;
pub mod selfcheck;

pub use cli::{Cli, CliMode};
pub use container::{
    validate_relative_path, ChecksumState, Manifest, PackageRef, PackageSelection, Project,
    ProjectInfo, ProjectPaths, SessionEntry, SessionIndex, SessionStatus, STATE_DIR,
    MANIFEST_FILE,
};
pub use error::BootError;
pub use harness::{run_one_turn, Harness, HarnessBuilder, CLEAN_MARKER};
pub use harness_toml::{ApprovalMode, HarnessProfile, ProviderConfig, HARNESS_TOML};
