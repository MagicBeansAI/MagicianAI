//! `grep` — content search via ripgrep.
//!
//! Shells out to `rg` with output-mode-aware flags. Returns matching
//! lines / file paths / per-file counts depending on `output_mode`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

const DEFAULT_HEAD_LIMIT: u64 = 250;

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "grep requires `pattern` (regex string).",
        }));
    };
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string();
    let glob = args.get("glob").and_then(Value::as_str);
    let output_mode = args
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("files_with_matches");
    let case_insensitive = args
        .get("case_insensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let context = args.get("context").and_then(Value::as_u64);
    let head_limit = args
        .get("head_limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_HEAD_LIMIT);
    let type_filter = args.get("type").and_then(Value::as_str);

    let mut cmd = tokio::process::Command::new("rg");
    match output_mode {
        "files_with_matches" => {
            cmd.arg("--files-with-matches");
        },
        "count" => {
            cmd.arg("--count");
        },
        "content" => {
            cmd.arg("-n");
            if let Some(c) = context {
                cmd.arg(format!("-C{c}"));
            }
        },
        other => {
            return Ok(json!({
                "status": "error",
                "reason": format!(
                    "grep `output_mode` `{other}` is invalid. Use one of: content | files_with_matches | count."
                ),
            }));
        },
    }
    if case_insensitive {
        cmd.arg("-i");
    }
    if let Some(g) = glob {
        cmd.arg("--glob").arg(g);
    }
    if let Some(t) = type_filter {
        cmd.arg("--type").arg(t);
    }
    cmd.arg(pattern).arg(&path);

    let output = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to spawn ripgrep: {e}"),
            }));
        },
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut lines: Vec<String> = stdout.lines().map(str::to_string).collect();
    let total = lines.len();
    let truncated = (total as u64) > head_limit;
    if truncated {
        lines.truncate(head_limit as usize);
    }

    Ok(json!({
        "status": "ok",
        "pattern": pattern,
        "path": path,
        "output_mode": output_mode,
        "total_matches": total,
        "result_count": lines.len(),
        "results": lines,
        "truncated": truncated,
    }))
}
