//! Bounds and JSON Schema validation for the neutral rail. No tool-name policy.

use std::collections::BTreeSet;

use decision_engine_contract::action::{ActionRequest, ActionTool, ToolCall};
use serde_json::Value;

pub const MAX_TOOLS: usize = 256;
pub const MAX_EVIDENCE: usize = 32;
pub const MAX_PLAN: usize = 32;
pub const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SCHEMA_BYTES: usize = 128 * 1024;
pub const MAX_ARGUMENT_BYTES: usize = 256 * 1024;
pub const MAX_CANDIDATES: usize = 64;
pub const MAX_VALUE_OPTIONS: usize = 8;

/// Keep schema compilation local and bounded, including when a tool carries
/// $ref. The validator's network/file resolver features are also disabled.
fn local_schema(schema: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match schema {
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            if matches!(key.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef")
                && !value
                    .as_str()
                    .is_some_and(|reference| reference.starts_with('#'))
            {
                return false;
            }
            local_schema(value, depth + 1)
        }),
        Value::Array(values) => values.iter().all(|v| local_schema(v, depth + 1)),
        _ => true,
    }
}

pub fn validator(tool: &ActionTool) -> Result<jsonschema::Validator, String> {
    if serde_json::to_vec(&tool.parameters)
        .map_err(|_| "invalid_schema")?
        .len()
        > MAX_SCHEMA_BYTES
        || !local_schema(&tool.parameters, 0)
    {
        return Err("unsupported_schema".into());
    }
    jsonschema::options()
        .build(&tool.parameters)
        .map_err(|_| "invalid_schema".into())
}

pub fn validate_request(request: &ActionRequest) -> Result<(), String> {
    if request.snapshot.trim().is_empty() || request.snapshot.len() > 256 {
        return Err("invalid_snapshot".into());
    }
    if request.tools.len() > MAX_TOOLS
        || request.context.evidence.len() > MAX_EVIDENCE
        || request.plan.steps.len() > MAX_PLAN
        || serde_json::to_vec(request)
            .map_err(|_| "invalid_request")?
            .len()
            > MAX_REQUEST_BYTES
    {
        return Err("request_budget_exceeded".into());
    }
    let mut names = BTreeSet::new();
    for tool in &request.tools {
        if tool.name.trim().is_empty() || tool.name.len() > 256 || !names.insert(&tool.name) {
            return Err("invalid_tool_catalog".into());
        }
    }
    let mut ids = BTreeSet::new();
    for evidence in &request.context.evidence {
        if evidence.id.is_empty() || evidence.id == "latest" || !ids.insert(&evidence.id) {
            return Err("invalid_evidence_ids".into());
        }
    }
    ids.clear();
    for step in &request.plan.steps {
        if step.id.is_empty()
            || step.id.len() > 256
            || !ids.insert(&step.id)
            || step.bindings.len() > 32
        {
            return Err("invalid_plan".into());
        }
    }
    Ok(())
}

pub fn validate_call(call: &ToolCall, tools: &[ActionTool]) -> Result<(), String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name == call.tool)
        .ok_or("tool_not_authorized")?;
    if !call.arguments.is_object()
        || serde_json::to_vec(&call.arguments)
            .map_err(|_| "invalid_arguments")?
            .len()
            > MAX_ARGUMENT_BYTES
    {
        return Err("invalid_arguments".into());
    }
    let validator = validator(tool)?;
    if !validator.is_valid(&call.arguments) {
        // The host substitutes value-reference markers (`{"$ref": …}`,
        // `{"$file": …}`) with the complete value before dispatch and checks
        // the substituted call itself. A marker stands in for any value, so a
        // schema error at or inside one is not a mismatch; every other error
        // still rejects the call.
        let markers = value_reference_marker_paths(&call.arguments);
        if markers.is_empty()
            || validator.iter_errors(&call.arguments).any(|error| {
                let path = error.instance_path.as_str();
                !markers.iter().any(|marker| {
                    path == marker
                        || path
                            .strip_prefix(marker.as_str())
                            .is_some_and(|rest| rest.starts_with('/'))
                })
            })
        {
            return Err("arguments_do_not_match_schema".into());
        }
    }
    Ok(())
}

/// A host value-reference marker: `{"$ref": "<string>"}` alone, or
/// `{"$file": "<string>"}` with an optional `format`.
fn is_value_reference_marker(map: &serde_json::Map<String, Value>) -> bool {
    match (map.get("$ref"), map.get("$file")) {
        (Some(Value::String(_)), None) => map.len() == 1,
        (None, Some(Value::String(_))) => map.keys().all(|key| key == "$file" || key == "format"),
        _ => false,
    }
}

/// JSON pointers (RFC 6901) of every marker in an argument object.
fn value_reference_marker_paths(arguments: &Value) -> Vec<String> {
    fn escape(segment: &str) -> String {
        segment.replace('~', "~0").replace('/', "~1")
    }
    fn walk(value: &Value, path: String, out: &mut Vec<String>, depth: usize) {
        if depth > 64 || out.len() >= 64 {
            return;
        }
        match value {
            Value::Object(map) if is_value_reference_marker(map) => out.push(path),
            Value::Object(map) => {
                for (key, child) in map {
                    walk(child, format!("{path}/{}", escape(key)), out, depth + 1);
                }
            },
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, format!("{path}/{index}"), out, depth + 1);
                }
            },
            _ => {},
        }
    }
    let mut out = Vec::new();
    // The root object itself is never a marker: the call needs an object.
    if let Value::Object(map) = arguments {
        for (key, child) in map {
            walk(child, format!("/{}", escape(key)), &mut out, 1);
        }
    }
    out
}

#[cfg(test)]
mod value_reference_tests {
    use serde_json::json;

    use super::*;

    fn call_tool() -> ActionTool {
        ActionTool {
            name: "mcp__call_tool".into(),
            description: String::new(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tool_name": {"type": "string"},
                    "arguments_json": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["tool_name", "arguments_json"],
                "additionalProperties": false
            }),
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall {
            tool: "mcp__call_tool".into(),
            arguments,
        }
    }

    #[test]
    fn a_reference_marker_stands_in_for_any_declared_type() {
        let tools = [call_tool()];
        assert!(validate_call(
            &call(json!({"tool_name": "execute_sql", "arguments_json": {"$file": "/task/q.json"}})),
            &tools
        )
        .is_ok());
        assert!(validate_call(
            &call(json!({"tool_name": "x", "arguments_json": "{}", "limit": {"$ref": "result_ref_v1_a#/n"}})),
            &tools
        )
        .is_ok());
    }

    #[test]
    fn errors_outside_markers_still_reject() {
        let tools = [call_tool()];
        // Wrong type on a non-marker field.
        assert_eq!(
            validate_call(
                &call(json!({"tool_name": 7, "arguments_json": {"$file": "/q"}})),
                &tools
            ),
            Err("arguments_do_not_match_schema".to_string())
        );
        // Missing required field.
        assert!(validate_call(&call(json!({"arguments_json": {"$file": "/q"}})), &tools).is_err());
        // A marker with extra keys is not a marker.
        assert!(validate_call(
            &call(json!({"tool_name": "x", "arguments_json": {"$file": "/q", "extra": 1}})),
            &tools
        )
        .is_err());
    }
}
