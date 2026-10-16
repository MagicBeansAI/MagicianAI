//! `stop_task` — cancel the active execution of a task (if any).

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "stop_task")?;
    let workspace = require_scope_str(&args, "__workspace", "stop_task")?;

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
            "reason": "stop_task requires a non-empty `task_id`",
        }));
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    let task = match service.get_task(&scope, task_id).await {
        Ok(task) => task,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("stop_task lookup failed: {error}"),
            }));
        },
    };

    let Some(execution_id) = task.state.active_root_execution_id.clone() else {
        return Ok(json!({
            "status": "noop",
            "task_id": task_id,
            "task_status": task.state.status,
            "reason": "Task has no active execution to cancel.",
        }));
    };

    match service.cancel_execution_by_id(&scope, &execution_id).await {
        Ok((task, execution)) => Ok(json!({
            "status": "cancelled",
            "task_id": task.manifest.task_id,
            "execution_id": execution.state.execution_id,
            "task_status": task.state.status,
            "execution_status": execution.state.status,
        })),
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("stop_task cancel failed: {error}"),
        })),
    }
}
