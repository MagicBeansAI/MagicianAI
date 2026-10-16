//! `deactivate_skill` — universal: clear the agent-scope
//! `active_procedure_skill` memory tier. Phase 0.8c-6.
//!
//! Idempotent — calling on an empty tier is a success with a no-op
//! note. Prompt builders re-read the tier on each turn, so clearing
//! takes effect immediately.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use crate::magician_v2::agents::memory_tiers::TierScope;
use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::skills::active_procedure_skill_tier_def;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "deactivate_skill")?;
    let workspace = require_scope_str(&args, "__workspace", "deactivate_skill")?;
    let agent_id = require_scope_str(&args, "__agent_id", "deactivate_skill")?;
    let _ = args; // no other args used

    let memory_service = resources
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .map_err(|e| {
            ExecutionError::Step(format!("deactivate_skill memory resolver failed: {e}"))
        })?;

    let tier_def = active_procedure_skill_tier_def();
    let previous_name = match memory_service
        .load_native_tier(&agent_id, &tier_def, None)
        .await
    {
        Ok(Some(record)) => record
            .fields
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        Ok(None) => None,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to read active procedure skill: {error}"),
            }));
        },
    };

    if previous_name.is_none() {
        return Ok(json!({
            "status": "ok",
            "outcome": "noop",
            "message": "No procedure skill was active; nothing to deactivate.",
        }));
    }

    // Clear by writing an empty `fields` record (preserves the tier
    // record itself but signals "no active skill" via the absence of
    // a `name` field).
    let record = V3MemoryTierRecord {
        schema_version: "v3".to_string(),
        record_type: "memory_tier".to_string(),
        principal: Some(principal),
        workspace: Some(workspace),
        agent_id: Some(agent_id.clone()),
        tier_name: "active_procedure_skill".to_string(),
        tier_scope: TierScope::Agent,
        goal_id: None,
        last_updated: Utc::now(),
        fields: HashMap::new(),
    };
    if let Err(error) = memory_service
        .save_native_tier(&agent_id, &tier_def, None, &record)
        .await
    {
        return Ok(json!({
            "status": "error",
            "reason": format!("Failed to clear active procedure skill: {error}"),
        }));
    }

    let previous_label = previous_name.as_deref().unwrap_or("");
    Ok(json!({
        "status": "ok",
        "outcome": "deactivated",
        "previous_active": previous_name,
        "message": format!("Deactivated procedure skill '{}'.", previous_label),
    }))
}
