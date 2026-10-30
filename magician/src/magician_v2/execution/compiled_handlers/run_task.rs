//! `run_task` — universal agentic core: start a new execution for an
//! existing task. No chat-specific side effects; chat dispatch attaches
//! the resulting task's progress automatically.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::{is_terminal_execution_status, ScopeRef, V3ReadApi};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "run_task")?;
    let workspace = require_scope_str(&args, "__workspace", "run_task")?;

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
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "run_task requires a non-empty `task_id`",
        }));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    Ok(
        match Arc::clone(service)
            .start_execution(scope.clone(), task_id.to_string(), None, false)
            .await
        {
            Ok((task, execution)) => json!({
                "status": "started",
                "task_id": task.manifest.task_id,
                "task_url": crate::magician_v2::ui_links::task_record_href(&task, Some(&execution.state.execution_id)),
                "execution_id": execution.state.execution_id,
                "task_status": task.state.status,
                "execution_status": execution.state.status,
            }),
            Err(error) => {
                // The service's atomic active-root gate remains the launch
                // authority. A repeated request must follow its live winner,
                // rather than redo the work in chat or launch a detached worker.
                if error.to_string().contains("task_execution_in_progress:") {
                    if let Ok(task) = service.get_task(&scope, task_id).await {
                        if let Some(id) = task.state.active_root_execution_id.as_deref() {
                            if let Ok(execution) = service.get_execution(&scope, task_id, id).await
                            {
                                if !is_terminal_execution_status(&execution.state.status) {
                                    return Ok(json!({
                                        "status": "already_active",
                                        "task_id": task.manifest.task_id,
                                        "task_url": crate::magician_v2::ui_links::task_record_href(&task, Some(id)),
                                        "execution_id": id,
                                        "task_status": task.state.status,
                                        "execution_status": execution.state.status,
                                    }));
                                }
                            }
                        }
                    }
                }
                json!({"status": "error", "reason": format!("run_task failed: {error}")})
            },
        },
    )
}
