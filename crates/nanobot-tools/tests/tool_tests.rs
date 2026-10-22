//! Integration tests for ocad-tools: ToolRegistry, ToolPipeline, ApprovalPolicy.

use ocad_tools::{
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
        contract: ocad_tools::contract_for(name).unwrap_or(ToolContract::Mutates),
    }
}

#[test]
fn registry_rejects_duplicates() {
    let reg = ToolRegistry::new();
    reg.register(make_tool("foo", false)).unwrap();
    assert!(reg.register(make_tool("foo", false)).is_err());
}

#[test]
fn pipeline_pre_execute_allows_registered() {
    let reg = Arc::new(ToolRegistry::new());
    reg.register(make_tool("foo", false)).unwrap();
    let pipeline = ToolPipeline::new(reg, Arc::new(AutoApprovalPolicy));
    let result = pipeline.pre_execute("foo", &serde_json::json!({}));
    assert_eq!(result, PreExecuteResult::Allow);
}

#[test]
fn pipeline_pre_execute_denies_unknown() {
    let reg = Arc::new(ToolRegistry::new());
    let pipeline = ToolPipeline::new(reg, Arc::new(AutoApprovalPolicy));
    let result = pipeline.pre_execute("nope", &serde_json::json!({}));
    assert!(matches!(result, PreExecuteResult::Deny(_)));
}

#[test]
fn pipeline_approval_policy_deny() {
    let reg = Arc::new(ToolRegistry::new());
    reg.register(make_tool("dangerous", true)).unwrap();
    let pipeline = ToolPipeline::new(reg, Arc::new(DenyAllPolicy));
    let result = pipeline.pre_execute("dangerous", &serde_json::json!({}));
    assert!(matches!(result, PreExecuteResult::Deny(_)));
}

#[test]
fn pipeline_execute_calls_body() {
    let reg = Arc::new(ToolRegistry::new());
    reg.register(make_tool("echo", false)).unwrap();
    let pipeline = ToolPipeline::new(reg, Arc::new(AutoApprovalPolicy));
    let result = futures_executor::block_on(pipeline.execute(
        "echo",
        serde_json::json!({"msg": "hi"}),
        |_args| async { Ok("echo result".to_string()) },
    ));
    assert_eq!(result.unwrap(), "echo result");
}
