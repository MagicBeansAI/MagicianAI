//! `unpublish_dashboard` — universal: retract a previously published
//! surface record.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "unpublish_dashboard")?;
    let workspace = require_scope_str(&args, "__workspace", "unpublish_dashboard")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };

    let Some(surface_id) = args
        .get("surface_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "unpublish_dashboard requires a non-empty `surface_id`",
        }));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    Ok(
        match service.unpublish_surface_record(&scope, surface_id).await {
            Ok(record) => json!({
                "status": "unpublished",
                "surface_id": record.surface_id,
                "route": record.route,
                "title": record.title,
                "task_id": record.task_id,
                "unpublished_at": record.unpublished_at,
            }),
            Err(error) => json!({
                "status": "error",
                "reason": format!("unpublish_dashboard failed: {error}"),
            }),
        },
    )
}
