//! `update_monitor` — edit an existing monitor's title / spec fields /
//! schedule through the same `update_task` path `PATCH
//! /api/magician/v3/monitors/{task_id}` uses. A spec edit bumps the
//! server-owned `monitor_revision`; plain tasks are refused with
//! `monitor_not_found` so generic tasks are never mutated here. All logic
//! lives in [`super::monitor_tools`].

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::ScopeRef;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::monitor_tools::update_monitor_response;
use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "update_monitor")?;
    let workspace = require_scope_str(&args, "__workspace", "update_monitor")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    Ok(update_monitor_response(service, &scope, &args).await)
}
