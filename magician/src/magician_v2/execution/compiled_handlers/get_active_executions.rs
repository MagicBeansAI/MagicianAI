//! `get_active_executions` — list the currently running / planning /
//! paused / pending tasks in the calling scope.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "get_active_executions")?;
    let workspace = require_scope_str(&args, "__workspace", "get_active_executions")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    let tasks = match V3ReadApi::list_tasks(service.as_ref(), &scope).await {
        Ok(tasks) => tasks,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("get_active_executions failed: {error}"),
            }));
        },
    };

    let active: Vec<Value> = tasks
        .into_iter()
        .filter(|task| {
            matches!(
                task.status.as_str(),
                "running" | "planning" | "paused" | "pending"
            )
        })
        .map(|task| {
            json!({
                "task_id": task.id,
                "title": task.title,
                "status": task.status,
                "agent_id": task.agent_id,
                "active_execution_id": task.active_root_execution_id,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "count": active.len(),
        "executions": active,
    }))
}
