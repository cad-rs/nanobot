//! Skill manifest types and TOML parsing (design-harness M3).
//!
//! A skill manifest is a `skill.toml` file that declares:
//! - Package identity (name, version, api_version)
//! - Permissions (tools, knowledge_query, file_io)
//! - Skill definition (intent, scene, inputs, steps, on_failure)
//! - Validation cases (optional)

use serde::{Deserialize, Serialize};

use crate::error::SkillError;

/// The top-level skill manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifest {
    pub package: SkillPackage,
    #[serde(default)]
    pub permissions: SkillPermissions,
    pub skill: SkillDefinition,
    #[serde(default)]
    pub validation: Option<SkillValidation>,
}

/// Package identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackage {
    pub name: String,
    pub version: String,
    pub api_version: u32,
}

/// Permission declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillPermissions {
    pub tools: Vec<String>,
    pub knowledge_query: bool,
    pub file_io: bool,
}

/// Skill definition: intent, scene, inputs, steps, and failure strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub intent: String,
    pub scene: String,
    #[serde(default)]
    pub inputs: SkillInputs,
    #[serde(default)]
    pub steps: Vec<SkillStep>,
    #[serde(default)]
    pub on_failure: FailureStrategy,
}

/// Input field declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillInputs {
    pub required: Vec<String>,
    pub optional: Vec<String>,
}

/// One step in the skill's execution plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillStep {
    /// Ask the user for information.
    Ask { ask: String },
    /// Invoke a tool.
    Tool {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        type_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params_ref: Option<String>,
    },
    /// Run a check.
    Check {
        check: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<serde_json::Value>,
    },
}

/// Failure strategy for the skill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStrategy {
    #[default]
    Ask,
    Abort,
    Rollback,
}

/// Validation case declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillValidation {
    pub cases: Vec<String>,
}

impl SkillManifest {
    /// Parse a skill manifest from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, SkillError> {
        let manifest: SkillManifest =
            toml::from_str(text).map_err(|e| SkillError::ManifestParse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the manifest's invariants.
    pub fn validate(&self) -> Result<(), SkillError> {
        if self.package.name.trim().is_empty() {
            return Err(SkillError::Validation("package name is empty".into()));
        }
        if self.package.version.trim().is_empty() {
            return Err(SkillError::Validation("package version is empty".into()));
        }
        if self.package.api_version == 0 {
            return Err(SkillError::Validation("api_version must be > 0".into()));
        }
        if self.skill.intent.trim().is_empty() {
            return Err(SkillError::Validation("skill intent is empty".into()));
        }
        if self.skill.scene.trim().is_empty() {
            return Err(SkillError::Validation("skill scene is empty".into()));
        }
        Ok(())
    }

    /// Get the skill name (from the package).
    pub fn name(&self) -> &str {
        &self.package.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MANIFEST: &str = r#"
[package]
name = "sheet-metal-bracket"
version = "0.2.1"
api_version = 1

[permissions]
tools = ["add_feature", "set_parameter", "what_if", "rebuild"]
knowledge_query = true
file_io = false

[skill]
intent = "Design a manufacturable sheet-metal mounting bracket"
scene = "brackets, mounts, fixtures"

[skill.inputs]
required = ["material", "thickness_mm", "mounting_points", "load_case"]
optional = ["surface_treatment"]

[[skill.steps]]
ask = "material, thickness"

[[skill.steps]]
tool = "add_feature"
type_id = "flange"
params_ref = "derived"

[[skill.steps]]
check = "rule_set:bend-rules"

[[skill.steps]]
check = "tool:rebuild"
expect = { manifold = true }

on_failure = "ask"

[validation]
cases = ["cases/m8-bracket.json", "cases/l-bracket.json"]
"#;

    #[test]
    fn parse_valid_manifest() {
        let manifest = SkillManifest::from_toml(VALID_MANIFEST).unwrap();
        assert_eq!(manifest.package.name, "sheet-metal-bracket");
        assert_eq!(manifest.package.version, "0.2.1");
        assert_eq!(manifest.package.api_version, 1);
        assert_eq!(manifest.permissions.tools.len(), 4);
        assert!(manifest.permissions.knowledge_query);
        assert!(!manifest.permissions.file_io);
        assert_eq!(manifest.skill.inputs.required.len(), 4);
        assert_eq!(manifest.skill.inputs.optional.len(), 1);
        assert_eq!(manifest.skill.steps.len(), 4);
        assert_eq!(manifest.skill.on_failure, FailureStrategy::Ask);
        assert!(manifest.validation.is_some());
    }

    #[test]
    fn parse_minimal_manifest() {
        let text = r#"
[package]
name = "test-skill"
version = "1.0.0"
api_version = 1

[skill]
intent = "Test intent"
scene = "test"
"#;
        let manifest = SkillManifest::from_toml(text).unwrap();
        assert_eq!(manifest.package.name, "test-skill");
        assert!(manifest.permissions.tools.is_empty());
        assert!(!manifest.permissions.knowledge_query);
        assert!(manifest.skill.inputs.required.is_empty());
        assert!(manifest.skill.steps.is_empty());
        assert_eq!(manifest.skill.on_failure, FailureStrategy::Ask);
        assert!(manifest.validation.is_none());
    }

    #[test]
    fn reject_empty_name() {
        let text = r#"
[package]
name = ""
version = "1.0.0"
api_version = 1

[skill]
intent = "Test"
scene = "test"
"#;
        assert!(SkillManifest::from_toml(text).is_err());
    }

    #[test]
    fn reject_zero_api_version() {
        let text = r#"
[package]
name = "test"
version = "1.0.0"
api_version = 0

[skill]
intent = "Test"
scene = "test"
"#;
        assert!(SkillManifest::from_toml(text).is_err());
    }

    #[test]
    fn step_ask_variant() {
        let step = SkillStep::Ask {
            ask: "material".into(),
        };
        match step {
            SkillStep::Ask { ask } => assert_eq!(ask, "material"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn step_tool_variant() {
        let step = SkillStep::Tool {
            tool: "add_feature".into(),
            type_id: Some("flange".into()),
            params_ref: Some("derived".into()),
        };
        match step {
            SkillStep::Tool {
                tool,
                type_id,
                params_ref,
            } => {
                assert_eq!(tool, "add_feature");
                assert_eq!(type_id.as_deref(), Some("flange"));
                assert_eq!(params_ref.as_deref(), Some("derived"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn step_check_variant() {
        let step = SkillStep::Check {
            check: "rule_set:bend-rules".into(),
            expect: None,
        };
        match step {
            SkillStep::Check { check, expect } => {
                assert_eq!(check, "rule_set:bend-rules");
                assert!(expect.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn failure_strategy_abort() {
        let text = r#"
[package]
name = "test"
version = "1.0.0"
api_version = 1

[skill]
intent = "Test"
scene = "test"
on_failure = "abort"
"#;
        let manifest = SkillManifest::from_toml(text).unwrap();
        assert_eq!(manifest.skill.on_failure, FailureStrategy::Abort);
    }

    #[test]
    fn failure_strategy_rollback() {
        let text = r#"
[package]
name = "test"
version = "1.0.0"
api_version = 1

[skill]
intent = "Test"
scene = "test"
on_failure = "rollback"
"#;
        let manifest = SkillManifest::from_toml(text).unwrap();
        assert_eq!(manifest.skill.on_failure, FailureStrategy::Rollback);
    }
}
