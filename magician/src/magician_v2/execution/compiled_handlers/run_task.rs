//! `run_task` — universal agentic core: start a new execution for an
//! existing task. No chat-specific side effects; chat surfaces the
//! running task by following up with `subscribe_to_task_for_chat`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
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
            .start_execution(scope, task_id.to_string(), None, false)
            .await
        {
            Ok((task, execution)) => json!({
                "status": "started",
                "task_id": task.manifest.task_id,
                "execution_id": execution.state.execution_id,
                "task_status": task.state.status,
                "execution_status": execution.state.status,
            }),
            Err(error) => json!({
                "status": "error",
                "reason": format!("run_task failed: {error}"),
            }),
        },
    )
}
