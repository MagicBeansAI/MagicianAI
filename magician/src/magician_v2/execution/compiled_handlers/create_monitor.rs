//! `create_monitor` — activate a PREVIEWED monitor through the same
//! service path `POST /api/magician/v3/monitors` uses (plan §5.1 step 5:
//! "confirmation calls `create_monitor`").
//!
//! Refuses without a matching `preview_fingerprint` from `preview_monitor`
//! (`monitor_preview_required` / `monitor_preview_stale`) — the
//! review-before-recurring-spend gate is data, not prompt convention. All
//! logic lives in [`super::monitor_tools`].

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::ScopeRef;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::monitor_tools::create_monitor_response;
use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "create_monitor")?;
    let workspace = require_scope_str(&args, "__workspace", "create_monitor")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
    Ok(create_monitor_response(service, &scope, &args).await)
}
