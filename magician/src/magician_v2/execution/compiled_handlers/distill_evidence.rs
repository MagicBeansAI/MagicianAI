//! `distill_evidence` — generic: promote the metadata-only roll-up rows a
//! scheduled observe writer just wrote into reviewable work-evidence.
//!
//! Called as the FINAL step of an observe-writer run (after `update_memory_tier`
//! has persisted the per-account/day rows). Drives the SAME
//! [`distill_tier_producer`](crate::magician_v2::evidence::distill_tier_producer)
//! routine the `POST /evidence/distill/{producer}` HTTP entrypoint uses — the
//! LLM router + prompt manager come from process globals, so the tool needs only
//! the scoped `AgentResources` it already gets. This is what closes the
//! writer → tier → evidence loop without a polling sweep (re-LLM waste) or a hook
//! in the contended scheduled-task completion path.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "distill_evidence")?;
    let workspace = require_scope_str(&args, "__workspace", "distill_evidence")?;

    let Some(producer) = args
        .get("producer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "distill_evidence requires `producer` (e.g. \"email\", \"calendar\", or \"meeting\")",
        }));
    };
    let days = args.get("days").and_then(Value::as_u64).unwrap_or(1).max(1);

    let Some(router) =
        crate::magician_v2::query_analysis::operation_llm_router::global_operation_router()
    else {
        return Ok(json!({
            "status": "error",
            "producer": producer,
            "reason": "operation router not initialised — cannot distil evidence",
        }));
    };
    let Some(prompt_manager) = crate::magician_v2::prompts::global_prompt_manager() else {
        return Ok(json!({
            "status": "error",
            "producer": producer,
            "reason": "prompt manager not initialised — cannot distil evidence",
        }));
    };

    match crate::magician_v2::evidence::distill_tier_producer_with_broadcaster(
        resources.memory_resolver.as_ref(),
        &resources.artifact_workspace,
        &principal,
        &workspace,
        producer,
        days,
        router.as_ref(),
        &prompt_manager,
        resources.event_broadcaster.as_ref(),
    )
    .await
    {
        Ok(mut summary) => {
            if let Some(object) = summary.as_object_mut() {
                object.insert("status".to_string(), Value::String("ok".to_string()));
            }
            Ok(summary)
        },
        Err(reason) => Ok(json!({
            "status": "error",
            "producer": producer,
            "reason": reason,
        })),
    }
}
