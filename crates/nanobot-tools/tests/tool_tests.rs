//! Integration tests for nanobot-tools: ToolRegistry, ToolPipeline,
//! ApprovalPolicy.

use nanobot_tools::{
    AutoApprovalPolicy, DenyAllPolicy, PreExecuteResult, ToolContract, ToolDefinition,
    ToolPipeline, ToolRegistry,
};
use std::sync::Arc;

fn make_tool(name: &str, requires_approval: bool) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: format!("Test tool {name}"),
        parameters_schema: serde_json::json!({"type": "object"}),
        requires_approval,
        contract: ToolContract::ReadOnly,
    }
}

#[test]
fn registry_rejects_duplicates() {
    let registry = ToolRegistry::new();
    registry.register(make_tool("t", false)).unwrap();
    assert!(registry.register(make_tool("t", false)).is_err());
    assert!(registry.contains("t"));
    assert!(registry.names().contains(&"t".to_string()));
}

#[test]
fn pre_execute_allow_deny_and_ask() {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(make_tool("t", true)).unwrap();
    let pipeline = ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy));

    // AutoApprovalPolicy approves, so the gated tool is allowed through.
    assert!(matches!(
        pipeline.pre_execute("t", &serde_json::json!({})),
        PreExecuteResult::Allow
    ));
    assert!(matches!(
        pipeline.pre_execute("missing", &serde_json::json!({})),
        PreExecuteResult::Deny(_)
    ));
}

#[test]
fn execute_runs_the_caller_supplied_body() {
    let registry = Arc::new(ToolRegistry::new());
    registry.register(make_tool("t", false)).unwrap();
    let pipeline = ToolPipeline::new(registry, Arc::new(DenyAllPolicy));

    let ran = futures_executor::block_on(pipeline.execute(
        "t",
        serde_json::json!({}),
        |_args| async { Ok("ran".to_string()) },
    ))
    .unwrap();
    assert_eq!(ran, "ran");
}
