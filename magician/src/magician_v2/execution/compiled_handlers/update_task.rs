//! `update_task` — mutate task fields (title / description / priority
//! / due_date / tags / schedule).

use std::sync::Arc;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "update_task")?;
    let workspace = require_scope_str(&args, "__workspace", "update_task")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };

    let Some(task_id) = args
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "update_task requires a non-empty `task_id`",
        }));
    };

    let title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    let priority = args
        .get("priority")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(ToOwned::to_owned)
        .map(Some);
    let due_date = args
        .get("due_date")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(ToOwned::to_owned)
        .map(Some);
    let tags = args.get("tags").and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(
                |name| crate::magician_v2::artifact_v2::models::TaskTagRecord {
                    id: Uuid::new_v4().to_string(),
                    name: name.trim().to_string(),
                    color: None,
                },
            )
            .collect::<Vec<_>>()
    });
    let schedule = match args.as_object().and_then(|object| object.get("schedule")) {
        Some(value) if value.is_null() => Some(None),
        Some(value) => {
            if let Err(error) = serde_json::from_value::<
                crate::magician_v2::storage::task_models::TaskSchedule,
            >(value.clone())
            {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("update_task schedule is not a valid TaskSchedule: {error}"),
                }));
            }
            Some(Some(value.clone()))
        },
        None => None,
    };

    if title.is_none()
        && description.is_none()
        && priority.is_none()
        && due_date.is_none()
        && tags.is_none()
        && schedule.is_none()
    {
        return Ok(json!({
            "status": "error",
            "reason": "update_task requires at least one field to update.",
        }));
    }

    let input = crate::magician_v2::artifact_v2::service::UpdateTaskInput {
        title,
        description,
        priority,
        due_date,
        tags,
        schedule,
        ..Default::default()
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    match service.update_task(&scope, task_id, input).await {
        Ok(task) => Ok(json!({
            "status": "updated",
            "task_id": task.manifest.task_id,
            "title": task.manifest.title,
            "description": task.manifest.description,
            "priority": task.manifest.priority,
            "due_date": task.manifest.due_date,
            "tags": task.manifest.tags
                .iter()
                .map(|tag| tag.name.clone())
                .collect::<Vec<_>>(),
        })),
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("update_task failed: {error}"),
        })),
    }
}
