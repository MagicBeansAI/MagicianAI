//! `list_scheduled_tasks` — list the scheduled tasks in the calling
//! scope.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "list_scheduled_tasks")?;
    let workspace = require_scope_str(&args, "__workspace", "list_scheduled_tasks")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };

    match service.list_scheduled_tasks_across_scopes().await {
        Ok(scheduled) => {
            let filtered: Vec<Value> = scheduled
                .into_iter()
                .filter(|entry| {
                    entry.scope.principal() == principal && entry.scope.workspace() == workspace
                })
                .map(|entry| {
                    let schedule =
                        serde_json::to_value(&entry.schedule_kind).unwrap_or(Value::Null);
                    let (cron, at) = match &entry.schedule_kind {
                        crate::magician_v2::storage::TaskScheduleKind::Cron {
                            expression, ..
                        } => (Some(expression.clone()), None),
                        crate::magician_v2::storage::TaskScheduleKind::Once { at } => {
                            (None, Some(at.to_rfc3339()))
                        },
                        _ => (None, None),
                    };
                    json!({
                        "task_id": entry.task_id,
                        "schedule": schedule,
                        "cron": cron,
                        "at": at,
                    })
                })
                .collect();
            Ok(json!({
                "status": "ok",
                "count": filtered.len(),
                "scheduled_tasks": filtered,
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("list_scheduled_tasks failed: {error}"),
        })),
    }
}
