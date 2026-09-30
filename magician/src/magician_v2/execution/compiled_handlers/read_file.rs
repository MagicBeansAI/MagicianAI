//! `read_file` — read a file from the local filesystem.
//!
//! Thin wrapper that builds a `FileAction::Read` and dispatches it
//! through `execute_file_action` with the per-scope `FileSandboxConfig`
//! on `AgentResources`. Same dispatch path as the multi-action `files`
//! pack (`action: "read"`), exposed as a named single-purpose tool
//! for ergonomics.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::actions::{ActionResult, FileAction};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::native_executors::execute_file_action;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(file_path) = args.get("file_path").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "read_file requires `file_path` (absolute path string).",
        }));
    };
    let encoding = args
        .get("encoding")
        .and_then(Value::as_str)
        .map(str::to_string);

    let action = FileAction::Read {
        path: PathBuf::from(file_path),
        encoding,
    };

    // P0.4 — widen the boot sandbox with any roots the operator approved this
    // session via the sandbox-override HITL, so a previously-denied path is
    // readable on the retry (mirrors the native `files` pack merge at the
    // `ExecutableAction::File` gate). `None` outside a scoped dispatch → the
    // boot `file_sandbox` is used unchanged.
    let mut effective_sandbox = resources.file_sandbox.clone();
    if let Some(roots) =
        crate::magician_v2::execution::compiled_dispatch::current_session_file_sandbox_roots()
    {
        if let Ok(guard) = roots.lock() {
            effective_sandbox
                .allowed_roots
                .extend(guard.iter().cloned());
        }
    }

    match execute_file_action(&action, &effective_sandbox).await {
        Ok(ActionResult::Text { content }) => Ok(json!({
            "status": "ok",
            "file_path": file_path,
            "content": content,
            "byte_count": content.len(),
        })),
        Ok(other) => Ok(json!({
            "status": "ok",
            "file_path": file_path,
            "result": format!("{other:?}"),
        })),
        // Propagate a pure out-of-roots denial so the executor chokepoint can
        // raise the sandbox-override HITL (approve a folder → retry). Swallowing
        // it into a soft `{status:error}` here would hide the recoverable case
        // from the escalation path and hard-fail the read for the LLM instead.
        Err(e @ ExecutionError::PathAccessDenied { .. }) => Err(e),
        Err(e) => Ok(json!({
            "status": "error",
            "reason": format!("{e}"),
        })),
    }
}
