//! Platform faults end the run on their first occurrence.
//!
//! `run_coding_task` marks a setup failure the model cannot fix —
//! Magician's own folders, an engine install, an adapter, a missing
//! credential, or a project repository a clone cannot use — with
//! `"fault": "platform"` in its result. Retrying or re-planning cannot fix
//! it, and a model left to iterate on one burns its budget and then starts
//! debugging Magician itself. So the loop ends the run failed, with an
//! owner-facing summary, the first time it sees one.
//!
//! Only that handler's own result counts: an HTTP body, a file or another
//! tool's output saying `"fault": "platform"` is data, never a reason to end
//! a run. A nested segment (an in-context delegate) that ends on one records
//! it for its execution, and the parent ends on that record in turn, so the
//! fault reaches the owner instead of being delegated again.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::magician_v2::execution::actions::ExecutableAction;

/// The opening of a platform-fault summary on Magician's side.
pub(crate) const PLATFORM_FAULT_SUMMARY_PREFIX: &str =
    "Magician couldn't run this because of a problem on its side, not with your request:";

/// The opening when the project's repository is what the owner fixes
/// (`"fault_scope": "project"`).
pub(crate) const PROJECT_FAULT_SUMMARY_PREFIX: &str =
    "Magician couldn't start work in the project's repository:";

/// The owner-facing summary when `payload` (a tool's JSON result text)
/// reports a platform fault at its top level; `None` otherwise. Callers go
/// through [`coding_platform_fault_summary`], which admits only the result of
/// the handler that sets the marker.
pub(crate) fn platform_fault_summary(payload: &str) -> Option<String> {
    let trimmed = payload.trim_start();
    if !trimmed.starts_with('{') || !trimmed.contains("\"fault\"") {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    if value.get("fault").and_then(Value::as_str) != Some("platform") {
        return None;
    }
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let opening = if text("fault_scope") == Some("project") {
        PROJECT_FAULT_SUMMARY_PREFIX
    } else {
        PLATFORM_FAULT_SUMMARY_PREFIX
    };
    let reason = text("reason").unwrap_or("a setup step failed");
    let mut summary = format!("{opening} {}", reason.trim_end_matches('.'));
    summary.push('.');
    if let Some(fix) = text("fix") {
        summary.push(' ');
        summary.push_str(fix);
    }
    Some(summary)
}

/// [`platform_fault_summary`] for the result of `action`, honoured only when
/// `action` is a `run_coding_task` call — the one handler that writes the
/// marker. Every other tool's output is data.
pub(crate) fn coding_platform_fault_summary(
    action: &ExecutableAction,
    payload: &str,
) -> Option<String> {
    match action {
        ExecutableAction::Pack {
            capability_name, ..
        } if capability_name
            == crate::magician_v2::execution::compiled_handlers::run_coding_task::TOOL_NAME =>
        {
            platform_fault_summary(payload)
        },
        _ => None,
    }
}

fn nested_faults() -> &'static Mutex<HashMap<String, String>> {
    static FAULTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    FAULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that a nested segment of `execution_id` ended on a platform fault
/// with `summary`, for its parent to take ([`take_nested_platform_fault`]).
pub(crate) fn record_nested_platform_fault(execution_id: &str, summary: &str) {
    nested_faults()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(execution_id.to_string(), summary.to_string());
}

/// The platform fault a nested segment of `execution_id` ended on, taken so
/// it is acted on once.
pub(crate) fn take_nested_platform_fault(execution_id: &str) -> Option<String> {
    nested_faults()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(execution_id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::magician_v2::execution::capability::ImplementationType;

    fn pack(capability: &str) -> ExecutableAction {
        ExecutableAction::Pack {
            capability_name: capability.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: capability.to_string(),
            },
            resolved_params: HashMap::new(),
        }
    }

    fn faulted() -> String {
        json!({
            "status": "error",
            "stage": "run_turn",
            "fault": "platform",
            "reason": "Pi coding task failed: unsupported Pi coding agent version 1.2.0.",
            "fix": "Run `make setup-pi-coding-agent`.",
        })
        .to_string()
    }

    #[test]
    fn a_coding_platform_fault_becomes_the_owner_summary() {
        let summary =
            coding_platform_fault_summary(&pack("run_coding_task"), &faulted()).expect("fault");
        assert_eq!(
            summary,
            "Magician couldn't run this because of a problem on its side, not with your \
             request: Pi coding task failed: unsupported Pi coding agent version 1.2.0. Run \
             `make setup-pi-coding-agent`."
        );
        let project = json!({
            "status": "error",
            "fault": "platform",
            "fault_scope": "project",
            "reason": "git clone failed",
            "fix": "Fix the repository.",
        })
        .to_string();
        assert_eq!(
            coding_platform_fault_summary(&pack("run_coding_task"), &project).as_deref(),
            Some(
                "Magician couldn't start work in the project's repository: git clone failed. \
                 Fix the repository."
            )
        );
    }

    /// An HTTP body, a file read or any other tool's output that happens to
    /// say `"fault": "platform"` never ends a run.
    #[test]
    fn only_the_coding_handler_result_is_honoured() {
        for action in [
            pack("http_request"),
            pack("read_file"),
            pack("metabase-mcp__call_tool"),
        ] {
            assert_eq!(coding_platform_fault_summary(&action, &faulted()), None);
        }
    }

    #[test]
    fn an_ordinary_error_is_not_a_platform_fault() {
        for payload in [
            json!({"status": "error", "reason": "repo_path `x` is not an existing directory"})
                .to_string(),
            json!({"status": "error", "fault": "task", "reason": "x"}).to_string(),
            // Only the top level counts.
            json!({"status": "ok", "result": {"fault": "platform"}}).to_string(),
            "plain text mentioning \"fault\"".to_string(),
        ] {
            assert_eq!(
                coding_platform_fault_summary(&pack("run_coding_task"), &payload),
                None,
                "{payload}"
            );
        }
    }

    #[test]
    fn a_nested_fault_is_taken_once_by_its_own_execution() {
        record_nested_platform_fault("exec-nested-fault", "summary");
        assert_eq!(take_nested_platform_fault("exec-other"), None);
        assert_eq!(
            take_nested_platform_fault("exec-nested-fault").as_deref(),
            Some("summary")
        );
        assert_eq!(take_nested_platform_fault("exec-nested-fault"), None);
    }

    /// The direct path ends the run on the first platform fault: the check
    /// sits before the result is matched, so no retry, steer or further
    /// decision follows it; a result without one goes on to the ordinary
    /// handling. An in-context delegate's platform fault ends its parent.
    #[test]
    fn the_direct_path_ends_on_a_platform_fault_before_handling_the_result() {
        let source = include_str!("executor.rs");
        let tail = source
            .split("\nfn execute_direct_path_post_setup_tail<'a>(")
            .nth(1)
            .expect("direct-path tail");
        let check = tail
            .find("super::platform_fault::coding_platform_fault_summary")
            .expect("platform-fault check");
        let matched = tail.find("        match &result {").expect("result match");
        assert!(check < matched, "the check must run before the result is handled");
        assert!(tail[check..matched].contains("return Ok(end_on_platform_fault("));
        let delegate = source
            .split("\npub(super) async fn handle_delegate_to_agent_decision(")
            .nth(1)
            .expect("delegate decision");
        assert!(delegate.contains("super::platform_fault::take_nested_platform_fault("));
    }
}
