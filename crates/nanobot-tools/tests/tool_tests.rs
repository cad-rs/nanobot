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
        validate_args: false,
    }
}

fn make_tool_with_schema(name: &str, schema: serde_json::Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: format!("Test tool {name}"),
        parameters_schema: schema,
        requires_approval: false,
        contract: ToolContract::ReadOnly,
        validate_args: false,
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

    // AutoApprovalPolicy approves, so the gated tool is allowed through. The
    // pipeline hands back the arguments to run with, so `Allow` carries them.
    assert_eq!(
        pipeline.pre_execute("t", &serde_json::json!({"a": 1})),
        PreExecuteResult::Allow(serde_json::json!({"a": 1}))
    );
    assert!(matches!(
        pipeline.pre_execute("missing", &serde_json::json!({})),
        PreExecuteResult::Deny(_)
    ));
}

#[test]
fn pre_execute_validates_the_declared_schema() {
    let registry = Arc::new(ToolRegistry::new());
    let mut measure = make_tool_with_schema(
        "measure",
        serde_json::json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer"},
                "unit": {"type": "string"}
            },
            "required": ["count"]
        }),
    );
    measure.validate_args = true;
    registry.register(measure).unwrap();
    let pipeline = ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy));

    // A missing required property is refused.
    let denied = pipeline.pre_execute("measure", &serde_json::json!({"unit": "um"}));
    let PreExecuteResult::Deny(reason) = denied else {
        panic!("a missing required property must be denied")
    };
    let parsed: serde_json::Value = serde_json::from_str(&reason).expect("the refusal is structured");
    assert_eq!(parsed["code"], "invalid_input");
    assert_eq!(parsed["keyword"], "required");

    // A wrong type is refused, and the path names the offending property.
    let PreExecuteResult::Deny(reason) =
        pipeline.pre_execute("measure", &serde_json::json!({"count": true}))
    else {
        panic!("a wrong type must be denied")
    };
    let parsed: serde_json::Value = serde_json::from_str(&reason).expect("the refusal is structured");
    assert_eq!(parsed["path"], "$.count");
    assert_eq!(parsed["keyword"], "type");
}

#[test]
fn pre_execute_coerces_the_lax_output_models_emit() {
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register(make_tool_with_schema(
            "measure",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "count": {"type": "integer"},
                    "flag": {"type": "boolean"},
                    "width": {"type": "number", "default": 3.0}
                },
                "required": ["count"]
            }),
        ))
        .unwrap();
    let pipeline = ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy));

    let PreExecuteResult::Allow(coerced) = pipeline.pre_execute(
        "measure",
        &serde_json::json!({"count": "17", "flag": "true", "width": null}),
    ) else {
        panic!("a near-miss must be coerced, not refused")
    };

    assert_eq!(coerced["count"], 17, "a numeric string becomes an integer");
    assert_eq!(coerced["flag"], true, "a boolean string becomes a boolean");
    assert_eq!(coerced["width"], 3.0, "a null takes the declared default");
}

#[test]
fn deep_validation_is_opt_in_per_tool() {
    let registry = Arc::new(ToolRegistry::new());
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"a": {"type": "integer"}},
        "additionalProperties": false
    });
    let mut strict = make_tool_with_schema("strict", schema.clone());
    strict.validate_args = true;
    let mut lenient = make_tool_with_schema("lenient", schema);
    lenient.validate_args = false;
    registry.register(strict).unwrap();
    registry.register(lenient).unwrap();
    let pipeline = ToolPipeline::new(registry, Arc::new(AutoApprovalPolicy));

    let extra = serde_json::json!({"a": 1, "b": 2});
    // The strict tool refuses the extra key; the lenient one coerces and lets
    // the product decide.
    assert!(matches!(
        pipeline.pre_execute("strict", &extra),
        PreExecuteResult::Deny(_)
    ));
    assert!(matches!(
        pipeline.pre_execute("lenient", &extra),
        PreExecuteResult::Allow(_)
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
