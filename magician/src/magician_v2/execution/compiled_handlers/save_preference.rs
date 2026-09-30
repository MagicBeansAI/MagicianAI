//! `save_preference` — universal: write a single key/value into the
//! user / agent / personality memory tier.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::chat::service::{
    merge_into_memory_tier, merge_user_preference_with_event, normalized_user_memory_tier_name,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let mut saved = handle_inner(resources.clone(), args.clone()).await?;
    if saved.get("status").and_then(Value::as_str) == Some("ok") {
        super::update_memory_tier::review_saved_user_memory(&resources, &args, &mut saved).await;
    }
    Ok(saved)
}

async fn handle_inner(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "save_preference")?;
    let workspace = require_scope_str(&args, "__workspace", "save_preference")?;
    let agent_id = require_scope_str(&args, "__agent_id", "save_preference")?;
    let resolver = resources.memory_resolver.as_ref();
    let definition_store = &resources.agent_definition_store;

    let Some(key) = args
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "save_preference requires a non-empty `key`",
        }));
    };
    let value = args.get("value").cloned().unwrap_or(Value::Null);
    if value.is_null() {
        return Ok(json!({
            "status": "error",
            "reason": "save_preference requires a `value` (use empty string to clear)",
        }));
    }
    let preferred_tier = args
        .get("tier")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    if let Some(user_tier_name) = preferred_tier
        .as_deref()
        .and_then(normalized_user_memory_tier_name)
    {
        let user_tier_name = if user_tier_name.is_empty() {
            "preferences".to_string()
        } else {
            user_tier_name
        };
        return Ok(merge_user_preference_with_event(
            resolver,
            &principal,
            &workspace,
            &user_tier_name,
            key,
            value,
            args.get("__execution_id").and_then(Value::as_str),
        )
        .await);
    }
    if preferred_tier.is_none() {
        return Ok(merge_user_preference_with_event(
            resolver,
            &principal,
            &workspace,
            "preferences",
            key,
            value,
            args.get("__execution_id").and_then(Value::as_str),
        )
        .await);
    }
    let definition = match definition_store
        .for_scope(&principal, &workspace)
        .get_definition(&agent_id)
        .await
    {
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
                "reason": format!("save_preference agent-definition lookup failed: {error}"),
            }));
        },
    };
    let candidate_names = [preferred_tier.as_deref(), Some("personality_profile")];
    let tier_def = candidate_names
        .iter()
        .flatten()
        .filter_map(|name| {
            definition
                .memory_tiers
                .iter()
                .find(|tier| tier.name.eq_ignore_ascii_case(name))
        })
        .next()
        .cloned();
    let Some(tier_def) = tier_def else {
        return Ok(json!({
            "status": "error",
            "reason": "No personality_profile / preferences memory tier exists on this agent.",
        }));
    };
    Ok(merge_into_memory_tier(
        resolver,
        &principal,
        &workspace,
        &agent_id,
        &tier_def,
        std::iter::once((key.to_string(), value)),
    )
    .await)
}
