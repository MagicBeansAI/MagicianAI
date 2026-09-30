//! `write_file` — write content to a file (creates parent directories
//! by default).
//!
//! Stages a create/modify transaction and returns a diff-approval pause
//! envelope; the actual write happens only after the operator applies the
//! transaction from HITL.

use std::sync::Arc;

use serde_json::{json, Value};

use super::staged_file_edit;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::file_edit::transaction::ProposedEdit;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(file_path) = args.get("file_path").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "write_file requires `file_path` (absolute path string).",
        }));
    };
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "write_file requires `content` (string to write).",
        }));
    };
    let create_dirs = args
        .get("create_dirs")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let scoped = match staged_file_edit::resolve_path(&resources, &args, file_path) {
        Ok(path) => path,
        Err(err) => {
            // Out-of-workspace write: escalate to the sandbox-override HITL (the
            // same recoverable path reads use) so the owner can approve the folder.
            // On approval the root joins `session_file_sandbox_roots` and the retry
            // stages the write — still gated by the per-write diff-approval, and
            // containment-checked against the approved root at apply time. A
            // non-sandbox resolve failure (empty/invalid path) stays a soft error.
            if staged_file_edit::is_workspace_root_denied(&err) {
                if crate::config::external_writes_enabled() {
                    return Err(ExecutionError::PathAccessDenied {
                        paths: vec![file_path.to_string()],
                    });
                }
                // Feature OFF (default): out-of-workspace writes stay scoped — give the
                // model an actionable message instead of escalating a HITL that can't land.
                return Ok(json!({
                    "status": "error",
                    "reason": format!(
                        "{err:#} — write inside your scoped workspace (out-of-workspace writes are disabled)."
                    ),
                }));
            }
            return Ok(json!({
                "status": "error",
                "reason": format!("Invalid `file_path`: {err:#}"),
            }));
        },
    };

    let exists = scoped.absolute_path.exists();
    if !exists && !create_dirs {
        let parent_exists = scoped
            .absolute_path
            .parent()
            .map(std::path::Path::exists)
            .unwrap_or(false);
        if !parent_exists {
            return Ok(json!({
                "status": "error",
                "reason": format!(
                    "Parent directory for `{file_path}` does not exist and `create_dirs` is false."
                ),
            }));
        }
    }

    if exists {
        match staged_file_edit::read_text_bounded(&scoped.absolute_path) {
            Ok(existing) if existing == content => {
                return Ok(json!({
                    "status": "ok",
                    "file_path": file_path,
                    "byte_count": content.len(),
                    "create_dirs": create_dirs,
                    "no_change": true,
                }));
            },
            Ok(_) => {},
            Err(err) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("Could not read existing `{file_path}`: {err:#}"),
                }));
            },
        }
    }

    let edit = if exists {
        ProposedEdit::Modify {
            path: scoped.relative_path.clone(),
            new_content: content.to_string(),
        }
    } else {
        ProposedEdit::Create {
            path: scoped.relative_path.clone(),
            content: content.to_string(),
        }
    };

    // An approved out-of-workspace write must be VISIBLE to the operator on the
    // diff-approval screen (gate 2), not just on the earlier sandbox-override
    // screen — so surface the ABSOLUTE destination + an explicit marker in the
    // rationale rather than the bare relative filename.
    let verb = if exists { "Rewrite" } else { "Create" };
    let rationale = if scoped.apply_root.is_some() {
        format!(
            "{verb} {} [OUTSIDE WORKSPACE — approved external write]",
            scoped.absolute_path.display()
        )
    } else {
        format!("{verb} {}", scoped.relative_path.display())
    };
    match staged_file_edit::stage_one_edit(&resources, &scoped, rationale, edit) {
        Ok(mut value) => {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "file_path".to_string(),
                    Value::String(file_path.to_string()),
                );
                obj.insert("byte_count".to_string(), json!(content.len()));
                obj.insert("create_dirs".to_string(), json!(create_dirs));
            }
            Ok(value)
        },
        Err(e) => Ok(json!({
            "status": "error",
            "file_path": file_path,
            "byte_count": content.len(),
            "create_dirs": create_dirs,
            "reason": format!("Could not stage `{file_path}` for approval: {e:#}"),
        })),
    }
}
