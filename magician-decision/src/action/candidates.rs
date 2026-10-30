//! Generic candidate construction from schemas and evidence. Free-form content
//! is never invented here; unsupported or ambiguous shapes go to the planner.

use decision_engine_contract::action::{ActionCandidate, ActionRequest, ArgumentBinding, ToolCall};
use serde_json::{Map, Value};

use super::validation::{
    validate_call, validator, MAX_ARGUMENT_BYTES, MAX_CANDIDATES, MAX_REQUEST_BYTES,
};

fn bound_value(request: &ActionRequest, binding: &ArgumentBinding) -> Option<Value> {
    let evidence = if binding.evidence == "latest" {
        request.context.evidence.last()
    } else {
        request
            .context
            .evidence
            .iter()
            .find(|e| e.id == binding.evidence)
    }?;
    // Failed tool results cannot become arguments for automatic continuation.
    if evidence.succeeded == Some(false) {
        return None;
    }
    evidence.value.pointer(&binding.pointer).cloned()
}

pub fn materialize(
    request: &ActionRequest,
    candidate: &ActionCandidate,
) -> Result<ToolCall, String> {
    let mut call = candidate.call.clone();
    let mut used = std::collections::BTreeSet::new();
    for binding in &candidate.bindings {
        if !binding.argument.starts_with('/') || !used.insert(&binding.argument) {
            return Err("invalid_argument_binding".into());
        }
        let value = bound_value(request, binding).ok_or("evidence_binding_unavailable")?;
        // The proposed argument tree must contain a placeholder at the pointer.
        // This avoids creating arbitrary paths or interpreting binding text.
        *call
            .arguments
            .pointer_mut(&binding.argument)
            .ok_or("argument_binding_unavailable")? = value;
    }
    validate_call(&call, &request.tools)?;
    Ok(call)
}

/// Candidate generation deliberately does not parse prose or synthesize strings.
/// Each row inherits only its ancestors' fields, never a sibling's arguments.
#[derive(Clone, Default)]
struct Shape {
    properties: Map<String, Value>,
    required: std::collections::BTreeSet<String>,
}

fn merge(mut left: Shape, right: Shape) -> Shape {
    left.required.extend(right.required);
    for (name, schema) in right.properties {
        left.properties
            .entry(name)
            .and_modify(|prior| {
                if *prior != schema {
                    *prior = serde_json::json!({"allOf": [prior, &schema]});
                }
            })
            .or_insert(schema);
    }
    left
}

/// Expand object alternatives only; the original schema remains authoritative.
/// Unsupported/deep shapes may yield no candidates, never an unchecked call.
fn shapes(schema: &Value, depth: usize) -> Vec<Shape> {
    if depth > 12 {
        return Vec::new();
    }
    let base = Shape {
        properties: schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        required: schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    };
    let mut result = vec![base];
    for key in ["allOf", "anyOf", "oneOf"] {
        let Some(parts) = schema.get(key).and_then(Value::as_array) else {
            continue;
        };
        if key == "allOf" {
            for part in parts {
                let additions = shapes(part, depth + 1);
                result = result
                    .iter()
                    .flat_map(|left| {
                        additions
                            .iter()
                            .map(move |right| merge(left.clone(), right.clone()))
                    })
                    .take(MAX_CANDIDATES)
                    .collect();
            }
        } else {
            let additions: Vec<_> = parts
                .iter()
                .flat_map(|part| shapes(part, depth + 1))
                .take(MAX_CANDIDATES)
                .collect();
            result = result
                .iter()
                .flat_map(|left| {
                    additions
                        .iter()
                        .map(move |right| merge(left.clone(), right.clone()))
                })
                .take(MAX_CANDIDATES)
                .collect();
        }
    }
    result
}

fn keyword<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema.get(key).or_else(|| {
        schema
            .get("allOf")
            .and_then(Value::as_array)?
            .iter()
            .find_map(|part| keyword(part, key))
    })
}

fn finite_values(schema: &Value) -> Vec<Value> {
    if let Some(value) = keyword(schema, "const") {
        return vec![value.clone()];
    }
    if let Some(values) = keyword(schema, "enum").and_then(Value::as_array) {
        return values.iter().take(MAX_CANDIDATES).cloned().collect();
    }
    if keyword(schema, "type").and_then(Value::as_str) == Some("boolean") {
        return vec![Value::Bool(false), Value::Bool(true)];
    }
    Vec::new()
}

fn rows<'a>(
    value: &'a Value,
    ancestors: &[&'a Map<String, Value>],
    out: &mut Vec<Vec<&'a Map<String, Value>>>,
    depth: usize,
) {
    if depth > 12 || out.len() >= 256 {
        return;
    }
    match value {
        Value::Object(fields) => {
            let mut row = ancestors.to_vec();
            row.push(fields);
            out.push(row.clone());
            for child in fields.values() {
                rows(child, &row, out, depth + 1);
            }
        },
        Value::Array(items) => {
            for item in items.iter().take(32) {
                rows(item, ancestors, out, depth + 1);
            }
        },
        _ => {},
    }
}

fn grounded<'a>(row: &[&'a Map<String, Value>], name: &str, schema: &Value) -> Option<&'a Value> {
    let aliases = keyword(schema, "x-magician-grounding-aliases").and_then(Value::as_array);
    // A declared name always takes precedence over its aliases in the same row.
    for fields in row.iter().rev() {
        for key in
            std::iter::once(name).chain(aliases.into_iter().flatten().filter_map(Value::as_str))
        {
            if let Some(value) = fields.get(key).filter(|value| !value.is_null()) {
                return Some(value);
            }
        }
    }
    None
}

#[derive(Default)]
pub struct CandidateSet {
    pub candidates: Vec<ActionCandidate>,
    pub tools_without_candidates: usize,
    pub truncated: bool,
}

/// Complete calls, never independently mixed tool/target/argument heads.
/// Candidate order is deterministic; round-robin admission avoids one tool
/// consuming the entire catalog budget before later tools are considered.
pub fn prepare_candidates(request: &ActionRequest) -> Vec<ActionCandidate> {
    prepare_candidate_set(request).candidates
}

pub fn prepare_candidate_set(request: &ActionRequest) -> CandidateSet {
    let mut result = CandidateSet::default();
    if let Some(first) = request.plan.steps.first() {
        if let Ok(call) = materialize(request, first) {
            result.candidates.push(ActionCandidate {
                id: "plan:0".into(),
                call,
                bindings: Vec::new(),
                reason: first.reason.clone(),
            });
        }
    }
    let mut evidence_rows = Vec::new();
    if let Some(evidence) = request
        .context
        .evidence
        .last()
        .filter(|e| e.succeeded != Some(false))
    {
        rows(&evidence.value, &[], &mut evidence_rows, 0);
    }
    if evidence_rows.is_empty() {
        evidence_rows.push(Vec::new());
    }
    let mut per_tool = Vec::new();
    let mut pool_bytes = 0_usize;
    for tool in &request.tools {
        let Ok(compiled) = validator(tool) else {
            result.tools_without_candidates += 1;
            continue;
        };
        let mut calls: Vec<(ToolCall, bool)> = Vec::new();
        let mut tool_bytes = 0_usize;
        for shape in shapes(&tool.parameters, 0) {
            let properties: Vec<_> = shape
                .properties
                .iter()
                .map(|(name, schema)| {
                    let check = evidence_rows
                        .iter()
                        .any(|row| grounded(row, name, schema).is_some())
                        .then(|| jsonschema::options().build(schema).ok())
                        .flatten();
                    (name, schema, check)
                })
                .collect();
            for row in &evidence_rows {
                let mut options = vec![Map::new()];
                for (name, schema, check) in &properties {
                    let mut choices: Vec<_> = grounded(row, name, schema)
                        .filter(|value| {
                            check.as_ref().is_none_or(|check| check.is_valid(value))
                                && serde_json::to_vec(value)
                                    .is_ok_and(|bytes| bytes.len() <= MAX_ARGUMENT_BYTES)
                        })
                        .cloned()
                        .into_iter()
                        .collect();
                    if shape.required.contains(*name) {
                        for value in finite_values(schema) {
                            if !choices.contains(&value) {
                                choices.push(value);
                            }
                        }
                    }
                    if choices.is_empty() {
                        if shape.required.contains(*name) {
                            options.clear();
                            break;
                        }
                        continue;
                    }
                    options = options
                        .iter()
                        .flat_map(|args| {
                            choices.iter().map(move |value| {
                                let mut args = args.clone();
                                args.insert((*name).clone(), value.clone());
                                args
                            })
                        })
                        .take(MAX_CANDIDATES)
                        .collect();
                }
                for args in options {
                    // Keep both the enriched row and a valid minimal call:
                    // an observed optional field need not be the intended input.
                    let minimal = args
                        .iter()
                        .filter(|(name, _)| shape.required.contains(*name))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect();
                    for args in [args, minimal] {
                        let arguments = Value::Object(args);
                        if compiled.is_valid(&arguments) {
                            let call = ToolCall {
                                tool: tool.name.clone(),
                                arguments,
                            };
                            if !calls.iter().any(|(existing, _)| existing == &call) {
                                let evidence_grounded =
                                    shape.properties.iter().any(|(name, schema)| {
                                        grounded(row, name, schema).is_some_and(|value| {
                                            call.arguments.get(name) == Some(value)
                                        })
                                    });
                                let bytes =
                                    serde_json::to_vec(&call).expect("call serializes").len();
                                if bytes > MAX_ARGUMENT_BYTES
                                    || tool_bytes + bytes > MAX_ARGUMENT_BYTES
                                    || pool_bytes + bytes > MAX_REQUEST_BYTES * 2
                                {
                                    result.truncated = true;
                                    continue;
                                }
                                tool_bytes += bytes;
                                pool_bytes += bytes;
                                calls.push((call, evidence_grounded));
                            }
                        }
                        if calls.len() > MAX_CANDIDATES {
                            break;
                        }
                    }
                    if calls.len() > MAX_CANDIDATES {
                        break;
                    }
                }
                if calls.len() > MAX_CANDIDATES {
                    break;
                }
            }
            if calls.len() > MAX_CANDIDATES {
                break;
            }
        }
        if calls.is_empty() {
            result.tools_without_candidates += 1;
        }
        if calls.len() > MAX_CANDIDATES {
            result.truncated = true;
            calls.truncate(MAX_CANDIDATES);
        }
        per_tool.push(calls);
    }
    // Fresh evidence bindings take precedence over generic default/enum calls.
    // Within each tier, take turns across tools before offering more arguments
    // for one tool. This keeps default actions from exhausting the whole cap.
    for grounded in [true, false] {
        let tier: Vec<Vec<_>> = per_tool
            .iter()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|(_, has_evidence)| *has_evidence == grounded)
                    .map(|(call, _)| call)
                    .collect()
            })
            .collect();
        for index in 0..MAX_CANDIDATES {
            for calls in &tier {
                if let Some(call) = calls.get(index) {
                    if result
                        .candidates
                        .iter()
                        .any(|candidate| candidate.call == **call)
                    {
                        continue;
                    }
                    if result.candidates.len() == MAX_CANDIDATES {
                        result.truncated = true;
                        continue;
                    }
                    let id = format!("schema:{}", result.candidates.len());
                    result.candidates.push(ActionCandidate {
                        id,
                        call: (*call).clone(),
                        bindings: Vec::new(),
                        reason: String::new(),
                    });
                }
            }
        }
    }
    result
}
