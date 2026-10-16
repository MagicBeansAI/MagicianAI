//! `get_execution_history` — list the most recent executions of a task.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "get_execution_history")?;
    let workspace = require_scope_str(&args, "__workspace", "get_execution_history")?;

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
            "reason": "get_execution_history requires `task_id`",
        }));
    };

    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(10)
        .clamp(1, 100);

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    match V3ReadApi::list_executions(service.as_ref(), &scope, task_id).await {
        Ok(mut executions) => {
            executions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            let entries: Vec<Value> = executions
                .into_iter()
                .take(limit)
                .map(|entry| {
                    json!({
                        "execution_id": entry.execution_id,
                        "status": entry.status,
                        "started_at": entry.started_at,
                        "completed_at": entry.completed_at,
                    })
                })
                .collect();
            Ok(json!({
                "status": "ok",
                "task_id": task_id,
                "count": entries.len(),
                "executions": entries,
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("get_execution_history failed: {error}"),
        })),
    }
}
