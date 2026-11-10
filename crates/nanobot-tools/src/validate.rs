//! Tool-argument coercion and validation against a JSON Schema.
//!
//! The pipeline runs **coercion** over a tool call's arguments before anything
//! reaches the executor, and — when the tool opts in — **validation**.
//!
//! ## Coercion (always on)
//!
//! Models emit lax JSON: a numeric field arrives as `"17"`, a boolean as
//! `"true"`, an omitted optional field as `null`. Coercion rewrites those
//! toward the type the schema declares, so a near-miss is fixed rather than
//! failed. It never refuses, and it never rewrites a value the schema would
//! have accepted anyway — a schema that declares `["string", "number"]` keeps
//! the string, and an `Option<T>` field keeps an explicit `null` (that is a
//! legitimate `None`, not a missing value).
//!
//! ## Validation (opt-in per tool)
//!
//! Deep validation is **not** the default, and the reason is a property of
//! `schemars`: the schema it generates describes the type's **serialized**
//! shape, not the shape its deserializer **accepts**. A type with a custom
//! `Deserialize` (this repo's `SfqEndpoint` takes a bare string, a legacy
//! `[instance, hook]` array, or an object) is lossy in its schema, so a strict
//! check refuses input the product accepts.
//!
//! A product that knows its schema is faithful opts in with
//! [`ToolDefinition::validate_args`](crate::ToolDefinition::validate_args).
//! The product's own `serde_json::from_value` is the authoritative check
//! either way; this is a gate in front of it, not a replacement.
//!
//! ### Covered keywords
//!
//! `type` (string or array), `required`, `properties`, `additionalProperties`
//! (only when explicitly `false`), `items`, `minItems`/`maxItems`,
//! `minProperties`/`maxProperties`, `minLength`/`maxLength`, `minimum`/
//! `maximum`/`exclusiveMinimum`/`exclusiveMaximum`, `enum`, `const`, `anyOf`,
//! `oneOf`, `allOf`, `not`, `$ref` (resolved against the root document), and
//! `default` (substituted for a `null` that is not an allowed type).
//!
//! ### Deliberately not covered
//!
//! `pattern` (a regex engine would add a dependency the kernel does not
//! carry), `format`, `uniqueItems`, `propertyNames`, `if`/`then`/`else`,
//! `dependencies`, `contains`. A schema using them still validates — those
//! keywords are simply not enforced.

use serde_json::{Map, Value};

/// One validation failure: which keyword failed, where, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidInput {
    /// The keyword that failed (`type`, `required`, ...).
    pub keyword: String,
    /// Where it failed, as a JSON path (`$.width`, `$.items[0].x`).
    pub path: String,
    /// What was wrong.
    pub message: String,
}

impl InvalidInput {
    fn new(keyword: impl Into<String>, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            keyword: keyword.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Coerce the arguments toward the schema's declared types. Never refuses.
///
/// This is the always-on half of the pipeline's argument handling.
pub fn coerce(schema: &Value, args: &Value) -> Value {
    let mut node = args.clone();
    // Coercion-only: the walk's checks are off, so it cannot fail.
    let _ = walk(schema, schema, "$", &mut node, false);
    node
}

/// Coerce and then validate the arguments against the schema.
///
/// Returns the arguments to execute with, or the first failure. A failure
/// means the call was refused: nothing ran.
pub fn validate(schema: &Value, args: &Value) -> Result<Value, InvalidInput> {
    let mut node = args.clone();
    walk(schema, schema, "$", &mut node, true)?;
    Ok(node)
}

/// Resolve a `$ref` pointer (`#/definitions/Foo`) against the root document.
fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix("#/")?;
    let mut current = root;
    for raw in pointer.split('/') {
        let token = raw.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&token)?,
            Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// The type names a schema accepts, if it declares any.
///
/// A union without a top-level `type` — schemars' `Option<T>` shape, an
/// `anyOf` of the field type and `null` — derives its types from the branches,
/// which is what lets coercion see that `null` is legitimate there.
fn expected_types(schema: &Map<String, Value>) -> Option<Vec<String>> {
    match schema.get("type") {
        Some(Value::String(one)) => return Some(vec![one.clone()]),
        Some(Value::Array(many)) => {
            return Some(
                many.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect(),
            )
        }
        _ => {}
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(branches)) = schema.get(key) {
            let mut types = Vec::new();
            for branch in branches {
                match branch.get("type") {
                    Some(Value::String(one)) => types.push(one.clone()),
                    Some(Value::Array(many)) => {
                        types.extend(many.iter().filter_map(|t| t.as_str().map(str::to_string)))
                    }
                    _ => {}
                }
            }
            if !types.is_empty() {
                return Some(types);
            }
        }
    }
    None
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => match value {
            Value::Number(n) => {
                n.as_i64().is_some() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
            }
            _ => false,
        },
        // An unknown type name is not a reason to refuse: the schema may use a
        // vocabulary this build does not know.
        _ => true,
    }
}

/// Rewrite a lax value toward the declared type. Only rewrites when the value's
/// own type is not one the schema accepts.
fn coerce_value(schema: &Map<String, Value>, value: &Value) -> Value {
    let Value::String(text) = value else {
        return value.clone();
    };
    let Some(types) = expected_types(schema) else {
        return value.clone();
    };
    // The schema accepts a string here (a union like `["string", "number"]`):
    // the value is already valid, so leave it alone.
    if types.iter().any(|t| t == "string") {
        return value.clone();
    }

    if types.iter().any(|t| t == "number" || t == "integer") {
        if let Ok(parsed) = text.parse::<f64>() {
            // Keep whole numbers as integers; a fractional value stays a float.
            if parsed.fract() == 0.0 && parsed >= i64::MIN as f64 && parsed <= i64::MAX as f64 {
                return Value::from(parsed as i64);
            }
            return serde_json::Number::from_f64(parsed)
                .map(Value::Number)
                .unwrap_or_else(|| value.clone());
        }
    }

    if types.iter().any(|t| t == "boolean") {
        match text.as_str() {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            _ => {}
        }
    }

    value.clone()
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn check_bounds(
    schema: &Map<String, Value>,
    lo_key: &str,
    hi_key: &str,
    actual: usize,
    path: &str,
    unit: &str,
) -> Result<(), InvalidInput> {
    if let Some(min) = schema.get(lo_key).and_then(Value::as_u64) {
        if (actual as u64) < min {
            return Err(InvalidInput::new(
                lo_key,
                path,
                format!("{actual} is below the minimum of {min} {unit}"),
            ));
        }
    }
    if let Some(max) = schema.get(hi_key).and_then(Value::as_u64) {
        if (actual as u64) > max {
            return Err(InvalidInput::new(
                hi_key,
                path,
                format!("{actual} is above the maximum of {max} {unit}"),
            ));
        }
    }
    Ok(())
}

/// Walk one node in place. `deep` gates the *checks*; the recursion into
/// declared children always runs, so coercion reaches nested values.
fn walk(
    root: &Value,
    schema: &Value,
    path: &str,
    node: &mut Value,
    deep: bool,
) -> Result<(), InvalidInput> {
    // `$ref` points into the root document (schemars emits `#/definitions/...`).
    let mut schema = schema;
    if let Some(Value::String(reference)) = schema.get("$ref") {
        if let Some(target) = resolve_ref(root, reference) {
            schema = target;
        }
    }
    let Some(s) = schema.as_object() else {
        return Ok(());
    };

    // A `null` takes the schema's default — but only when `null` is not an
    // allowed type. For an `Option<T>` field the schema declares
    // `["integer","null"]`, and an explicit null is a legitimate `None`.
    if node.is_null() {
        let null_allowed = expected_types(s)
            .map(|types| types.iter().any(|t| t == "null"))
            .unwrap_or(true);
        if !null_allowed {
            if let Some(default) = s.get("default") {
                if !default.is_null() {
                    *node = default.clone();
                }
            }
        }
    }

    *node = coerce_value(s, node);

    if deep {
        if let Some(types) = expected_types(s) {
            if !types.iter().any(|t| type_matches(t, node)) {
                return Err(InvalidInput::new(
                    "type",
                    path,
                    format!("expected {}, got {}", types.join(" or "), describe(node)),
                ));
            }
        }

        if let Some(Value::Array(options)) = s.get("enum") {
            if !options.iter().any(|option| option == node) {
                return Err(InvalidInput::new(
                    "enum",
                    path,
                    format!("must be one of {}", render_list(options)),
                ));
            }
        }
        if let Some(expected) = s.get("const") {
            if expected != node {
                return Err(InvalidInput::new("const", path, format!("must equal {expected}")));
            }
        }

        if let Some(Value::Array(branches)) = s.get("anyOf") {
            if !branches.iter().any(|branch| shallow_ok(root, branch, path, node)) {
                return Err(InvalidInput::new(
                    "anyOf",
                    path,
                    "matched none of the allowed shapes",
                ));
            }
        }
        if let Some(Value::Array(branches)) = s.get("oneOf") {
            let matched = branches
                .iter()
                .filter(|branch| shallow_ok(root, branch, path, node))
                .count();
            if matched != 1 {
                return Err(InvalidInput::new(
                    "oneOf",
                    path,
                    format!("matched {matched} branches, exactly 1 required"),
                ));
            }
        }
        if let Some(Value::Array(branches)) = s.get("allOf") {
            for branch in branches {
                if !shallow_ok(root, branch, path, node) {
                    return Err(InvalidInput::new("allOf", path, "failed a required branch"));
                }
            }
        }
        if let Some(not_schema) = s.get("not") {
            if shallow_ok(root, not_schema, path, node) {
                return Err(InvalidInput::new(
                    "not",
                    path,
                    "matched a shape it must not match",
                ));
            }
        }
    }

    match node {
        Value::Object(map) => {
            if deep {
                if let Some(Value::Array(required)) = s.get("required") {
                    for name in required {
                        if let Some(name) = name.as_str() {
                            if !map.contains_key(name) {
                                return Err(InvalidInput::new(
                                    "required",
                                    path,
                                    format!("missing required property `{name}`"),
                                ));
                            }
                        }
                    }
                }
            }

            let properties = s.get("properties").and_then(Value::as_object);
            let reject_unknown = s.get("additionalProperties") == Some(&Value::Bool(false));

            if let Some(properties) = properties {
                // Snapshot the keys: the loop inserts back into the same map.
                let keys: Vec<String> = map.keys().cloned().collect();
                for key in keys {
                    let child_path = format!("{path}.{key}");
                    match properties.get(&key) {
                        Some(child_schema) => {
                            let mut child = map.get(&key).cloned().unwrap_or(Value::Null);
                            walk(root, child_schema, &child_path, &mut child, deep)?;
                            map.insert(key, child);
                        }
                        None if reject_unknown => {
                            return Err(InvalidInput::new(
                                "additionalProperties",
                                &child_path,
                                format!("unexpected property `{key}`"),
                            ));
                        }
                        None => {}
                    }
                }
            } else if deep && reject_unknown && !map.is_empty() {
                return Err(InvalidInput::new(
                    "additionalProperties",
                    path,
                    "no properties are declared, but some were given",
                ));
            }

            if deep {
                check_bounds(s, "minProperties", "maxProperties", map.len(), path, "propertie")?;
            }
            Ok(())
        }
        Value::Array(items) => {
            if deep {
                check_bounds(s, "minItems", "maxItems", items.len(), path, "item")?;
            }
            if let Some(item_schema) = s.get("items") {
                for (index, item) in items.iter_mut().enumerate() {
                    walk(root, item_schema, &format!("{path}[{index}]"), item, deep)?;
                }
            }
            Ok(())
        }
        Value::String(text) => {
            if deep {
                check_bounds(
                    s,
                    "minLength",
                    "maxLength",
                    text.chars().count(),
                    path,
                    "character",
                )?;
            }
            Ok(())
        }
        Value::Number(n) => {
            if deep {
                let value = n.as_f64().unwrap_or(0.0);
                if let Some(min) = s.get("minimum").and_then(Value::as_f64) {
                    if value < min {
                        return Err(InvalidInput::new(
                            "minimum",
                            path,
                            format!("{value} is below the minimum of {min}"),
                        ));
                    }
                }
                if let Some(max) = s.get("maximum").and_then(Value::as_f64) {
                    if value > max {
                        return Err(InvalidInput::new(
                            "maximum",
                            path,
                            format!("{value} is above the maximum of {max}"),
                        ));
                    }
                }
                if let Some(min) = s.get("exclusiveMinimum").and_then(Value::as_f64) {
                    if value <= min {
                        return Err(InvalidInput::new(
                            "exclusiveMinimum",
                            path,
                            format!("{value} must be greater than {min}"),
                        ));
                    }
                }
                if let Some(max) = s.get("exclusiveMaximum").and_then(Value::as_f64) {
                    if value >= max {
                        return Err(InvalidInput::new(
                            "exclusiveMaximum",
                            path,
                            format!("{value} must be less than {max}"),
                        ));
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Does `node` satisfy `schema`?
///
/// The combinators use this so that testing a branch cannot rewrite the value
/// the *other* branches are judged against: the probe is a clone, and `node`
/// itself is never touched. (The clone is coerced, which is what lets a
/// near-miss match a branch — the same permissiveness the top-level walk has.)
fn shallow_ok(root: &Value, schema: &Value, path: &str, node: &Value) -> bool {
    let mut probe = node.clone();
    walk(root, schema, path, &mut probe, true).is_ok()
}

fn render_list(options: &[Value]) -> String {
    let rendered: Vec<String> = options.iter().map(|o| o.to_string()).collect();
    rendered.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(schema: serde_json::Value, args: serde_json::Value) {
        assert!(
            validate(&schema, &args).is_ok(),
            "expected {args} to validate against {schema}"
        );
    }

    fn err(schema: serde_json::Value, args: serde_json::Value) {
        assert!(
            validate(&schema, &args).is_err(),
            "expected {args} to be refused by {schema}"
        );
    }

    #[test]
    fn a_numeric_string_is_coerced_to_a_number() {
        let schema = serde_json::json!({"type": "number"});
        assert_eq!(
            coerce(&schema, &serde_json::json!("3.5")),
            serde_json::json!(3.5)
        );

        let integer = serde_json::json!({"type": "integer"});
        assert_eq!(
            coerce(&integer, &serde_json::json!("7")),
            serde_json::json!(7)
        );
    }

    #[test]
    fn a_boolean_string_is_coerced_to_a_boolean() {
        let schema = serde_json::json!({"type": "boolean"});
        assert_eq!(
            coerce(&schema, &serde_json::json!("true")),
            serde_json::json!(true)
        );
        assert_eq!(
            coerce(&schema, &serde_json::json!("false")),
            serde_json::json!(false)
        );
    }

    #[test]
    fn a_null_takes_the_declared_default_only_when_null_is_not_allowed() {
        // A required-typed field with a default: null means "missing".
        let schema = serde_json::json!({"type": "integer", "default": 620});
        assert_eq!(
            coerce(&schema, &serde_json::Value::Null),
            serde_json::json!(620)
        );

        // An `Option<T>` field declares `["integer","null"]`: an explicit null
        // is a legitimate `None`, not a missing value.
        let optional = serde_json::json!({"type": ["integer", "null"], "default": 620});
        assert_eq!(
            coerce(&optional, &serde_json::Value::Null),
            serde_json::Value::Null
        );
    }

    #[test]
    fn a_union_that_accepts_strings_is_left_alone() {
        let schema = serde_json::json!({"type": ["string", "number"]});
        assert_eq!(
            coerce(&schema, &serde_json::json!("17")),
            serde_json::json!("17")
        );
    }

    #[test]
    fn coercion_reaches_nested_values() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}}
        });
        assert_eq!(
            coerce(&schema, &serde_json::json!({"count": "17"})),
            serde_json::json!({"count": 17})
        );
    }

    #[test]
    fn required_properties_are_enforced() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "count": {"type": "integer"}},
            "required": ["name"]
        });
        ok(schema.clone(), serde_json::json!({"name": "qpu"}));
        ok(schema.clone(), serde_json::json!({"name": "qpu", "count": 17}));
        ok(schema.clone(), serde_json::json!({"name": "qpu", "count": "17"}));

        let missing = serde_json::json!({"count": 17});
        let failure = validate(&schema, &missing).unwrap_err();
        assert_eq!(failure.keyword, "required");
        assert_eq!(failure.path, "$");
        assert!(failure.message.contains("name"));
    }

    #[test]
    fn a_wrong_type_names_the_path_and_the_expected_type() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"width": {"type": "number"}}
        });
        let wrong = serde_json::json!({"width": true});
        let failure = validate(&schema, &wrong).unwrap_err();
        assert_eq!(failure.keyword, "type");
        assert_eq!(failure.path, "$.width");
        assert!(failure.message.contains("number"), "{}", failure.message);
    }

    #[test]
    fn enum_and_const_are_enforced() {
        err(serde_json::json!({"enum": ["sfq", "qc"]}), serde_json::json!("pc"));
        ok(serde_json::json!({"enum": ["sfq", "qc"]}), serde_json::json!("sfq"));
        err(serde_json::json!({"const": 3}), serde_json::json!(4));
    }

    #[test]
    fn arrays_are_validated_item_by_item() {
        let schema = serde_json::json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1
        });
        ok(schema.clone(), serde_json::json!(["a", "b"]));
        ok(
            serde_json::json!({"type": "array", "items": {"type": "integer"}}),
            serde_json::json!(["1", "2"]),
        );
        err(schema, serde_json::json!([]));
    }

    #[test]
    fn bounds_are_enforced() {
        err(
            serde_json::json!({"type": "string", "minLength": 2}),
            serde_json::json!("x"),
        );
        err(
            serde_json::json!({"type": "number", "maximum": 10}),
            serde_json::json!(11),
        );
        err(
            serde_json::json!({"type": "object", "maxProperties": 1}),
            serde_json::json!({"a": 1, "b": 2}),
        );
    }

    #[test]
    fn any_of_and_one_of_are_enforced() {
        let schema = serde_json::json!({
            "anyOf": [{"type": "string"}, {"type": "integer"}]
        });
        ok(schema.clone(), serde_json::json!("text"));
        ok(schema.clone(), serde_json::json!(5));
        err(schema, serde_json::json!(true));
    }

    #[test]
    fn refs_resolve_against_the_root_document() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"spec": {"$ref": "#/definitions/ChipSpec"}},
            "required": ["spec"],
            "definitions": {
                "ChipSpec": {
                    "type": "object",
                    "properties": {"qubits": {"type": "integer"}},
                    "required": ["qubits"]
                }
            }
        });
        ok(schema.clone(), serde_json::json!({"spec": {"qubits": 17}}));
        err(schema, serde_json::json!({"spec": {}}));
    }

    #[test]
    fn additional_properties_false_rejects_unknown_keys() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"a": {"type": "integer"}},
            "additionalProperties": false
        });
        ok(schema.clone(), serde_json::json!({"a": 1}));
        let unknown = serde_json::json!({"a": 1, "b": 2});
        let failure = validate(&schema, &unknown).unwrap_err();
        assert_eq!(failure.keyword, "additionalProperties");
        assert_eq!(failure.path, "$.b");
    }

    #[test]
    fn an_unknown_type_name_is_not_a_reason_to_refuse() {
        ok(serde_json::json!({"type": "custom"}), serde_json::json!(true));
    }

    #[test]
    fn a_schema_without_a_type_still_validates_its_keywords() {
        let schema = serde_json::json!({"required": ["a"]});
        ok(schema.clone(), serde_json::json!({"a": 1}));
        err(schema, serde_json::json!({}));
    }

    #[test]
    fn the_legacy_endpoint_shape_is_coerced_but_not_refused_by_coercion() {
        // The regression that motivated the opt-in: `SfqEndpoint` accepts a
        // legacy `[instance, hook]` array, but its schemars schema describes
        // only the canonical object. Coercion must not refuse it — and does
        // not, because coercion never refuses.
        let schema = serde_json::json!({
            "type": "object",
            "required": ["instance"],
            "properties": {"instance": {"type": "string"}}
        });
        let legacy = serde_json::json!(["q0", "ptl:clk:out"]);
        assert_eq!(coerce(&schema, &legacy), legacy);
        // Deep validation does refuse it, which is exactly why the product
        // opts out of deep validation for the tool that takes it.
        assert!(validate(&schema, &legacy).is_err());
    }
}
