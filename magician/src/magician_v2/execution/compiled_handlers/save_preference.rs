//! `save_preference` — universal: write a single key/value into the
//! user / agent / personality memory tier.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::agents::public_memory::merge_isolated_public_user_fields;
use crate::magician_v2::agents::types::UserMemoryIsolation;
use crate::magician_v2::agents::TierScope;
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
    let definition = match definition_store
        .for_scope(&principal, &workspace)
        .get_definition(&agent_id)
        .await
    {
        Ok(Some(record)) => Some(record.definition),
        Ok(None) => None,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("save_preference agent-definition lookup failed: {error}"),
            }));
        },
    };
    let fully_isolated = definition.as_ref().is_some_and(|definition| {
        definition.user_memory_isolation == UserMemoryIsolation::FullyIsolated
    });
    let untrusted = definition
        .as_ref()
        .is_some_and(|definition| definition.trust_level.is_untrusted());
    // User-tier notes from a public agent stay in that agent's own file.
    // Trust is enough: an untrusted agent that is not fully isolated must
    // not fall through to the owner's shared preferences.
    if fully_isolated || untrusted {
        if let Some(user_tier_name) = preferred_tier
            .as_deref()
            .and_then(normalized_user_memory_tier_name)
            .or_else(|| preferred_tier.is_none().then(|| "preferences".to_string()))
        {
            let user_tier_name = if user_tier_name.is_empty() {
                "preferences".to_string()
            } else {
                user_tier_name
            };
            let service = match resolver.resolve_for_scope(&principal, &workspace) {
                Ok(service) => service,
                Err(error) => {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!("memory resolver failed: {error}"),
                    }));
                },
            };
            return Ok(merge_isolated_public_user_fields(
                &service,
                &agent_id,
                &user_tier_name,
                &serde_json::Map::from_iter([(key.to_string(), value)]),
            )
            .await);
        }
    }
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
    let Some(definition) = definition else {
        return Ok(json!({
            "status": "error",
            "reason": format!("Agent '{agent_id}' has no definition in this scope."),
        }));
    };
    if untrusted {
        return Ok(json!({
            "status": "error",
            "reason": "save refused: a public agent stores notes only as its own preferences",
        }));
    }
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
    if fully_isolated && matches!(tier_def.scope, TierScope::User) {
        let service = match resolver.resolve_for_scope(&principal, &workspace) {
            Ok(service) => service,
            Err(error) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("memory resolver failed: {error}"),
                }));
            },
        };
        return Ok(merge_isolated_public_user_fields(
            &service,
            &agent_id,
            &tier_def.name,
            &serde_json::Map::from_iter([(key.to_string(), value)]),
        )
        .await);
    }
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
