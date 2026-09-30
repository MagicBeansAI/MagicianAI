//! `list_memory_tiers` — list the calling agent's declared memory tiers.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::{require_scope_str, scope_arg_str};

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "list_memory_tiers")?;
    let workspace = require_scope_str(&args, "__workspace", "list_memory_tiers")?;
    let agent_id = scope_arg_str(&args, "__agent_id")
        .ok_or_else(|| {
            ExecutionError::Step(
                "list_memory_tiers handler missing `agent_id` (passed as `__agent_id` from autonomous, or `agent_id` from chat bridge injection)".to_string(),
            )
        })?;

    let scoped = resources
        .agent_definition_store
        .for_scope(&principal, &workspace);
    let definition = match scoped.get_definition(&agent_id).await {
        Ok(Some(record)) => record.definition,
        Ok(None) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Agent '{agent_id}' has no definition in this scope."),
            }));
        },
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to load agent definition: {error}"),
            }));
        },
    };

    let tiers: Vec<Value> = definition
        .memory_tiers
        .iter()
        .map(|tier| {
            json!({
                "name": tier.name,
                "scope": format!("{:?}", tier.scope).to_ascii_lowercase(),
                "description": tier.description,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "agent_id": agent_id,
        "count": tiers.len(),
        "tiers": tiers,
    }))
}
