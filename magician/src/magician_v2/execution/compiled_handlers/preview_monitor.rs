//! `preview_monitor` — interpret a monitoring ask into a reviewable
//! `MonitorSpecV1` + schedule draft WITHOUT creating anything (plan §5.1
//! step 2: Presto calls `preview_monitor`, not generic `create_task`).
//!
//! Deterministic: validates/normalizes through the Phase 1 admission gate
//! and returns the interpreted contract + `preview_fingerprint` that
//! `create_monitor` requires back. All logic lives in
//! [`super::monitor_tools`] so tests drive it without `AgentResources`.

use std::sync::Arc;

use serde_json::Value;

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::monitor_tools::preview_monitor_response;
use super::shared::require_scope_str;

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    // Scope params are runtime-injected; preview touches no store but the
    // dispatch contract still requires a bound scope (fail-closed).
    let _principal = require_scope_str(&args, "__principal", "preview_monitor")?;
    let _workspace = require_scope_str(&args, "__workspace", "preview_monitor")?;
    Ok(preview_monitor_response(&args))
}
