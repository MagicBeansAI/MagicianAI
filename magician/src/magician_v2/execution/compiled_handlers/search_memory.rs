//! `search_memory` — universal memory recall using the shared
//! lancedb hybrid index with a keyword fallback.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::agents::{
    default_temperature_tier, memory_candidate_has_superseded_lifecycle,
    memory_temperature_candidate_key, memory_temperature_entry_is_superseded,
    record_memory_temperature_retrieval_usage, sync_memory_temperature_overlay,
};
use crate::magician_v2::apps::memory_bridge::parse_source_eligibility_envelope;
use crate::magician_v2::chat::service::{
    memory_candidate_scope_label, normalized_user_memory_tier_name, rank_memory_candidates_hybrid,
    RankedMemoryCandidate,
};
use crate::magician_v2::engagement_retrieval::retrieval_scope_from_runtime_args;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "search_memory")?;
    let workspace = require_scope_str(&args, "__workspace", "search_memory")?;
    let agent_id = require_scope_str(&args, "__agent_id", "search_memory")?;
    let resolver = resources.memory_resolver.as_ref();
    let definition_store = &resources.agent_definition_store;
    let Some(query) = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "search_memory requires a non-empty `query`",
        }));
    };
    let tier_filter = args
        .get("tier")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(10)
        .clamp(1, 50);
    let retrieval_scope = match retrieval_scope_from_runtime_args(&args) {
        Ok(scope) => scope,
        Err(reason) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("search_memory refused: {reason}"),
            }));
        },
    };
    Ok(search_memory_recall(
        definition_store,
        resolver,
        &principal,
        &workspace,
        &agent_id,
        query,
        tier_filter.as_deref(),
        limit,
        &retrieval_scope,
        true,
    )
    .await)
}

/// Rank memory the way `search_memory` does and return that tool's JSON.
///
/// `record_retrieval` writes a temperature use. The owner Memory page passes
/// `false` so a debug lookup does not warm or cool the rows it is inspecting.
pub async fn search_memory_recall(
    definition_store: &Arc<
        crate::magician_v2::agents::definition_store::AgentDefinitionStore,
    >,
    resolver: &crate::magician_v2::agents::AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    agent_id: &str,
    query: &str,
    tier_filter: Option<&str>,
    limit: usize,
    retrieval_scope: &crate::magician_v2::agents::RetrievalScope,
    record_retrieval: bool,
) -> Value {
    let limit = limit.clamp(1, 50);

    let definition = match definition_store
        .for_scope(&principal, &workspace)
        .get_definition(&agent_id)
        .await
    {
        Ok(Some(record)) => record.definition,
        Ok(None) => {
            return json!({
                "status": "error",
                "reason": format!("Agent '{agent_id}' has no definition in this scope."),
            });
        },
        Err(error) => {
            return json!({
                "status": "error",
                "reason": format!("search_memory agent-definition lookup failed: {error}"),
            });
        },
    };
    let memory_service = match resolver.resolve_for_scope(principal, workspace) {
        Ok(service) => service,
        Err(error) => {
            return json!({
                "status": "error",
                "reason": format!("search_memory resolver failed: {error}"),
            });
        },
    };
    let user_tier_filter = tier_filter.and_then(normalized_user_memory_tier_name);

    let (ranked, meta) = rank_memory_candidates_hybrid(
        Some(definition_store),
        &principal,
        &workspace,
        &agent_id,
        &definition,
        &memory_service,
        query,
        tier_filter,
        user_tier_filter.as_deref(),
        retrieval_scope,
    )
    .await;
    let ranked_candidates = ranked
        .iter()
        .map(|ranked| ranked.candidate.clone())
        .collect::<Vec<_>>();
    let temperature_overlay =
        match sync_memory_temperature_overlay(memory_service.storage(), &ranked_candidates).await {
            Ok(overlay) => Some(overlay),
            Err(error) => {
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %error,
                    "failed to sync search_memory temperature overlay"
                );
                None
            },
        };
    let live_app_memory = memory_service
        .app_memory_prompt_eligibility(
            ranked_candidates
                .iter()
                .map(|candidate| candidate.metadata_json.clone()),
            chrono::Utc::now(),
        )
        .await;
    let top = ranked
        .into_iter()
        .filter(|ranked| {
            match parse_source_eligibility_envelope(&ranked.candidate.metadata_json) {
                None => {},
                Some(Err(_)) => return false,
                Some(Ok(envelope)) => {
                    if live_app_memory.get(&envelope.candidate_id.to_string()) != Some(&true) {
                        return false;
                    }
                },
            }
            if memory_candidate_has_superseded_lifecycle(&ranked.candidate) {
                return false;
            }
            temperature_overlay
                .as_ref()
                .and_then(|overlay| {
                    overlay
                        .entries
                        .get(&memory_temperature_candidate_key(&ranked.candidate))
                })
                .is_none_or(|entry| !memory_temperature_entry_is_superseded(entry))
        })
        .take(limit)
        .collect::<Vec<_>>();
    let retrieved_candidate_keys = top
        .iter()
        .map(|ranked| memory_temperature_candidate_key(&ranked.candidate))
        .collect::<Vec<_>>();
    if record_retrieval {
        if let Err(error) = record_memory_temperature_retrieval_usage(
            memory_service.storage(),
            &retrieved_candidate_keys,
        )
        .await
        {
            tracing::warn!(
                agent_id = %agent_id,
                error = %error,
                "failed to record search_memory temperature retrieval usage"
            );
        }
    }

    let matches: Vec<Value> = top
        .into_iter()
        .map(|ranked| {
            let RankedMemoryCandidate {
                score,
                used_hybrid,
                candidate,
            } = ranked;
            let source_path = candidate
                .source_path
                .as_ref()
                .map(|path| path.display().to_string());
            let kind = if candidate.json_pointer.is_empty() {
                "episode"
            } else {
                "tier_field"
            };
            let temperature_tier = temperature_overlay
                .as_ref()
                .and_then(|overlay| {
                    overlay
                        .entries
                        .get(&memory_temperature_candidate_key(&candidate))
                        .map(|entry| entry.temperature_tier)
                })
                .unwrap_or_else(|| default_temperature_tier(candidate.semantic_memory_type));
            json!({
                "kind": kind,
                "scope": memory_candidate_scope_label(&candidate.scope),
                "tier": candidate.tier_name,
                "semantic_memory_type": candidate.semantic_memory_type.as_str(),
                "temperature_tier": temperature_tier.as_str(),
                "key": candidate.item_key,
                "value": candidate.text,
                "score": score,
                "score_backend": if used_hybrid { "lancedb_hybrid" } else { "keyword" },
                "confidence": candidate.confidence,
                "last_updated": candidate.last_updated,
                "goal_id": candidate.goal_id,
                "source_path": source_path,
                "json_pointer": candidate.json_pointer,
                "content_hash": candidate.content_hash,
                "metadata": candidate.metadata_json,
            })
        })
        .collect();

    let mut response = json!({
        "status": "ok",
        "query": query,
        "count": matches.len(),
        "matches": matches,
        "backend": meta.backend,
    });
    if let Some(reason) = meta.fallback_reason {
        if let Some(obj) = response.as_object_mut() {
            obj.insert("fallback_reason".to_string(), Value::String(reason));
        }
    }
    response
}
