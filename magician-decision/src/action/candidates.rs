//! Generic candidate construction from schemas and evidence. Free-form content
//! is never invented here; unsupported or ambiguous shapes go to the planner.

use decision_engine_contract::action::{ActionCandidate, ActionRequest, ArgumentBinding, ToolCall};
use serde_json::{Map, Value};

use super::validation::{validate_call, validator, MAX_CANDIDATES, MAX_VALUE_OPTIONS};

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

fn collect_named(value: &Value, name: &str, out: &mut Vec<Value>, depth: usize) {
    if depth > 12 || out.len() >= MAX_VALUE_OPTIONS {
        return;
    }
    match value {
        Value::Object(fields) => {
            if let Some(value) = fields.get(name) {
                if !value.is_null() && !out.contains(value) {
                    out.push(value.clone());
                }
            }
            for nested in fields.values() {
                collect_named(nested, name, out, depth + 1);
            }
        },
        Value::Array(values) => {
            for value in values.iter().take(32) {
                collect_named(value, name, out, depth + 1);
            }
        },
        _ => {},
    }
}

fn values(request: &ActionRequest, name: &str, schema: &Value) -> Vec<Value> {
    if let Some(value) = schema.get("const") {
        return vec![value.clone()];
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.iter().take(MAX_CANDIDATES).cloned().collect();
    }
    if schema.get("type").and_then(Value::as_str) == Some("boolean") {
        return vec![Value::Bool(false), Value::Bool(true)];
    }
    let mut found = Vec::new();
    // Only the newest result supplies implicit values. Older evidence requires
    // an explicit planner binding, judged again against the current context.
    if let Some(evidence) = request
        .context
        .evidence
        .last()
        .filter(|e| e.succeeded != Some(false))
    {
        collect_named(&evidence.value, name, &mut found, 0);
    }
    found
}

/// Complete calls, never independently mixed tool/target/argument heads.
/// Candidate order is deterministic so IDs remain auditable within a snapshot.
pub fn prepare_candidates(request: &ActionRequest) -> Vec<ActionCandidate> {
    let mut candidates = Vec::new();
    if let Some(first) = request.plan.steps.first() {
        if let Ok(call) = materialize(request, first) {
            candidates.push(ActionCandidate {
                id: "plan:0".into(),
                call,
                bindings: Vec::new(),
                reason: first.reason.clone(),
            });
        }
    }
    for tool in &request.tools {
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
        let Ok(compiled) = validator(tool) else {
            continue;
        };
        let Some(properties) = tool.parameters.get("properties").and_then(Value::as_object) else {
            let args = serde_json::json!({});
            if compiled.is_valid(&args) {
                push_unique(
                    &mut candidates,
                    ToolCall {
                        tool: tool.name.clone(),
                        arguments: args,
                    },
                );
            }
            continue;
        };
        let required: Vec<_> = tool
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut options = vec![Map::new()];
        for name in required {
            let Some(schema) = properties.get(name) else {
                options.clear();
                break;
            };
            let choices = values(request, name, schema);
            if choices.is_empty() {
                options.clear();
                break;
            }
            let mut expanded = Vec::new();
            for args in &options {
                for value in &choices {
                    let mut args = args.clone();
                    args.insert(name.into(), value.clone());
                    expanded.push(args);
                    if expanded.len() >= MAX_CANDIDATES {
                        break;
                    }
                }
                if expanded.len() >= MAX_CANDIDATES {
                    break;
                }
            }
            options = expanded;
        }
        for args in options {
            let arguments = Value::Object(args);
            if compiled.is_valid(&arguments) {
                push_unique(
                    &mut candidates,
                    ToolCall {
                        tool: tool.name.clone(),
                        arguments,
                    },
                );
            }
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    candidates
}

fn push_unique(candidates: &mut Vec<ActionCandidate>, call: ToolCall) {
    if candidates.iter().any(|candidate| candidate.call == call) {
        return;
    }
    let id = format!("schema:{}", candidates.len());
    candidates.push(ActionCandidate {
        id,
        call,
        bindings: Vec::new(),
        reason: String::new(),
    });
}
