//! Three waterfalls: pre-execute, execute, post-execute (design-harness 10.5).
//!
//! The tool pipeline is:
//! 1. **pre-execute**: check tool is registered + validate parameters against
//!    JSON Schema + check approval requirement -> `Allow` / `Deny` / `Ask`
//! 2. **execute**: call the tool body (async closure provided by the caller,
//!    typically wrapping an `ocad-engine` dispatch)
//! 3. **post-execute**: lightweight verification hook -> `Accept` / `Replace` / `Block`
//!
//! The pipeline owns the dispatch plane (harness kernel side); `ocad-engine`
//! owns execution and transport (AgentSession direct / Worker bridge).

use crate::approval::ApprovalPolicy;
use crate::error::ToolError;
use crate::registry::{ToolDefinition, ToolRegistry};
use std::sync::Arc;

/// Result of the pre-execute waterfall.
#[derive(Debug, Clone, PartialEq)]
pub enum PreExecuteResult {
    /// Tool is registered, parameters valid, no approval needed.
    ///
    /// Carries the arguments to actually execute with: [`validate`] may have
    /// coerced a lax model output (a numeric string into a number, `"true"`
    /// into a boolean, a `null` into the schema's default). Ignoring it and
    /// using the raw arguments would throw the coercion away.
    Allow(serde_json::Value),
    /// Tool is denied (unknown tool, validation failure, or approval denied).
    ///
    /// A validation failure carries a JSON object string
    /// (`{"code":"invalid_input","keyword":...,"path":...,"message":...}`) so
    /// the refusal is structured for the product; the other refusals stay
    /// prose.
    Deny(String),
    /// Approval is required before execution.
    Ask(String),
}

/// Result of the post-execute waterfall.
#[derive(Debug, Clone, PartialEq)]
pub enum PostExecuteResult {
    /// Result is accepted as-is.
    Accept,
    /// Result should be replaced with a different value.
    Replace(String),
    /// Result should be blocked (execution was incorrect).
    Block(String),
}

/// The guarded tool pipeline.
///
/// `ToolPipeline` is `Send + Sync` (Arc + Mutex internals). Clone is cheap.
pub struct ToolPipeline {
    registry: Arc<ToolRegistry>,
    approval_policy: Arc<dyn ApprovalPolicy>,
}

impl ToolPipeline {
    /// Create a new pipeline with the given registry and approval policy.
    pub fn new(registry: Arc<ToolRegistry>, approval_policy: Arc<dyn ApprovalPolicy>) -> Self {
        Self {
            registry,
            approval_policy,
        }
    }

    /// Get a reference to the registry.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Pre-execute waterfall: check registration, validate + coerce
    /// parameters, and determine whether approval is needed.
    pub fn pre_execute(&self, name: &str, arguments: &serde_json::Value) -> PreExecuteResult {
        // 1. Check tool is registered.
        let def = match self.registry.get(name) {
            Some(def) => def,
            None => {
                return PreExecuteResult::Deny(format!(
                    "unknown tool: {name}. Available: {:?}",
                    self.registry.names()
                ))
            }
        };

        // 2. Coerce the arguments, and validate them when the tool opts in.
        //
        // Coercion is always on: it only rewrites a value toward the type the
        // schema declares, so a near-miss (a numeric string where a number was
        // declared) is fixed rather than failed. Deep validation is the
        // tool's own choice, because a schemars schema describes the
        // serialized shape, not the accepted input — see `validate_args`.
        let arguments = if def.validate_args {
            match crate::validate::validate(&def.parameters_schema, arguments) {
                Ok(arguments) => arguments,
                Err(invalid) => {
                    return PreExecuteResult::Deny(
                        serde_json::json!({
                            "code": "invalid_input",
                            "keyword": invalid.keyword,
                            "path": invalid.path,
                            "message": invalid.message,
                        })
                        .to_string(),
                    )
                }
            }
        } else {
            crate::validate::coerce(&def.parameters_schema, arguments)
        };

        // 3. Check approval requirement.
        if def.requires_approval {
            let approved = self
                .approval_policy
                .request_approval(&format!("tool:{name}"), &def.name);
            if !approved {
                return PreExecuteResult::Deny("approval denied".to_string());
            }
        }

        PreExecuteResult::Allow(arguments)
    }

    /// Execute waterfall: call the tool body.
    ///
    /// The `body` closure is provided by the caller (typically wrapping an
    /// `ocad-engine` dispatch). This keeps `ocad-tools` independent of
    /// `ocad-engine`.
    pub async fn execute<F, Fut>(
        &self,
        name: &str,
        arguments: serde_json::Value,
        body: F,
    ) -> Result<String, ToolError>
    where
        F: FnOnce(serde_json::Value) -> Fut,
        Fut: std::future::Future<Output = Result<String, ToolError>>,
    {
        // Verify tool is still registered.
        if !self.registry.contains(name) {
            return Err(ToolError::UnknownTool(
                name.to_string(),
                self.registry.names(),
            ));
        }

        body(arguments).await
    }

    /// Post-execute waterfall: lightweight verification hook.
    ///
    /// The default implementation accepts any non-empty result. Replace
    /// with domain-specific verification (e.g. topology signature recording).
    pub fn post_execute(&self, name: &str, result: &str) -> PostExecuteResult {
        if result.is_empty() {
            return PostExecuteResult::Block(format!("tool {name} returned empty output"));
        }
        PostExecuteResult::Accept
    }

    /// Get a tool definition by name.
    pub fn get_tool(&self, name: &str) -> Option<ToolDefinition> {
        self.registry.get(name)
    }

    /// List all registered tool definitions.
    pub fn list_tools(&self) -> Vec<ToolDefinition> {
        self.registry.list()
    }
}
