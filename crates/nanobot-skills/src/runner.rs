//! Skill runner: compiles manifests into listener combinations (design-harness M3).
//!
//! Each enabled skill is a `Fiber`. The runner registers waterfall and serial
//! listeners on the `Context` event bus as fiber effects. Disposing the fiber
//! removes all listeners in reverse order (LIFO), which implements the
//! "disable = uninstall" semantics.

use nanobot_cordis::{Context, Fiber};
use std::sync::{Arc, Mutex};

use crate::error::SkillError;
use crate::events::{PlanAssemble, PlanContingent, PlanDrafting, PromptAssemble};
use crate::manifest::SkillManifest;

/// The skill runner: manages skill fibers on a context.
pub struct SkillRunner {
    context: Context,
    fibers: Mutex<Vec<Arc<Fiber>>>,
}

impl SkillRunner {
    /// Create a new skill runner on the given context.
    pub fn new(context: Context) -> Self {
        Self {
            context,
            fibers: Mutex::new(Vec::new()),
        }
    }

    /// Enable a skill: create a fiber, register listeners, activate the fiber.
    ///
    /// The following listeners are registered on the context event bus:
    /// - `prompt/assemble` waterfall: injects persona section from intent/scene
    /// - `plan/assemble` waterfall: injects step templates from `[skill.steps]`
    /// - `plan/drafting` serial: provides input field templates
    /// - `plan/contingent` serial: provides failure strategy
    ///
    /// All listeners are recorded as fiber effects, so disposing the fiber
    /// removes them in reverse order.
    pub fn enable(&self, manifest: &SkillManifest) -> Result<Arc<Fiber>, SkillError> {
        let name = manifest.name().to_string();

        // Check for duplicate.
        {
            let fibers = self.fibers.lock().unwrap();
            if fibers.iter().any(|f| f.name() == name) {
                return Err(SkillError::AlreadyEnabled(name));
            }
        }

        let fiber = Arc::new(Fiber::new(&name));

        // Register prompt/assemble waterfall listener.
        let intent = manifest.skill.intent.clone();
        let scene = manifest.skill.scene.clone();
        let d = self
            .context
            .events()
            .on_waterfall::<PromptAssemble, PromptAssemble>(
                "prompt/assemble",
                move |payload, next| {
                    let mut result = next(payload);
                    if !result.system_prompt.is_empty() {
                        result.system_prompt.push('\n');
                    }
                    result
                        .system_prompt
                        .push_str(&format!("\n[Skill: {intent}]\nScene: {scene}"));
                    result
                },
            );
        fiber.add_effect(d);

        // Register plan/assemble waterfall listener.
        let steps = manifest.skill.steps.clone();
        if !steps.is_empty() {
            let d = self
                .context
                .events()
                .on_waterfall::<PlanAssemble, PlanAssemble>(
                    "plan/assemble",
                    move |payload, next| {
                        let mut result = next(payload);
                        for step in &steps {
                            if !result.steps.contains(step) {
                                result.steps.push(step.clone());
                            }
                        }
                        result
                    },
                );
            fiber.add_effect(d);
        }

        // Register plan/drafting serial listener.
        let required = manifest.skill.inputs.required.clone();
        let optional = manifest.skill.inputs.optional.clone();
        if !required.is_empty() || !optional.is_empty() {
            let d =
                self.context
                    .events()
                    .on_serial::<(), PlanDrafting>("plan/drafting", move |_| {
                        Some(PlanDrafting {
                            required: required.clone(),
                            optional: optional.clone(),
                        })
                    });
            fiber.add_effect(d);
        }

        // Register plan/contingent serial listener.
        let strategy = manifest.skill.on_failure.clone();
        let d =
            self.context
                .events()
                .on_serial::<(), PlanContingent>("plan/contingent", move |_| {
                    Some(PlanContingent {
                        strategy: strategy.clone(),
                    })
                });
        fiber.add_effect(d);

        fiber.set_active();
        self.fibers.lock().unwrap().push(fiber.clone());
        Ok(fiber)
    }

    /// Disable a skill by name: dispose its fiber, removing all listeners.
    ///
    /// Returns `true` if the skill was found and disabled.
    pub fn disable(&self, name: &str) -> bool {
        let mut fibers = self.fibers.lock().unwrap();
        let pos = fibers.iter().position(|f| f.name() == name);
        match pos {
            Some(idx) => {
                let fiber = fibers.remove(idx);
                fiber.dispose();
                true
            }
            None => false,
        }
    }

    /// Disable all skills: dispose all fibers in reverse order.
    pub fn disable_all(&self) {
        let mut fibers = self.fibers.lock().unwrap();
        for fiber in fibers.drain(..).rev() {
            fiber.dispose();
        }
    }

    /// Get the names of all enabled skills.
    pub fn enabled(&self) -> Vec<String> {
        self.fibers
            .lock()
            .unwrap()
            .iter()
            .map(|f| f.name().to_string())
            .collect()
    }

    /// Get the number of enabled skills.
    pub fn len(&self) -> usize {
        self.fibers.lock().unwrap().len()
    }

    /// Check if no skills are enabled.
    pub fn is_empty(&self) -> bool {
        self.fibers.lock().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        FailureStrategy, SkillDefinition, SkillInputs, SkillManifest, SkillPackage, SkillStep,
    };

    fn make_manifest(name: &str) -> SkillManifest {
        SkillManifest {
            package: SkillPackage {
                name: name.into(),
                version: "1.0.0".into(),
                api_version: 1,
            },
            permissions: Default::default(),
            skill: SkillDefinition {
                intent: format!("Intent for {name}"),
                scene: "test".into(),
                inputs: SkillInputs::default(),
                steps: vec![],
                on_failure: FailureStrategy::Ask,
            },
            validation: None,
        }
    }

    fn make_manifest_with_steps(name: &str) -> SkillManifest {
        SkillManifest {
            package: SkillPackage {
                name: name.into(),
                version: "1.0.0".into(),
                api_version: 1,
            },
            permissions: Default::default(),
            skill: SkillDefinition {
                intent: format!("Intent for {name}"),
                scene: "test".into(),
                inputs: SkillInputs {
                    required: vec!["material".into()],
                    optional: vec!["finish".into()],
                },
                steps: vec![SkillStep::Ask {
                    ask: "material".into(),
                }],
                on_failure: FailureStrategy::Ask,
            },
            validation: None,
        }
    }

    #[test]
    fn enable_and_disable() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx);

        assert!(runner.is_empty());

        let fiber = runner.enable(&make_manifest("test-skill")).unwrap();
        assert_eq!(fiber.state(), nanobot_cordis::FiberState::Active);
        assert_eq!(runner.len(), 1);
        assert_eq!(runner.enabled(), vec!["test-skill"]);

        assert!(runner.disable("test-skill"));
        assert!(runner.is_empty());
        assert!(!runner.disable("test-skill"));
    }

    #[test]
    fn enable_duplicate_rejected() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx);

        runner.enable(&make_manifest("dup")).unwrap();
        assert!(runner.enable(&make_manifest("dup")).is_err());
    }

    #[test]
    fn disable_all() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx);

        runner.enable(&make_manifest("a")).unwrap();
        runner.enable(&make_manifest("b")).unwrap();
        runner.enable(&make_manifest("c")).unwrap();
        assert_eq!(runner.len(), 3);

        runner.disable_all();
        assert!(runner.is_empty());
    }

    #[test]
    fn prompt_assemble_waterfall() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner.enable(&make_manifest("persona-skill")).unwrap();

        let result = ctx.events().waterfall::<PromptAssemble, PromptAssemble>(
            "prompt/assemble",
            PromptAssemble {
                system_prompt: "Base prompt.".into(),
            },
        );

        assert!(result.is_some());
        let assembled = result.unwrap();
        assert!(assembled.system_prompt.contains("Base prompt."));
        assert!(assembled
            .system_prompt
            .contains("[Skill: Intent for persona-skill]"));
        assert!(assembled.system_prompt.contains("Scene: test"));
    }

    #[test]
    fn prompt_assemble_removed_after_disable() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner.enable(&make_manifest("temp")).unwrap();
        runner.disable("temp");

        let result = ctx.events().waterfall::<PromptAssemble, PromptAssemble>(
            "prompt/assemble",
            PromptAssemble {
                system_prompt: "Base.".into(),
            },
        );

        // No listeners left, so waterfall returns None.
        assert!(result.is_none());
    }

    #[test]
    fn plan_assemble_waterfall() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner
            .enable(&make_manifest_with_steps("step-skill"))
            .unwrap();

        let result = ctx.events().waterfall::<PlanAssemble, PlanAssemble>(
            "plan/assemble",
            PlanAssemble { steps: vec![] },
        );

        assert!(result.is_some());
        let assembled = result.unwrap();
        assert_eq!(assembled.steps.len(), 1);
        assert_eq!(
            assembled.steps[0],
            SkillStep::Ask {
                ask: "material".into()
            }
        );
    }

    #[test]
    fn plan_drafting_serial() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner
            .enable(&make_manifest_with_steps("input-skill"))
            .unwrap();

        let result = ctx.events().serial::<(), PlanDrafting>("plan/drafting", ());

        assert!(result.is_some());
        let drafting = result.unwrap();
        assert_eq!(drafting.required, vec!["material"]);
        assert_eq!(drafting.optional, vec!["finish"]);
    }

    #[test]
    fn plan_contingent_serial() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner.enable(&make_manifest("failure-skill")).unwrap();

        let result = ctx
            .events()
            .serial::<(), PlanContingent>("plan/contingent", ());

        assert!(result.is_some());
        let contingent = result.unwrap();
        assert_eq!(contingent.strategy, FailureStrategy::Ask);
    }

    #[test]
    fn multiple_skills_compose() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner.enable(&make_manifest("skill-a")).unwrap();
        runner.enable(&make_manifest_with_steps("skill-b")).unwrap();

        // Both persona sections should appear.
        let result = ctx.events().waterfall::<PromptAssemble, PromptAssemble>(
            "prompt/assemble",
            PromptAssemble {
                system_prompt: String::new(),
            },
        );
        let assembled = result.unwrap();
        assert!(assembled.system_prompt.contains("skill-a"));
        assert!(assembled.system_prompt.contains("skill-b"));

        // Steps from skill-b should appear.
        let result = ctx.events().waterfall::<PlanAssemble, PlanAssemble>(
            "plan/assemble",
            PlanAssemble { steps: vec![] },
        );
        let plan = result.unwrap();
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn disable_removes_only_target_skill() {
        let ctx = Context::root();
        let runner = SkillRunner::new(ctx.clone());

        runner.enable(&make_manifest("keep")).unwrap();
        runner.enable(&make_manifest("remove")).unwrap();

        runner.disable("remove");

        // "keep" should still inject.
        let result = ctx.events().waterfall::<PromptAssemble, PromptAssemble>(
            "prompt/assemble",
            PromptAssemble {
                system_prompt: String::new(),
            },
        );
        let assembled = result.unwrap();
        assert!(assembled.system_prompt.contains("keep"));
        assert!(!assembled.system_prompt.contains("remove"));
    }
}
