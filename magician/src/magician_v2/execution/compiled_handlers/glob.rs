//! `glob` — file pattern matching via ripgrep.
//!
//! Shells out to `rg --files --glob <pattern>` to enumerate paths
//! matching the glob. Caps results at 100 entries to keep history
//! lean; refine `pattern` or `path` to drill in.
//!
//! Fenced like `grep`: the root is checked as a read, `rg` runs in the OS
//! box when one works here, and a path the acting agent may not read is
//! dropped.

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

const MAX_RESULTS: usize = 100;

/// `rg --files --null` output with the paths the agent may not read removed.
fn admitted_files(stdout: &str) -> Vec<String> {
    let mut cache = HashMap::new();
    stdout
        .split('\0')
        .map(|path| path.trim_matches('\n'))
        .filter(|path| !path.is_empty() && super::grep::admit(path, &mut cache))
        .map(str::to_string)
        .collect()
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
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

    if let Some(refused) = super::grep::check_search_root(&resources, &path).await? {
        return Ok(refused);
    }
    let rg_args: Vec<OsString> = vec![
        "--files".into(),
        "--null".into(),
        "--glob".into(),
        pattern.into(),
        "--".into(),
        path.clone().into(),
    ];
    let output = match super::grep::ripgrep(&rg_args).output().await {
        Ok(o) => o,
        Err(e) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to spawn ripgrep: {e}"),
            }));
        },
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut filenames = admitted_files(&stdout);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_drops_paths_in_a_workspace_the_agent_is_not_a_member_of() {
        let fx = crate::magician_v2::execution::goal_workspace_access::test_fixture();
        let goal = fx.root.join("Magican/Goals/X/x.md").display().to_string();
        let secret = fx
            .root
            .join("Magican/Goals/X/secret.md")
            .display()
            .to_string();
        let open = fx
            .root
            .parent()
            .unwrap()
            .join("open.md")
            .display()
            .to_string();
        let stdout = format!("{goal}\0{secret}\0{open}\0");
        crate::magician_v2::execution::goal_workspace_access::with_test_roots(
            vec![fx.root.clone()],
            || {
                let outsider = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("ambassador".to_string()), || admitted_files(&stdout));
                assert_eq!(outsider, vec![open.clone()]);
                let member = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("cmo".to_string()), || admitted_files(&stdout));
                assert_eq!(member, vec![goal.clone(), open.clone()]);
            },
        );
    }
}
