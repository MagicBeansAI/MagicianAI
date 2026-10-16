//! `list_tasks` — list tasks in the calling scope, optionally
//! filtered by status.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "list_tasks")?;
    let workspace = require_scope_str(&args, "__workspace", "list_tasks")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };

    let status_filter = args
        .get("status_filter")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_ascii_lowercase);

    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    match V3ReadApi::list_tasks(service.as_ref(), &scope).await {
        Ok(tasks) => {
            let filtered: Vec<Value> = tasks
                .into_iter()
                .filter(|task| {
                    status_filter
                        .as_deref()
                        .map(|filter| task.status.eq_ignore_ascii_case(filter))
                        .unwrap_or(true)
                })
                .map(|task| {
                    json!({
                        "task_id": task.id,
                        "title": task.title,
                        "status": task.status,
                        "agent_id": task.agent_id,
                        "ui_thread_id": task.ui_thread_id,
                    })
                })
                .collect();
            Ok(json!({
                "status": "ok",
                "count": filtered.len(),
                "tasks": filtered,
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("list_tasks failed: {error}"),
        })),
    }
}
