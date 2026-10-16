//! One-off backfill of planning model usage onto task timelines.
//!
//! A task's planning cost reaches its chat card and task panel through the
//! `stage: "llm"` planning milestones `normalize.rs` writes to the task's
//! `task:{id}` progress shard, one per model call. Tasks planned before those
//! milestones existed have none, although every one of their planning calls
//! is in the LLM call ledger, keyed by the planning execution id
//! (`planexec_…`). This job reads each such execution's totals from the
//! ledger's parquet and appends ONE summary milestone per planning execution, carrying
//! `llm_calls` with the call count.
//!
//! Idempotent: a planning execution whose shard tail already holds an `llm`
//! milestone (live rows, or an earlier backfill) is skipped, and backfilled
//! rows have a deterministic id. A per-scope marker records the executions
//! already handled so a restart does not re-scan them. It runs in-process
//! because the server owns the write lease on progress shards.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use crate::magician_v2::analytics::planning_usage_ledger::{
    planning_usage_by_execution, PlanningUsage,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::progress_channel_seam::{
    event_log::EventLog,
    normalize::base_message,
    types::{ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource},
};

/// Shard tail checked for existing `llm` milestones; matches the panel's
/// planning-timeline window, so a row the panel can see is never doubled.
const SHARD_TAIL: usize = 400;
const MARKER_FILE: &str = ".planning_usage_backfill_v1.json";
/// A planning run with no ledger rows this long after it started never gets
/// any; the ledger materializes calls within minutes.
const SETTLED_AFTER_MS: i64 = 2 * 24 * 60 * 60 * 1_000;

/// One task's planning execution, as found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningExecution {
    pub task_id: String,
    pub execution_id: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Marker {
    done: Vec<String>,
}

/// Spawn the backfill for every scope. Failures are logged; nothing waits on it.
pub fn spawn(workspace: ArtifactV2Workspace, event_log: EventLog) {
    tokio::spawn(async move {
        for (principal, scope_workspace) in workspace.list_scopes() {
            match backfill_scope(&workspace, &event_log, &principal, &scope_workspace).await {
                Ok(0) => {},
                Ok(written) => info!(
                    principal,
                    workspace = scope_workspace,
                    written,
                    "[PLANNING-USAGE-BACKFILL] wrote planning usage for past tasks"
                ),
                Err(error) => warn!(
                    principal,
                    workspace = scope_workspace,
                    %error,
                    "[PLANNING-USAGE-BACKFILL] scope failed; it is retried on the next start"
                ),
            }
        }
    });
}

async fn backfill_scope(
    workspace: &ArtifactV2Workspace,
    event_log: &EventLog,
    principal: &str,
    scope_workspace: &str,
) -> Result<usize> {
    let marker_path = workspace
        .progress_events_dir(principal, scope_workspace)
        .join(MARKER_FILE);
    let mut marker = read_marker(&marker_path).await;
    let done: HashSet<String> = marker.done.iter().cloned().collect();
    let tasks_root = workspace.tasks_root(principal, scope_workspace);
    let executions: Vec<PlanningExecution> =
        tokio::task::spawn_blocking(move || planning_executions(&tasks_root))
            .await?
            .into_iter()
            .filter(|execution| !done.contains(&execution.execution_id))
            .collect();
    if executions.is_empty() {
        return Ok(0);
    }

    let usage = ledger_usage(workspace, principal, scope_workspace, &executions).await?;
    let mut written = 0usize;
    let now_ms = chrono::Utc::now().timestamp_millis();
    for execution in &executions {
        let Some(totals) = usage.get(&execution.execution_id) else {
            // No ledger rows. A recent run may not be materialized yet, so it
            // is retried on a later start; an older one never will be (it
            // predates planning calls in the ledger), so it is marked done.
            if now_ms - execution.created_at_ms > SETTLED_AFTER_MS {
                marker.done.push(execution.execution_id.clone());
            }
            continue;
        };
        let log_key = format!("task:{}", execution.task_id);
        let tail = event_log
            .list_recent(principal, scope_workspace, &log_key, SHARD_TAIL)
            .await
            .unwrap_or_default();
        if !has_llm_milestone(&tail, &execution.execution_id) {
            let message = usage_message(principal, scope_workspace, execution, totals);
            event_log
                .append(&message)
                .await
                .with_context(|| format!("appending planning usage for {}", execution.task_id))?;
            written += 1;
        }
        marker.done.push(execution.execution_id.clone());
    }
    write_marker(&marker_path, &marker).await?;
    Ok(written)
}

/// Every `tasks/<task>/executions/planexec_*` with its creation time.
pub fn planning_executions(tasks_root: &Path) -> Vec<PlanningExecution> {
    let mut found = Vec::new();
    let Ok(tasks) = std::fs::read_dir(tasks_root) else {
        return found;
    };
    for task in tasks.flatten() {
        let task_id = task.file_name().to_string_lossy().to_string();
        if !task_id.starts_with("task_") {
            continue;
        }
        let Ok(executions) = std::fs::read_dir(task.path().join("executions")) else {
            continue;
        };
        for execution in executions.flatten() {
            let execution_id = execution.file_name().to_string_lossy().to_string();
            if !execution_id.starts_with("planexec_") {
                continue;
            }
            let created_at_ms = std::fs::read(execution.path().join("execution.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|record| {
                    record
                        .pointer("/execution/created_at")
                        .and_then(Value::as_i64)
                });
            let Some(created_at_ms) = created_at_ms else {
                continue;
            };
            found.push(PlanningExecution {
                task_id: task_id.clone(),
                execution_id,
                created_at_ms,
            });
        }
    }
    found
}

/// Ledger totals for these executions (see
/// `analytics::planning_usage_ledger`), off the async runtime.
async fn ledger_usage(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope_workspace: &str,
    executions: &[PlanningExecution],
) -> Result<BTreeMap<String, PlanningUsage>> {
    let calls_root = workspace.analytics_llm_calls_root(principal, scope_workspace);
    let days = partition_days(executions);
    let wanted: HashSet<String> = executions.iter().map(|e| e.execution_id.clone()).collect();
    tokio::task::spawn_blocking(move || planning_usage_by_execution(calls_root, days, wanted))
        .await?
}

/// The UTC partition days (`YYYY-MM-DD`) worth reading: each execution's
/// creation day and the day after, since planning can cross midnight.
pub fn partition_days(executions: &[PlanningExecution]) -> Vec<String> {
    let mut days = std::collections::BTreeSet::new();
    for execution in executions {
        for offset in [0_i64, 1] {
            let ms = execution.created_at_ms + offset * 24 * 60 * 60 * 1_000;
            if let Some(at) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms) {
                days.insert(at.format("%Y-%m-%d").to_string());
            }
        }
    }
    days.into_iter().collect()
}

/// Whether the shard tail already carries model usage for this execution.
pub fn has_llm_milestone(tail: &[ProgressMessage], execution_id: &str) -> bool {
    tail.iter().any(|message| match &message.kind {
        ProgressMessageKind::AgentNotification { metadata, .. } => {
            metadata.get("planning_stage").and_then(Value::as_str) == Some("llm")
                && metadata.get("execution_id").and_then(Value::as_str) == Some(execution_id)
        },
        _ => false,
    })
}

/// The summary milestone: shaped like a live `llm` planning milestone, with
/// `llm_calls` so readers count every call it stands for.
pub fn usage_message(
    principal: &str,
    scope_workspace: &str,
    execution: &PlanningExecution,
    usage: &PlanningUsage,
) -> ProgressMessage {
    let tokens = usage.input_tokens + usage.output_tokens;
    let cost = usage
        .cost_usd
        .map(|cost| format!(" · ${cost:.3}"))
        .unwrap_or_default();
    let timestamp = usage.last_call_ms.max(execution.created_at_ms);
    let mut message = base_message(
        ProgressSource::Execution,
        principal.to_string(),
        scope_workspace.to_string(),
        Some(execution.execution_id.clone()),
        Some(execution.task_id.clone()),
        Some(execution.task_id.clone()),
        Some(execution.execution_id.clone()),
        None,
        None,
        None,
        None,
        ProgressSeverity::Info,
        ProgressMessageKind::AgentNotification {
            event_type: "execution.progress".to_string(),
            message: format!(
                "Planning model usage: {} call{} · {} tok{cost}",
                usage.calls,
                if usage.calls == 1 { "" } else { "s" },
                tokens
            ),
            entity_key: Some(format!("planning:{}:llm:backfill", execution.execution_id)),
            metadata: serde_json::json!({
                "execution_id": execution.execution_id,
                "planning_stage": "llm",
                "llm_calls": usage.calls,
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cost_usd": usage.cost_usd,
                "backfilled": true,
            }),
        },
        timestamp,
    );
    message.id = format!("planning-usage-backfill:{}", execution.execution_id);
    message
}

async fn read_marker(path: &Path) -> Marker {
    tokio::fs::read(path)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

async fn write_marker(path: &Path, marker: &Marker) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    crate::magician_v2::artifact_v2::io::write_bytes_atomic(path, &serde_json::to_vec(marker)?)
        .await
        .map_err(|error| anyhow::anyhow!("writing planning-usage backfill marker: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execution() -> PlanningExecution {
        PlanningExecution {
            task_id: "task_1".to_string(),
            execution_id: "planexec_1".to_string(),
            created_at_ms: 1_000,
        }
    }

    #[test]
    fn summary_reads_like_a_live_llm_milestone_with_a_call_count() {
        let usage = PlanningUsage {
            calls: 40,
            input_tokens: 90_000,
            output_tokens: 6_000,
            cost_usd: Some(0.2682),
            last_call_ms: 5_000,
        };
        let message = usage_message("anonymous", "default", &execution(), &usage);
        assert_eq!(message.log_key, "task:task_1");
        assert_eq!(message.id, "planning-usage-backfill:planexec_1");
        assert_eq!(message.timestamp, 5_000);
        let ProgressMessageKind::AgentNotification {
            metadata,
            message: text,
            ..
        } = &message.kind
        else {
            panic!("expected a notification");
        };
        assert_eq!(metadata["planning_stage"], "llm");
        assert_eq!(metadata["llm_calls"], 40);
        assert_eq!(metadata["cost_usd"], 0.2682);
        assert!(text.contains("40 calls"), "{text}");
        assert!(has_llm_milestone(
            std::slice::from_ref(&message),
            "planexec_1"
        ));
        assert!(!has_llm_milestone(
            std::slice::from_ref(&message),
            "planexec_2"
        ));
    }

    #[test]
    fn reads_the_creation_day_and_the_next() {
        // 2026-10-01T23:30:00Z
        let late = PlanningExecution {
            created_at_ms: 1_790_897_400_000,
            ..execution()
        };
        assert_eq!(partition_days(&[late]), vec!["2026-10-01", "2026-10-02"]);
    }

    #[test]
    fn finds_planning_executions_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let exec = tmp.path().join("task_a/executions/planexec_x");
        std::fs::create_dir_all(&exec).unwrap();
        std::fs::write(
            exec.join("execution.json"),
            br#"{"execution":{"created_at":42}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("task_a/executions/exec_run")).unwrap();
        let found = planning_executions(tmp.path());
        assert_eq!(
            found,
            vec![PlanningExecution {
                task_id: "task_a".to_string(),
                execution_id: "planexec_x".to_string(),
                created_at_ms: 42,
            }]
        );
    }
}
