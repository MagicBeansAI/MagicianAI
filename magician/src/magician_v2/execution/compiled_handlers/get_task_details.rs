//! `get_task_details` — universal compiled tool returning basic task
//! metadata + a bounded recent-execution history. Intentionally minimal:
//! the rich chat-coupled surface (download URLs, projected output
//! cards, continuation-pack previews) lives on the chat-runtime
//! `get_task_details_for_chat` tool.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

const DEFAULT_EXECUTION_LIMIT: usize = 5;
const MAX_EXECUTION_LIMIT: usize = 20;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "get_task_details")?;
    let workspace = require_scope_str(&args, "__workspace", "get_task_details")?;

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
            "reason": "get_task_details requires a non-empty `task_id`",
        }));
    };

    let execution_limit = args
        .get("execution_limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_EXECUTION_LIMIT)
        .clamp(1, MAX_EXECUTION_LIMIT);

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    let task = match V3ReadApi::get_task(service.as_ref(), &scope, task_id).await {
        Ok(task) => task,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("get_task_details failed: {error}"),
            }));
        },
    };

    let recent_executions: Vec<Value> =
        match V3ReadApi::list_executions(service.as_ref(), &scope, task_id).await {
            Ok(mut executions) => {
                executions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
                executions
                    .into_iter()
                    .take(execution_limit)
                    .map(|entry| {
                        json!({
                            "execution_id": entry.execution_id,
                            "status": entry.status,
                            "started_at": entry.started_at,
                            "completed_at": entry.completed_at,
                        })
                    })
                    .collect()
            },
            Err(_) => Vec::new(),
        };

    Ok(json!({
        "status": "ok",
        "task_id": task.manifest.task_id,
        "title": task.manifest.title,
        "description": task.manifest.description,
        "status_value": task.state.status,
        "agent_id": task.manifest.agent_id,
        "ui_thread_id": task.manifest.ui_thread_id,
        "created_at": task.manifest.created_at,
        "updated_at": task.manifest.updated_at,
        "active_execution_id": task.state.active_root_execution_id,
        "latest_execution_id": task.state.latest_root_execution_id,
        "recent_executions": recent_executions,
    }))
}
