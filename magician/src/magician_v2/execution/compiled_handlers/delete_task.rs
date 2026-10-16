//! `delete_task` — archive (soft-delete) a task.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::{
    ArtifactV2Error, ScopeRef, APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "delete_task")?;
    let workspace = require_scope_str(&args, "__workspace", "delete_task")?;

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
            "reason": "delete_task requires a non-empty `task_id`",
        }));
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    match service
        .archive_task_with_options(&scope, task_id, true)
        .await
    {
        Ok(()) => Ok(json!({
            "status": "deleted",
            "task_id": task_id,
        })),
        Err(ArtifactV2Error::InvalidRequest(message))
            if message == format!("{APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED}:{task_id}") =>
        {
            Ok(json!({
                "status": "error",
                "error": APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED,
                "reason": "Governed app runs cannot be deleted through generic task tools.",
                "task_id": task_id,
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("delete_task failed: {error}"),
        })),
    }
}
