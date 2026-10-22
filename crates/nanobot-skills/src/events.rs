//! Event payload types for skill waterfalls (design-harness M3).
//!
//! These types are used as payloads in the `nanobot-cordis` event bus.
//! The skill runner registers waterfall and serial listeners that produce
//! or consume these payload types.

use crate::manifest::{FailureStrategy, SkillStep};

/// Payload for the `prompt/assemble` waterfall.
///
/// Each skill listener appends a persona section derived from the
/// manifest's `[skill]` intent and scene fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAssemble {
    /// The current system prompt text.
    pub system_prompt: String,
}

/// Payload for the `plan/assemble` waterfall.
///
/// Each skill listener injects step templates derived from the
/// manifest's `[skill.steps]` declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanAssemble {
    /// The current plan steps.
    pub steps: Vec<SkillStep>,
}

/// Payload for the `plan/drafting` serial event.
///
/// Each skill listener provides its required and optional input fields
/// as a clarification template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDrafting {
    /// Required input field names.
    pub required: Vec<String>,
    /// Optional input field names.
    pub optional: Vec<String>,
}

/// Payload for the `plan/contingent` serial event.
///
/// Each skill listener provides its failure strategy for contingent plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContingent {
    /// The failure strategy to apply.
    pub strategy: FailureStrategy,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::SkillStep;

    #[test]
    fn prompt_assemble_clone() {
        let p = PromptAssemble {
            system_prompt: "You are a CAD assistant.".into(),
        };
        let q = p.clone();
        assert_eq!(p, q);
    }

    #[test]
    fn plan_assemble_with_steps() {
        let steps = vec![SkillStep::Ask {
            ask: "material".into(),
        }];
        let p = PlanAssemble {
            steps: steps.clone(),
        };
        assert_eq!(p.steps.len(), 1);
    }

    #[test]
    fn plan_drafting_fields() {
        let p = PlanDrafting {
            required: vec!["material".into(), "thickness".into()],
            optional: vec!["finish".into()],
        };
        assert_eq!(p.required.len(), 2);
        assert_eq!(p.optional.len(), 1);
    }

    #[test]
    fn plan_contingent_strategy() {
        let p = PlanContingent {
            strategy: FailureStrategy::Ask,
        };
        assert_eq!(p.strategy, FailureStrategy::Ask);
    }
}
