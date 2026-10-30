//! Data by reference: large values move between tools without passing through
//! the model.
//!
//! A model can only see a bounded projection of a large tool result, and it
//! cannot reliably re-type tens of kilobytes into the next call's arguments.
//! Two generic primitives close that gap for every tool:
//!
//! - **Argument references.** Anywhere in a Pack call's arguments, an object
//!   `{"$ref": "result_ref_v1_…#/json/pointer"}` is replaced by the complete
//!   value at that pointer of a prior result, and `{"$file": "<path>"}` by the
//!   contents of a file in this task's directory (`"format": "json"` parses it).
//!   JSON-encoded string arguments (MCP `arguments_json`) are searched too, and
//!   re-serialized after substitution.
//! - **`export_result`.** Writes the complete value at a pointer of a prior
//!   result to a file under `<task>/workdir/exports/`, so shell, DuckDB, marimo
//!   and `read_file` can work on it, and `{"$file": …}` can send it onward.
//!
//! Neither widens authority. `$ref` and `export_result` read through exactly
//! the store authority `read_result` uses (same owner, agent, originating tool
//! grant and revision); `$file` is confined to the task directory. Substitution
//! runs at the top of `execute_action_inner`, so the outward gate, approvals
//! and trust policy judge the real values, while history and evidence keep the
//! compact reference form the model wrote.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{
    executor::{
        autonomous_result_access, canonical_result_error_value, task_filesystem_root,
        ActionExecutors, TrustDispatchGuard,
    },
    types::AgenticContext,
};
use crate::magician_v2::{
    execution::actions::{ActionResult, ExecutableAction},
    tool_result_materialization::ScopedResultRef,
};

pub(crate) const EXPORT_RESULT_TOOL: &str = "export_result";
const REF_KEY: &str = "$ref";
const FILE_KEY: &str = "$file";
const FORMAT_KEY: &str = "format";
/// Same ceiling the governed MCP runtime applies to one `arguments_json`.
const MAX_SUBSTITUTED_BYTES: usize = 16 * 1024 * 1024;
const MAX_REFERENCES_PER_ACTION: usize = 32;
const MAX_NESTED_JSON_STRING_BYTES: usize = 16 * 1024 * 1024;
const EXPORTS_DIR: &str = "exports";
const PREVIEW_CHARS: usize = 400;

/// One reference marker, as the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ValueRef {
    Result {
        content_ref: String,
        pointer: String,
    },
    File {
        path: String,
        json: bool,
    },
}

/// `Some(Ok(_))` for a well-formed marker object, `Some(Err(_))` for an object
/// that uses a marker key but is malformed, `None` for an ordinary object.
fn parse_marker(map: &Map<String, Value>) -> Option<std::result::Result<ValueRef, String>> {
    if let Some(target) = map.get(REF_KEY) {
        if map.len() != 1 {
            return Some(Err(format!(
                "`{REF_KEY}` must be the only key in its object"
            )));
        }
        let Some(target) = target.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
            return Some(Err(format!(
                "`{REF_KEY}` must be a string like \"result_ref_v1_…#/json/pointer\""
            )));
        };
        let (content_ref, pointer) = match target.split_once('#') {
            Some((content_ref, pointer)) => (content_ref, pointer),
            None => (target, ""),
        };
        return Some(Ok(ValueRef::Result {
            content_ref: content_ref.trim().to_string(),
            pointer: pointer.to_string(),
        }));
    }
    if let Some(path) = map.get(FILE_KEY) {
        if map.keys().any(|key| key != FILE_KEY && key != FORMAT_KEY) {
            return Some(Err(format!(
                "`{FILE_KEY}` allows only an optional `{FORMAT_KEY}` beside it"
            )));
        }
        let Some(path) = path.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
            return Some(Err(format!("`{FILE_KEY}` must be a file path string")));
        };
        let json = match map.get(FORMAT_KEY).map(|value| value.as_str()) {
            None | Some(Some("text")) => false,
            Some(Some("json")) => true,
            _ => {
                return Some(Err(format!(
                    "`{FORMAT_KEY}` must be \"text\" (default) or \"json\""
                )))
            },
        };
        return Some(Ok(ValueRef::File {
            path: path.to_string(),
            json,
        }));
    }
    None
}

/// A string argument that is itself a JSON document (MCP `arguments_json`).
/// Only strings that cheaply look like they carry a marker are parsed.
fn nested_json_with_markers(text: &str) -> Option<Value> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('['))
        || text.len() > MAX_NESTED_JSON_STRING_BYTES
        || !(text.contains(REF_KEY) || text.contains(FILE_KEY))
    {
        return None;
    }
    serde_json::from_str::<Value>(text).ok()
}

fn collect(value: &Value, found: &mut Vec<ValueRef>, errors: &mut Vec<String>) {
    match value {
        Value::Object(map) => match parse_marker(map) {
            Some(Ok(marker)) => found.push(marker),
            Some(Err(error)) => errors.push(error),
            None => map.values().for_each(|value| collect(value, found, errors)),
        },
        Value::Array(items) => items.iter().for_each(|value| collect(value, found, errors)),
        Value::String(text) => {
            if let Some(nested) = nested_json_with_markers(text) {
                collect(&nested, found, errors);
            }
        },
        _ => {},
    }
}

/// Replace every marker with its resolved value. Returns whether anything changed.
fn replace(value: &mut Value, resolved: &HashMap<ValueRef, Value>) -> bool {
    match value {
        Value::Object(map) => {
            if let Some(Ok(marker)) = parse_marker(map) {
                if let Some(replacement) = resolved.get(&marker) {
                    *value = replacement.clone();
                    return true;
                }
                return false;
            }
            let mut changed = false;
            for item in map.values_mut() {
                changed |= replace(item, resolved);
            }
            changed
        },
        Value::Array(items) => {
            let mut changed = false;
            for item in items.iter_mut() {
                changed |= replace(item, resolved);
            }
            changed
        },
        Value::String(text) => {
            let Some(mut nested) = nested_json_with_markers(text) else {
                return false;
            };
            if !replace(&mut nested, resolved) {
                return false;
            }
            match serde_json::to_string(&nested) {
                Ok(serialized) => {
                    *text = serialized;
                    true
                },
                Err(_) => false,
            }
        },
        _ => false,
    }
}

fn value_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        other => serde_json::to_vec(other)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX),
    }
}

/// True when the action's arguments carry at least one reference marker.
/// Cheap enough to call on every dispatch.
pub(crate) fn action_has_value_refs(action: &ExecutableAction) -> bool {
    let ExecutableAction::Pack {
        capability_name,
        resolved_params,
        ..
    } = action
    else {
        return false;
    };
    if is_result_tool(capability_name) {
        return false;
    }
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for value in resolved_params.values() {
        collect(value, &mut found, &mut errors);
        if !found.is_empty() || !errors.is_empty() {
            return true;
        }
    }
    false
}

/// The result tools take a `result_ref` string of their own; never rewrite them.
fn is_result_tool(capability_name: &str) -> bool {
    matches!(capability_name, "read_result" | EXPORT_RESULT_TOOL)
}

fn result_reads_denied(ctx: &AgenticContext) -> bool {
    ctx.denied_capability_names.iter().any(|denied| {
        crate::magician_v2::agents::types::tool_name_matches_block_entry("read_result", denied)
    })
}

/// Resolve and substitute every reference in a Pack action's arguments.
/// `Ok(None)` when there is nothing to substitute. An unresolvable reference is
/// an error the model sees as the call's failure, so it can correct the call.
pub(crate) async fn substitute_value_refs(
    action: &ExecutableAction,
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    trust_guard: Option<&TrustDispatchGuard>,
) -> Result<Option<ExecutableAction>> {
    let ExecutableAction::Pack {
        capability_name,
        implementation,
        resolved_params,
    } = action
    else {
        return Ok(None);
    };
    if is_result_tool(capability_name) {
        return Ok(None);
    }
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for value in resolved_params.values() {
        collect(value, &mut found, &mut errors);
    }
    if let Some(error) = errors.into_iter().next() {
        return Err(anyhow!("invalid argument reference: {error}"));
    }
    if found.is_empty() {
        return Ok(None);
    }
    // App workflows carry one immutable catalog and no ambient result access;
    // a reference is the same authority as `read_result`, which they refuse.
    if ctx.app_disclosure_guard.is_some() || is_app_workflow_task(ctx, executors).await? {
        return Err(anyhow!(
            "argument references are unavailable to app workflows"
        ));
    }
    let unique: Vec<ValueRef> = {
        let mut seen = HashSet::new();
        found
            .into_iter()
            .filter(|marker| seen.insert(marker.clone()))
            .collect()
    };
    if unique.len() > MAX_REFERENCES_PER_ACTION {
        return Err(anyhow!(
            "too many argument references ({}); at most {MAX_REFERENCES_PER_ACTION} per call",
            unique.len()
        ));
    }

    let mut resolved = HashMap::new();
    let mut total_bytes = 0usize;
    for marker in unique {
        let value = match &marker {
            ValueRef::Result {
                content_ref,
                pointer,
            } => {
                if result_reads_denied(ctx) {
                    return Err(anyhow!(
                        "`{REF_KEY}` result references are not available to this agent"
                    ));
                }
                resolve_result_value(ctx, executors, trust_guard, content_ref, pointer).await?
            },
            ValueRef::File { path, json } => resolve_file_value(ctx, executors, path, *json)?,
        };
        total_bytes = total_bytes.saturating_add(value_bytes(&value));
        if total_bytes > MAX_SUBSTITUTED_BYTES {
            return Err(anyhow!(
                "argument references expand to more than {} MB; filter or aggregate first",
                MAX_SUBSTITUTED_BYTES / (1024 * 1024)
            ));
        }
        resolved.insert(marker, value);
    }

    let mut params = resolved_params.clone();
    for value in params.values_mut() {
        replace(value, &resolved);
    }
    Ok(Some(ExecutableAction::Pack {
        capability_name: capability_name.clone(),
        implementation: implementation.clone(),
        resolved_params: params,
    }))
}

async fn is_app_workflow_task(ctx: &AgenticContext, executors: &ActionExecutors) -> Result<bool> {
    let (Some(service), Some(principal), Some(workspace), Some(task_id)) = (
        executors.artifact_v2_service.as_ref(),
        ctx.principal.as_deref(),
        ctx.workspace.as_deref(),
        ctx.task_id.as_deref(),
    ) else {
        return Ok(false);
    };
    let scope = crate::magician_v2::auth::ScopeRef::system_internal_unauthenticated(
        &principal.to_string(),
        &workspace.to_string(),
    );
    service
        .app_workflow_service()
        .is_workflow_task(&scope, task_id)
        .await
        .map_err(anyhow::Error::from)
}

async fn resolve_result_value(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    trust_guard: Option<&TrustDispatchGuard>,
    content_ref: &str,
    pointer: &str,
) -> Result<Value> {
    let describe = |error| {
        let detail = canonical_result_error_value(error);
        anyhow!(
            "could not resolve {{\"{REF_KEY}\": \"{content_ref}#{pointer}\"}}: {}",
            detail["error_code"].as_str().unwrap_or("result_error")
        )
    };
    let reference = ScopedResultRef::parse(content_ref).map_err(describe)?;
    let (store, read_context) =
        autonomous_result_access(ctx, executors, trust_guard).map_err(describe)?;
    store
        .read_value_at(&read_context, &reference, pointer)
        .await
        .map_err(describe)
}

/// Canonical task directory, which every `$file` and export must stay under.
fn task_root(ctx: &AgenticContext, executors: &ActionExecutors) -> Result<PathBuf> {
    let root = task_filesystem_root(ctx, executors)
        .ok_or_else(|| anyhow!("file references need a task workspace; none is bound"))?;
    std::fs::create_dir_all(root.join("workdir"))
        .map_err(|error| anyhow!("task workspace is unavailable: {error}"))?;
    root.canonicalize()
        .map_err(|error| anyhow!("task workspace is unavailable: {error}"))
}

fn resolve_file_value(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    path: &str,
    json: bool,
) -> Result<Value> {
    let root = task_root(ctx, executors)?;
    let requested = Path::new(path);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join("workdir").join(requested)
    };
    let resolved = candidate
        .canonicalize()
        .map_err(|error| anyhow!("{{\"{FILE_KEY}\": \"{path}\"}}: {error}"))?;
    if !resolved.starts_with(&root) {
        return Err(anyhow!(
            "{{\"{FILE_KEY}\": \"{path}\"}} is outside this task's directory ({}); \
             only task files can be passed by reference",
            root.display()
        ));
    }
    let metadata = std::fs::metadata(&resolved)
        .map_err(|error| anyhow!("{{\"{FILE_KEY}\": \"{path}\"}}: {error}"))?;
    if !metadata.is_file() {
        return Err(anyhow!("{{\"{FILE_KEY}\": \"{path}\"}} is not a file"));
    }
    if metadata.len() > MAX_SUBSTITUTED_BYTES as u64 {
        return Err(anyhow!(
            "{{\"{FILE_KEY}\": \"{path}\"}} is larger than {} MB",
            MAX_SUBSTITUTED_BYTES / (1024 * 1024)
        ));
    }
    let text = std::fs::read_to_string(&resolved)
        .map_err(|error| anyhow!("{{\"{FILE_KEY}\": \"{path}\"}} is not UTF-8 text: {error}"))?;
    if json {
        serde_json::from_str(&text)
            .map_err(|error| anyhow!("{{\"{FILE_KEY}\": \"{path}\"}} is not valid JSON: {error}"))
    } else {
        Ok(Value::String(text))
    }
}

/// `export_result`: write a result field to a task file. Failures come back as
/// a structured error payload, the same shape `read_result` uses.
pub(crate) async fn export_autonomous_tool_result(
    params: &HashMap<String, Value>,
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    trust_guard: Option<&TrustDispatchGuard>,
) -> Result<ActionResult> {
    let fail = |code: &str, reason: String| {
        Ok(ActionResult::text(
            json!({"status": "error", "error_code": code, "reason": reason}).to_string(),
        ))
    };
    let Some(content_ref) = params
        .get("result_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return fail(
            "invalid_result_request",
            "export_result requires `result_ref`".to_string(),
        );
    };
    let pointer = params
        .get("field_path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let format = params
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "auto" | "text" | "json" | "jsonl") {
        return fail(
            "invalid_result_request",
            "format must be one of auto, text, json, jsonl".to_string(),
        );
    }

    let reference = match ScopedResultRef::parse(content_ref) {
        Ok(reference) => reference,
        Err(error) => {
            return Ok(ActionResult::text(
                canonical_result_error_value(error).to_string(),
            ))
        },
    };
    let (store, read_context) = match autonomous_result_access(ctx, executors, trust_guard) {
        Ok(access) => access,
        Err(error) => {
            return Ok(ActionResult::text(
                canonical_result_error_value(error).to_string(),
            ))
        },
    };
    let value = match store
        .read_value_at(&read_context, &reference, &pointer)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            return Ok(ActionResult::text(
                canonical_result_error_value(error).to_string(),
            ))
        },
    };

    let (content, extension) = match render_export(&value, &format) {
        Ok(rendered) => rendered,
        Err(reason) => return fail("invalid_result_request", reason),
    };
    let root = match task_root(ctx, executors) {
        Ok(root) => root,
        Err(error) => return fail("export_unavailable", error.to_string()),
    };
    let workdir = root.join("workdir");
    let directory = workdir.join(EXPORTS_DIR);
    if let Err(error) = std::fs::create_dir_all(&directory) {
        return fail("export_unavailable", error.to_string());
    }
    let file_name = export_file_name(
        params.get("file_name").and_then(Value::as_str),
        content_ref,
        &pointer,
        extension,
    );
    let path = directory.join(&file_name);
    if let Err(error) = std::fs::write(&path, content.as_bytes()) {
        return fail("export_unavailable", error.to_string());
    }

    let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
    let preview: String = content.chars().take(PREVIEW_CHARS).collect();
    let records = match &value {
        Value::Array(items) => Some(items.len()),
        _ => None,
    };
    let path_text = path.display().to_string();
    let workspace_path = Path::new(EXPORTS_DIR).join(&file_name);
    Ok(ActionResult::text(
        json!({
            "status": "ok",
            "path": path_text,
            "workspace_path": workspace_path.to_string_lossy(),
            "workspace_root": workdir,
            "bytes": content.len(),
            "sha256": sha256,
            "value_type": json_type_name(&value),
            "records": records,
            "preview": preview,
            "preview_truncated": content.chars().count() > PREVIEW_CHARS,
            "how_to_use": format!(
                "Use `path` (absolute) or `workspace_path` (relative to this task's workdir) \
                 with file tools and governed CLI/MCP file arguments such as jq's input_file. \
                 To send its contents as any tool argument without retyping, pass \
                 {{\"{FILE_KEY}\": \"{path_text}\"}} (add \"format\": \"json\" to send parsed JSON)."
            ),
        })
        .to_string(),
    ))
}

fn render_export(
    value: &Value,
    format: &str,
) -> std::result::Result<(String, &'static str), String> {
    match (format, value) {
        ("auto" | "text", Value::String(text)) => Ok((text.clone(), "txt")),
        ("text", other) => Ok((other.to_string(), "txt")),
        ("jsonl", Value::Array(items)) => {
            let mut out = String::new();
            for item in items {
                out.push_str(&item.to_string());
                out.push('\n');
            }
            Ok((out, "jsonl"))
        },
        ("jsonl", _) => Err("format jsonl needs the selected field to be an array".to_string()),
        (_, other) => serde_json::to_string_pretty(other)
            .map(|text| (text, "json"))
            .map_err(|error| error.to_string()),
    }
}

fn export_file_name(
    requested: Option<&str>,
    content_ref: &str,
    pointer: &str,
    extension: &str,
) -> String {
    let sanitize = |text: &str| {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches(|c| c == '.' || c == '_')
            .chars()
            .take(96)
            .collect::<String>()
    };
    if let Some(name) = requested
        .map(|name| {
            Path::new(name)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
        })
        .map(sanitize)
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    let digest = content_ref
        .rsplit('_')
        .next()
        .unwrap_or(content_ref)
        .chars()
        .take(12)
        .collect::<String>();
    let field = sanitize(&pointer.trim_start_matches('/').replace('/', "-"));
    if field.is_empty() {
        format!("result-{digest}.{extension}")
    } else {
        format!("result-{digest}-{field}.{extension}")
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers(value: &Value) -> (Vec<ValueRef>, Vec<String>) {
        let mut found = Vec::new();
        let mut errors = Vec::new();
        collect(value, &mut found, &mut errors);
        (found, errors)
    }

    #[test]
    fn ref_marker_splits_pointer_and_defaults_to_root() {
        let (found, errors) = markers(&json!({
            "a": {"$ref": "result_ref_v1_abc#/rows/0"},
            "b": [{"$ref": "result_ref_v1_def"}]
        }));
        assert!(errors.is_empty());
        assert!(found.contains(&ValueRef::Result {
            content_ref: "result_ref_v1_abc".into(),
            pointer: "/rows/0".into()
        }));
        assert!(found.contains(&ValueRef::Result {
            content_ref: "result_ref_v1_def".into(),
            pointer: String::new()
        }));
    }

    #[test]
    fn markers_inside_json_string_arguments_are_found_and_replaced() {
        let mut params = json!({
            "tool_name": "execute_sql",
            "arguments_json": "{\"database_id\":236,\"sql\":{\"$file\":\"q.sql\"}}"
        });
        let (found, _) = markers(&params);
        let marker = ValueRef::File {
            path: "q.sql".into(),
            json: false,
        };
        assert_eq!(found, vec![marker.clone()]);
        let resolved = HashMap::from([(marker, json!("SELECT 1"))]);
        assert!(replace(&mut params, &resolved));
        let args: Value = serde_json::from_str(params["arguments_json"].as_str().unwrap()).unwrap();
        assert_eq!(args, json!({"database_id": 236, "sql": "SELECT 1"}));
        assert_eq!(params["tool_name"], json!("execute_sql"));
    }

    #[test]
    fn malformed_markers_are_errors_not_silent_literals() {
        let (_, errors) = markers(&json!({"x": {"$ref": "result_ref_v1_a", "extra": 1}}));
        assert_eq!(errors.len(), 1);
        let (_, errors) = markers(&json!({"x": {"$file": "a.txt", "format": "yaml"}}));
        assert_eq!(errors.len(), 1);
        let (_, errors) = markers(&json!({"x": {"$file": 7}}));
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn ordinary_strings_mentioning_markers_are_left_alone() {
        let mut params = json!({"note": "use $ref and $file when values are large"});
        let (found, errors) = markers(&params);
        assert!(found.is_empty() && errors.is_empty());
        assert!(!replace(&mut params, &HashMap::new()));
    }

    #[test]
    fn result_tools_are_never_rewritten() {
        let action = ExecutableAction::Pack {
            capability_name: "read_result".into(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "read_result".into(),
                },
            resolved_params: HashMap::from([(
                "result_ref".to_string(),
                json!({"$ref": "result_ref_v1_x"}),
            )]),
        };
        assert!(!action_has_value_refs(&action));
    }

    #[test]
    fn export_rendering_keeps_strings_raw_and_lines_arrays() {
        assert_eq!(
            render_export(&json!("SELECT 1"), "auto").unwrap(),
            ("SELECT 1".to_string(), "txt")
        );
        assert_eq!(
            render_export(&json!([{"a": 1}, {"a": 2}]), "jsonl")
                .unwrap()
                .0,
            "{\"a\":1}\n{\"a\":2}\n"
        );
        assert!(render_export(&json!({"a": 1}), "jsonl").is_err());
        assert_eq!(render_export(&json!({"a": 1}), "auto").unwrap().1, "json");
    }

    #[test]
    fn export_file_names_are_sanitized_basenames() {
        assert_eq!(
            export_file_name(Some("../../etc/passwd"), "result_ref_v1_abcdef", "", "txt"),
            "passwd"
        );
        assert_eq!(
            export_file_name(None, "result_ref_v1_0123456789abcdef", "/a/b", "json"),
            "result-0123456789ab-a-b.json"
        );
    }
}
