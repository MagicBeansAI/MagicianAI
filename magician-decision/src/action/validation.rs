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
    if !validator(tool)?.is_valid(&call.arguments) {
        return Err("arguments_do_not_match_schema".into());
    }
    Ok(())
}
