//! `update_memory_tier` — universal: bulk-update a memory tier's
//! fields.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::chat::service::{
    merge_into_memory_tier, merge_user_memory_tier_fields, normalized_user_memory_tier_name,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let mut saved = handle_inner(resources.clone(), args.clone()).await?;
    if saved.get("status").and_then(Value::as_str) == Some("ok") {
        review_saved_user_memory(&resources, &args, &mut saved).await;
    }
    Ok(saved)
}

pub(super) async fn review_saved_user_memory(
    resources: &AgentResources,
    args: &Value,
    saved: &mut Value,
) {
    if saved.get("scope").and_then(Value::as_str) != Some("user") {
        return;
    }
    let Some(principal) = args.get("__principal").and_then(Value::as_str) else {
        return;
    };
    let Some(workspace) = args.get("__workspace").and_then(Value::as_str) else {
        return;
    };
    let Ok(service) = resources
        .memory_resolver
        .resolve_for_scope(principal, workspace)
    else {
        return;
    };
    saved["consolidation"] = json!({"state":"pending","durably_saved":true,
        "message":"Saved for consolidation. Conflicting or unreviewed revisions are not settled current memory."});
    let router = resources.operation_llm_router.clone();
    // A provider call can exceed this tool's 15-second deadline. The write is
    // already durable; the ordinary worker also resumes it after a restart.
    tokio::spawn(async move {
        if let Err(error) = crate::magician_v2::agents::memory_lifecycle::runtime::pass(
            &service,
            router.as_deref(),
            None,
            chrono::Utc::now(),
            None,
        )
        .await
        {
            tracing::warn!(%error,"saved memory review deferred");
        }
    });
}

async fn handle_inner(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "update_memory_tier")?;
    let workspace = require_scope_str(&args, "__workspace", "update_memory_tier")?;
    let agent_id = require_scope_str(&args, "__agent_id", "update_memory_tier")?;
    let resolver = resources.memory_resolver.as_ref();
    let definition_store = &resources.agent_definition_store;

    let Some(tier_name) = args
        .get("tier")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "update_memory_tier requires `tier`",
        }));
    };
    let Some(fields) = args.get("fields").and_then(Value::as_object) else {
        return Ok(json!({
            "status": "error",
            "reason": "update_memory_tier requires a `fields` object",
        }));
    };
    let prepared = crate::magician_v2::agents::memory_lifecycle::provenance::tool_fields(
        fields,
        args.get("__execution_id").and_then(Value::as_str),
        chrono::Utc::now(),
    );
    let fields = &prepared;
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
                "reason": format!("update_memory_tier agent-definition lookup failed: {error}"),
            }));
        },
    };
    let Some(tier_def) = definition
        .memory_tiers
        .iter()
        .find(|tier| tier.name.eq_ignore_ascii_case(tier_name))
        .cloned()
    else {
        if let Some(user_tier_name) = normalized_user_memory_tier_name(tier_name) {
            if user_tier_name.is_empty() {
                return Ok(json!({
                    "status": "error",
                    "reason": "`update_memory_tier` requires a concrete user tier such as `user.preferences`",
                }));
            }
            return Ok(merge_user_memory_tier_fields(
                resolver,
                &principal,
                &workspace,
                &user_tier_name,
                fields,
            )
            .await);
        }
        return Ok(json!({
            "status": "error",
            "reason": format!("Tier '{tier_name}' is not defined on agent '{agent_id}'."),
        }));
    };
    Ok(merge_into_memory_tier(
        resolver,
        &principal,
        &workspace,
        &agent_id,
        &tier_def,
        fields.clone().into_iter(),
    )
    .await)
}
