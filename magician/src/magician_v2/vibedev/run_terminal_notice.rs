//! Tell the owner when a VibeDev run they are probably not watching ends.
//!
//! Until this existed, an unattended run's report-back was the agent's choice:
//! the Autopilot directive asks it to call `notify_owner` once at the end, and
//! a run that crashed, was stopped, or simply forgot never told anyone. The
//! server now files that notice itself when the run's task reaches a terminal
//! status. It goes through the same owner-briefing request `notify_owner`
//! files, so it lands in Attention and on the owner's devices exactly as the
//! agent's would.
//!
//! Which runs: every Autopilot run, however it ends (the owner said they are
//! walking away), and any other run that **fails**, since that one needs the
//! owner. A build or Discuss run that finishes or is stopped stays quiet: the
//! owner started it and is usually watching or about to follow up, and a
//! notice for every such run buried the ones that mattered. A run whose agent
//! already filed its own `notify_owner` report is left alone, so the owner
//! never gets two.

use serde_json::json;

use crate::magician_v2::artifact_v2::models::{is_vibedev_cockpit_run, TaskRecord};
use crate::magician_v2::user_requests::{RequestOption, UserRequest, UserRequestService};

use super::run_service::VIBEDEV_RUN_AUTOPILOT_TAG;

const NOTIFY_OWNER_REQUEST_PREFIX: &str = "harness.notify_owner.";
const BRIEFING_REQUEST_TYPE: &str = "harness.notify_owner.briefing";
/// How far back the owner-report check looks. A run's own report is recent.
const OWNER_REPORT_LOOKBACK: usize = 500;

/// What the owner is told, decided from the task alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTerminalNotice {
    pub request_id: String,
    pub title: String,
    pub message: String,
    pub severity: &'static str,
}

/// The notice for `task`, or `None` when this run should not notify.
pub fn run_terminal_notice(task: &TaskRecord) -> Option<RunTerminalNotice> {
    let manifest = &task.manifest;
    if !is_vibedev_cockpit_run(&manifest.ui_thread_id, &manifest.tags) {
        return None;
    }
    // A crew's lanes report through the crew's one settled notice
    // (`vibedev::crews`), not one notice per lane.
    if manifest.created_by == super::crews::CREW_LANE_CREATED_BY {
        return None;
    }
    let (verb, severity, follow) = match task.state.status.as_str() {
        "completed" => ("finished", "info", "Open it to review the result."),
        "failed" => ("failed", "warning", "Open it to see what went wrong."),
        "cancelled" => ("was stopped", "info", "Open it to see where it got to."),
        _ => return None,
    };
    let autopilot = manifest
        .tags
        .iter()
        .any(|tag| tag.name.eq_ignore_ascii_case(VIBEDEV_RUN_AUTOPILOT_TAG));
    if !autopilot && task.state.status != "failed" {
        return None;
    }
    let title = one_line(run_title_without_prefix(&manifest.title), 96);
    let kind = if autopilot {
        "Autopilot run"
    } else {
        "VibeDev run"
    };
    // Keyed on the run's root execution, so a re-assert of the same terminal
    // status replays the same request instead of filing a second one, while a
    // later continuation of the task can still report its own end.
    let execution = task
        .state
        .latest_root_execution_id
        .as_deref()
        .unwrap_or("run");
    Some(RunTerminalNotice {
        request_id: format!("vibedev-run-ended:{}:{}", manifest.task_id, execution),
        title: format!("{kind} {verb}: {title}"),
        // No timestamps or counters: an exact replay must carry identical
        // content, or the request service refuses it as a conflict.
        message: format!("{kind} {verb}: {title}. {follow}"),
        severity,
    })
}

/// File the notice, unless the run's agent already reported on its own.
/// Best-effort: a notice that cannot be filed is logged, never fatal.
pub async fn notify_owner_of_ended_run(
    requests: &UserRequestService,
    principal: &str,
    workspace: &str,
    task: &TaskRecord,
) {
    let Some(notice) = run_terminal_notice(task) else {
        return;
    };
    let task_id = task.manifest.task_id.as_str();
    let already_reported = requests
        .list_history_for_scope(principal, workspace, Some(OWNER_REPORT_LOOKBACK))
        .into_iter()
        .any(|record| {
            record.request.task_id.as_deref() == Some(task_id)
                && record
                    .request
                    .request_type
                    .starts_with(NOTIFY_OWNER_REQUEST_PREFIX)
                && record.request.id != notice.request_id
        });
    if already_reported {
        return;
    }
    let request = UserRequest {
        id: notice.request_id.clone(),
        request_type: BRIEFING_REQUEST_TYPE.to_string(),
        question: format!("Owner briefing: {}\n\n{}", notice.title, notice.message),
        options: vec![RequestOption {
            id: "acknowledge".to_string(),
            label: "Acknowledge".to_string(),
            requires_input: false,
        }],
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        context: json!({
            "owner_agent_id": "vibedev",
            "title": notice.title,
            "message": notice.message,
            "kind": "briefing",
            "severity": notice.severity,
            "source": "vibedev_run_ended",
        }),
        source: "harness".to_string(),
        execution_id: task.state.latest_root_execution_id.clone(),
        task_id: Some(task_id.to_string()),
        timeout_secs: 86_400,
        default_on_timeout: "acknowledge".to_string(),
        created_at: 0,
        sensitive: None,
    };
    if let Err(error) = requests.submit_nonblocking(request).await {
        tracing::warn!(
            task_id,
            %error,
            "could not tell the owner a VibeDev run ended"
        );
    }
}

/// Run titles carry a `VibeDev · ` (or `VibeDev follow-up · `) prefix for the
/// task list. The notice already says which kind of run it was.
fn run_title_without_prefix(title: &str) -> &str {
    let trimmed = title.trim_start();
    for prefix in ["VibeDev follow-up · ", "VibeDev · "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest;
        }
    }
    trimmed
}

fn one_line(value: &str, max_chars: usize) -> String {
    let line = value.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::TaskTagRecord;

    fn tag(name: &str) -> TaskTagRecord {
        TaskTagRecord {
            id: name.to_string(),
            name: name.to_string(),
            color: None,
        }
    }

    fn run(status: &str, tags: &[&str], minutes_ago: i64) -> TaskRecord {
        let now = chrono::Utc::now();
        let mut task: TaskRecord = serde_json::from_value(json!({
            "manifest": {
                "task_id": "task-1",
                "principal": "owner",
                "workspace": "default",
                "description": "VibeDev coding request:",
                "agent_id": "main",
                "title": "Fix the footer spacing\nand more",
                "ui_thread_id": "vibedev",
                "created_at": (now - chrono::Duration::minutes(minutes_ago)).to_rfc3339(),
                "updated_at": now.to_rfc3339(),
            },
            "state": {"task_id": "task-1", "status": status, "updated_at": now.to_rfc3339()},
            "refs": {"task_id": "task-1", "outputs": []},
        }))
        .expect("a minimal task record");
        task.manifest.tags = tags.iter().map(|name| tag(name)).collect();
        task.state.latest_root_execution_id = Some("exec-1".to_string());
        task
    }

    #[test]
    fn an_autopilot_run_always_notifies_when_it_ends() {
        let task = run("completed", &["vibedev", "autopilot"], 1);
        let notice = run_terminal_notice(&task).expect("a notice");
        assert_eq!(
            notice.title,
            "Autopilot run finished: Fix the footer spacing"
        );
        assert_eq!(notice.severity, "info");
        assert_eq!(notice.request_id, "vibedev-run-ended:task-1:exec-1");
    }

    #[test]
    fn a_build_run_notifies_only_when_it_fails_however_long_it_ran() {
        for status in ["completed", "cancelled"] {
            let long = run(status, &["vibedev"], 90);
            assert_eq!(run_terminal_notice(&long), None, "{status}");
        }
        let failed = run("failed", &["vibedev"], 2);
        let notice = run_terminal_notice(&failed).expect("a notice");
        assert_eq!(notice.title, "VibeDev run failed: Fix the footer spacing");
        assert_eq!(notice.severity, "warning");
    }

    #[test]
    fn a_crew_lane_leaves_its_notice_to_the_crew() {
        let mut lane = run("failed", &["vibedev"], 2);
        lane.manifest.created_by =
            crate::magician_v2::vibedev::crews::CREW_LANE_CREATED_BY.to_string();
        assert_eq!(run_terminal_notice(&lane), None);
    }

    #[test]
    fn only_terminal_vibedev_runs_notify() {
        let running = run("running", &["vibedev", "autopilot"], 60);
        assert_eq!(run_terminal_notice(&running), None);
        let mut other = run("completed", &["autopilot"], 60);
        other.manifest.ui_thread_id = "chat".to_string();
        assert_eq!(run_terminal_notice(&other), None);
    }

    #[test]
    fn the_task_list_prefix_is_dropped_from_the_notice_title() {
        let mut task = run("cancelled", &["vibedev", "autopilot"], 1);
        task.manifest.title = "VibeDev · Create stop.py".to_string();
        let notice = run_terminal_notice(&task).expect("a notice");
        assert_eq!(notice.title, "Autopilot run was stopped: Create stop.py");
    }

    #[test]
    fn a_replayed_terminal_status_produces_the_identical_notice() {
        let task = run("cancelled", &["vibedev", "autopilot"], 1);
        let mut reasserted = task.clone();
        reasserted.manifest.updated_at = "2099-01-01T00:00:00Z".to_string();
        reasserted.state.updated_at = "2099-01-01T00:00:00Z".to_string();
        assert_eq!(run_terminal_notice(&task), run_terminal_notice(&reasserted));
    }
}
