//! nanobot-skills: the skill runner.
//!
//! A skill is a declarative manifest compiled into listener combinations on
//! the [`nanobot_cordis::Context`] event bus. Each skill is a `Fiber`:
//! disabling a skill disposes its fiber, which removes the listeners it
//! registered in reverse order (LIFO).
//!
//! | Manifest element | Compiled product |
//! |---|---|
//! | `[skill]` intent/scene | `prompt/assemble` waterfall listener |
//! | `[skill.inputs]` gaps | `plan/drafting` serial listener |
//! | `[skill.steps]` | `plan/assemble` waterfall listener |
//! | `[skill.on_failure]` | `plan/contingent` serial listener |
//!
//! The skill *packages* themselves are product content: a harness ships its
//! own manifests and enables them through [`SkillRunner::enable`]. This crate
//! owns the mechanism, not the skills.
//!
//! Provenance: extracted from the ocad design harness (see
//! `docs/nanobot-architecture.md` in rcad-pro).
//!
//! No tokio runtime entry; the async runtime is provided by the host.

pub mod error;
pub mod events;
pub mod manifest;
pub mod runner;

pub use error::SkillError;
pub use events::{PlanAssemble, PlanContingent, PlanDrafting, PromptAssemble};
pub use manifest::{
    FailureStrategy, SkillDefinition, SkillInputs, SkillManifest, SkillPackage, SkillPermissions,
    SkillStep, SkillValidation,
};
pub use runner::SkillRunner;
