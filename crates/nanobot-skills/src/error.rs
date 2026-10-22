//! Error types for the skill runner.

use thiserror::Error;

/// Errors that can occur when enabling or disabling a skill.
#[derive(Debug, Error)]
pub enum SkillError {
    /// The skill manifest failed to parse.
    #[error("manifest parse error: {0}")]
    ManifestParse(String),

    /// The skill manifest failed validation.
    #[error("manifest validation error: {0}")]
    Validation(String),

    /// A skill with the same name is already enabled.
    #[error("skill `{0}` is already enabled")]
    AlreadyEnabled(String),

    /// The skill runner has no context to register listeners on.
    #[error("skill runner has no context")]
    NoContext,
}
