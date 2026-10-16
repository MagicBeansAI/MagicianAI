//! `glob` — file pattern matching via ripgrep.
//!
//! Shells out to `rg --files --glob <pattern>` to enumerate paths
//! matching the glob. Caps results at 100 entries to keep history
//! lean; refine `pattern` or `path` to drill in.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

const MAX_RESULTS: usize = 100;

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "glob requires `pattern` (glob string).",
        }));
    };
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string();

    let output = match tokio::process::Command::new("rg")
        .arg("--files")
        .arg("--glob")
        .arg(pattern)
        .arg(&path)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to spawn ripgrep: {e}"),
            }));
        },
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut filenames: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let total = filenames.len();
    let truncated = total > MAX_RESULTS;
    if truncated {
        filenames.truncate(MAX_RESULTS);
    }

    Ok(json!({
        "status": "ok",
        "pattern": pattern,
        "path": path,
        "num_files": filenames.len(),
        "total_matches": total,
        "filenames": filenames,
        "truncated": truncated,
    }))
}
