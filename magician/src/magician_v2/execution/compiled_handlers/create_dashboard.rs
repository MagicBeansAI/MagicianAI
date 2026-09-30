//! `create_dashboard` — universal: publish a task's surface record
//! into the workspace dashboards rail.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "create_dashboard")?;
    let workspace = require_scope_str(&args, "__workspace", "create_dashboard")?;

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
            "reason": "create_dashboard requires a non-empty `task_id`",
        }));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    let task_record = match V3ReadApi::get_task(service.as_ref(), &scope, task_id).await {
        Ok(record) => record,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("create_dashboard task lookup failed: {error}"),
            }));
        },
    };
    let route = args
        .get("route")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let placement_kind = args
        .get("placement_kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("workspace")
        .to_string();
    let pinned = args.get("pinned").and_then(Value::as_bool).unwrap_or(true);
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let summary_arg = args
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let source_output_id = args
        .get("source_output_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let input = match crate::magician_v2::artifact_v2::models::build_dashboard_publish_input(
        task_id,
        &scope.workspace(),
        &task_record.manifest.ui_thread_id,
        &placement_kind,
        pinned,
        route,
        title,
        summary_arg,
        source_output_id,
    ) {
        Ok(input) => input,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("create_dashboard: {error}"),
            }));
        },
    };
    Ok(match service.publish_surface_record(&scope, input).await {
        Ok(record) => json!({
            "status": "published",
            "surface_id": record.surface_id,
            "route": record.route,
            "surface_kind": record.surface_kind,
            "task_id": record.task_id,
            "title": record.title,
            "summary": record.summary,
            "placement": record.placement,
            "published_at": record.published_at,
        }),
        Err(error) => json!({
            "status": "error",
            "reason": format!("create_dashboard failed: {error}"),
        }),
    })
}
