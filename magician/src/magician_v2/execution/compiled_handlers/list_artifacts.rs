//! `list_artifacts` — enumerate the artifacts produced by a specific
//! task / execution.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "list_artifacts")?;
    let workspace = require_scope_str(&args, "__workspace", "list_artifacts")?;

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
            "reason": "list_artifacts requires `task_id`",
        }));
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);

    let task = match service.get_task(&scope, task_id).await {
        Ok(task) => task,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("list_artifacts task lookup failed: {error}"),
            }));
        },
    };

    let execution_id = args
        .get("execution_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| task.state.active_root_execution_id.clone())
        .or_else(|| task.state.latest_root_execution_id.clone());

    let Some(execution_id) = execution_id else {
        return Ok(json!({
            "status": "ok",
            "task_id": task_id,
            "count": 0,
            "artifacts": [],
            "note": "Task has no executions yet; nothing to list.",
        }));
    };

    let store = crate::magician_v2::artifact_v2::FilesystemExecutionArtifactIndexStore::new(
        service.workspace().clone(),
    );
    match store.list_artifacts(&scope, task_id, &execution_id).await {
        Ok(records) => {
            let artifacts: Vec<Value> = records
                .into_iter()
                .map(|record| {
                    json!({
                        "artifact_id": record.artifact_id,
                        "artifact_type": record.artifact_type,
                        "content_type": record.content_type,
                        "produced_at": record.produced_at,
                    })
                })
                .collect();
            Ok(json!({
                "status": "ok",
                "task_id": task_id,
                "execution_id": execution_id,
                "count": artifacts.len(),
                "artifacts": artifacts,
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("list_artifacts failed: {error}"),
        })),
    }
}
