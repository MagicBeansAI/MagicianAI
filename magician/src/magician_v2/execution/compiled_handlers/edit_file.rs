//! `edit_file` — exact string replacement in a file.
//!
//! Reads the file, replaces `old_string` with `new_string` (one
//! occurrence by default, all when `replace_all: true`), then stages the
//! resulting diff for operator approval before anything is written.
//! Fails if `old_string` is missing, ambiguous (more than one match
//! without `replace_all`), or equal to `new_string`.

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
            "reason": "edit_file requires `file_path` (absolute path string).",
        }));
    };
    let Some(old_string) = args.get("old_string").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "edit_file requires `old_string` (string to replace).",
        }));
    };
    let Some(new_string) = args.get("new_string").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "edit_file requires `new_string` (replacement string).",
        }));
    };
    let replace_all = args
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if new_string == old_string {
        return Ok(json!({
            "status": "error",
            "reason": "edit_file requires `new_string` to differ from `old_string`.",
        }));
    }

    let scoped = match staged_file_edit::resolve_path(&resources, &args, file_path) {
        Ok(path) => path,
        Err(err) => {
            // Out-of-workspace edit: escalate to the sandbox-override HITL (same
            // recoverable path reads use). On approval the root joins
            // session_file_sandbox_roots and the retry stages the edit — still gated
            // by the per-write diff-approval + apply-time containment check. A
            // non-sandbox resolve failure (empty/invalid path) stays a soft error.
            if staged_file_edit::is_workspace_root_denied(&err) {
                if crate::config::external_writes_enabled() {
                    return Err(ExecutionError::PathAccessDenied {
                        paths: vec![file_path.to_string()],
                    });
                }
                // Feature OFF (default): keep out-of-workspace edits scoped.
                return Ok(json!({
                    "status": "error",
                    "reason": format!(
                        "{err:#} — edit inside your scoped workspace (out-of-workspace writes are disabled)."
                    ),
                }));
            }
            return Ok(json!({
                "status": "error",
                "reason": format!("Invalid `file_path`: {err:#}"),
            }));
        },
    };
    let original = match staged_file_edit::read_text_bounded(&scoped.absolute_path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Could not read `{file_path}`: {e}"),
            }));
        },
    };

    let occurrences = original.matches(old_string).count();
    if occurrences == 0 {
        return Ok(json!({
            "status": "error",
            "reason": format!("`old_string` not found in `{file_path}`."),
        }));
    }
    if occurrences > 1 && !replace_all {
        return Ok(json!({
            "status": "error",
            "reason": format!(
                "`old_string` matches {occurrences} times in `{file_path}`. Add surrounding context to make the match unique, or set `replace_all: true`."
            ),
            "occurrences": occurrences,
        }));
    }

    let updated = if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };

    // Surface the ABSOLUTE destination + an explicit marker for an approved
    // out-of-workspace edit so the operator sees it on the diff-approval screen.
    let count = if replace_all { occurrences } else { 1 };
    let plural = if (replace_all && occurrences == 1) || (!replace_all) {
        ""
    } else {
        "s"
    };
    let rationale = if scoped.apply_root.is_some() {
        format!(
            "Replace {count} occurrence{plural} in {} [OUTSIDE WORKSPACE — approved external write]",
            scoped.absolute_path.display()
        )
    } else {
        format!(
            "Replace {count} occurrence{plural} in {}",
            scoped.relative_path.display()
        )
    };
    match staged_file_edit::stage_one_edit(
        &resources,
        &scoped,
        rationale,
        ProposedEdit::Modify {
            path: scoped.relative_path.clone(),
            new_content: updated,
        },
    ) {
        Ok(value) => Ok(value),
        Err(e) => Ok(json!({
            "status": "error",
            "reason": format!("Could not stage `{file_path}` for approval: {e:#}"),
        })),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    #[test]
    fn plural_suffix_for_replace_all() {
        let occurrences = 2usize;
        let replace_all = true;
        let suffix = if (replace_all && occurrences == 1) || (!replace_all) {
            ""
        } else {
            "s"
        };
        assert_eq!(suffix, "s");
    }
}
