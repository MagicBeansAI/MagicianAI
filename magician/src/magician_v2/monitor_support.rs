//! Monitor schedule helpers and lifecycle-event recording shared by the
//! monitors HTTP surface and lib-side storage/execution callers. Extracted
//! from `api::monitors_api` (api-crate extraction prerequisite).

use std::sync::Arc;

use serde_json::json;

use crate::magician_v2::artifact_v2::models::TaskRecord;
use crate::magician_v2::artifact_v2::service::ArtifactV2Error;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef};
use crate::magician_v2::artifact_v2::{CreateTaskInput, UpdateTaskInput};
use crate::magician_v2::monitors::monitor_spec::MonitorSpecV1;
use crate::magician_v2::storage::{TaskSchedule, TaskScheduleKind};

pub fn parsed_schedule(schedule: Option<&serde_json::Value>) -> Option<TaskSchedule> {
    schedule.and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub fn schedule_is_paused(schedule: Option<&TaskSchedule>) -> bool {
    schedule
        .map(|schedule| schedule.paused == Some(true))
        .unwrap_or(false)
}

/// §12 `monitor_updated` — shared by `PATCH /monitors/{task_id}` and the
/// `update_monitor` chat tool (both call `update_task` directly; this is
/// their one shared event seam). The seed carries the post-edit revision +
/// manifest timestamp, so each edit action traces once and identical
/// replays of the same persisted state add nothing.
pub async fn record_monitor_updated_event(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    task: &TaskRecord,
) {
    service
        .record_monitor_lifecycle_event(
            scope,
            "monitor_updated",
            &task.manifest.task_id,
            &format!(
                "{}\u{1f}{}\u{1f}{}",
                task.manifest.task_id, task.manifest.monitor_revision, task.manifest.updated_at
            ),
            json!({
                "monitor_revision": task.manifest.monitor_revision,
                "updated_at": task.manifest.updated_at,
            }),
        )
        .await;
}

/// Same default the generic create-task handler falls back to. Monitors are
/// never VibeDev coding runs, so the configured coding-lead override in
/// `create_task_v3_handler` can never apply here — the fallback is the whole
/// resolution. `pub` so the Phase 4 chat tools resolve the identical
/// default.
pub const DEFAULT_MONITOR_AGENT_ID: &str = "personal-assistant";

pub fn monitor_state_label(paused: bool) -> &'static str {
    if paused {
        "paused"
    } else {
        "active"
    }
}

/// Human cadence string from the parsed `Task.schedule` kind. `unscheduled`
/// when the task has no (parseable) schedule — a monitor can exist before
/// its cadence is set. `pub` so the chat preview/create tools render
/// the IDENTICAL summary the list rows show.
pub fn cadence_summary(schedule: Option<&TaskSchedule>) -> String {
    let Some(schedule) = schedule else {
        return "unscheduled".to_string();
    };
    match &schedule.kind {
        TaskScheduleKind::Cron {
            expression,
            timezone,
        } => {
            let timezone = timezone
                .clone()
                .or_else(|| schedule.timezone.clone())
                .filter(|value| !value.trim().is_empty());
            match timezone {
                Some(timezone) => format!("Cron {expression} ({timezone})"),
                None => format!("Cron {expression}"),
            }
        },
        TaskScheduleKind::Interval { seconds, .. } => format!("Every {seconds}s"),
        TaskScheduleKind::Once { at } => format!("Once at {}", at.to_rfc3339()),
        TaskScheduleKind::OnEvent { event_pattern } => format!("On event {event_pattern}"),
    }
}

/// Derive a task title from the spec objective when the caller supplied
/// none — the same truncation the HTTP create path has always used.
pub fn monitor_title_from(title: Option<String>, spec: &MonitorSpecV1) -> String {
    title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            spec.objective
                .chars()
                .take(MONITOR_TITLE_FROM_OBJECTIVE_CHARS)
                .collect()
        })
}

/// The ONE create service path (plan §8): mint a persistent task through the
/// canonical `create_task`, then attach the ALREADY-VALIDATED spec through
/// `update_task` so the server-owned `monitor_revision` starts at 1. Shared
/// by `create_monitor_v3_handler` (HTTP) and the `create_monitor` chat tool —
/// callers must run `validate_and_normalize` first.
pub async fn create_monitor_task(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    title: Option<String>,
    spec: MonitorSpecV1,
    schedule: Option<serde_json::Value>,
    agent_id: String,
) -> std::result::Result<TaskRecord, ArtifactV2Error> {
    let title = monitor_title_from(title, &spec);
    let task = service
        .create_task(CreateTaskInput {
            principal: scope.principal().to_string(),
            workspace: scope.workspace().to_string(),
            title,
            description: spec.objective.clone(),
            agent_id,
            goal_id: None,
            ui_thread_id: crate::magician_v2::artifact_v2::models::default_ui_thread_id(),
            priority: None,
            due_date: None,
            // The reserved monitor tag is a read-time projection — nothing
            // monitor-shaped is ever persisted in tags (plan §6.1).
            tags: Vec::new(),
            created_by: "user".to_string(),
            depends_on: Vec::new(),
            approved: true,
            schedule,
            output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::default(),
            chat_session_id: None,
            lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
            sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
        })
        .await?;
    let created = service
        .update_task(
            scope,
            &task.manifest.task_id,
            UpdateTaskInput {
                monitor_spec: Some(spec),
                ..Default::default()
            },
        )
        .await?;
    // §12 `monitor_created` — this is the ONE create service path, shared by
    // the HTTP route and the `create_monitor` chat tool, so the trace fires
    // exactly once per created monitor (deterministic seed = the task id).
    service
        .record_monitor_lifecycle_event(
            scope,
            "monitor_created",
            &created.manifest.task_id,
            &created.manifest.task_id,
            json!({
                "monitor_revision": created.manifest.monitor_revision,
                "has_schedule": created.manifest.schedule.is_some(),
                "agent_id": created.manifest.agent_id,
            }),
        )
        .await;
    Ok(created)
}

pub const MONITOR_TITLE_FROM_OBJECTIVE_CHARS: usize = 80;
