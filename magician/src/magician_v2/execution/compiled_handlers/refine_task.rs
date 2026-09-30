//! `refine_task` — append a `[refinement]` block to a task's
//! description (optionally updating its title).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "refine_task")?;
    let workspace = require_scope_str(&args, "__workspace", "refine_task")?;

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
            "reason": "refine_task requires a non-empty `task_id`",
        }));
    };
    let Some(refinement) = args
        .get("refinement")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "refine_task requires a non-empty `refinement`",
        }));
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    let existing = match service.get_task(&scope, task_id).await {
        Ok(task) => task,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("refine_task lookup failed: {error}"),
            }));
        },
    };

    let mut new_description = existing.manifest.description.clone();
    if !new_description.trim().is_empty() {
        new_description.push_str("\n\n");
    }
    new_description.push_str("[refinement] ");
    new_description.push_str(refinement);

    let new_title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let input = crate::magician_v2::artifact_v2::service::UpdateTaskInput {
        title: new_title,
        description: Some(new_description),
        ..Default::default()
    };

    match service.update_task(&scope, task_id, input).await {
        Ok(task) => Ok(json!({
            "status": "refined",
            "task_id": task.manifest.task_id,
            "title": task.manifest.title,
            "description": task.manifest.description,
        })),
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("refine_task failed: {error}"),
        })),
    }
}
