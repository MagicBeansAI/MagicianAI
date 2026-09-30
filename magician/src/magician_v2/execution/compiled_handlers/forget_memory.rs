//! `forget_memory` — universal: two-phase semantic delete with
//! confirm-token round-trip.

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::agents::{
    default_temperature_tier, load_memory_temperature_overlay, memory_temperature_candidate_key,
};
use crate::magician_v2::chat::service::{
    forget_memory_confirm_token, memory_candidate_scope_label, normalized_user_memory_tier_name,
    rank_memory_candidates_hybrid, remove_at_json_pointer, truncate_text_for_audit,
    RankedMemoryCandidate,
};
use crate::magician_v2::engagement_retrieval::retrieval_scope_from_runtime_args;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use magician_vector_index::storage_trait::MemoryStorage;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "forget_memory")?;
    let workspace = require_scope_str(&args, "__workspace", "forget_memory")?;
    let agent_id = require_scope_str(&args, "__agent_id", "forget_memory")?;
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
            "reason": "forget_memory requires a non-empty `query`",
        }));
    };
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
    let provided_confirm = args
        .get("confirm")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if !dry_run && provided_confirm.is_none() {
        return Ok(json!({
            "status": "error",
            "reason": "forget_memory with `dry_run: false` requires a `confirm` token from a prior `dry_run: true` call",
        }));
    }
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
        .unwrap_or(1)
        .clamp(1, 10);
    let user_tier_filter = tier_filter
        .as_deref()
        .and_then(normalized_user_memory_tier_name);

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
                "reason": format!("forget_memory agent-definition lookup failed: {error}"),
            }));
        },
    };
    let memory_service = match resolver.resolve_for_scope(&principal, &workspace) {
        Ok(service) => service,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("forget_memory resolver failed: {error}"),
            }));
        },
    };

    // §5A.2 — `forget_memory` picks its deletion candidates with the same
    // ranking `search_memory` shows, so it is the same retrieval surface and
    // takes the same containment. Without it a bound execution could enumerate
    // another engagement's entries through the deletion preview, and then
    // delete them.
    let retrieval_scope = match retrieval_scope_from_runtime_args(&args) {
        Ok(scope) => scope,
        Err(reason) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("forget_memory refused: {reason}"),
            }));
        },
    };

    let (ranked, meta) = rank_memory_candidates_hybrid(
        Some(definition_store),
        &principal,
        &workspace,
        &agent_id,
        &definition,
        &memory_service,
        query,
        tier_filter.as_deref(),
        user_tier_filter.as_deref(),
        &retrieval_scope,
    )
    .await;
    let top: Vec<RankedMemoryCandidate> = ranked.into_iter().take(limit).collect();
    let confirm_token = forget_memory_confirm_token(query, tier_filter.as_deref(), &top);
    let temperature_overlay = load_memory_temperature_overlay(memory_service.storage())
        .await
        .ok();

    let render_match = |ranked: &RankedMemoryCandidate| -> Value {
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
                    .get(&memory_temperature_candidate_key(candidate))
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
            "score_backend": if *used_hybrid { "lancedb_hybrid" } else { "keyword" },
            "confidence": candidate.confidence,
            "content_hash": candidate.content_hash,
            "last_updated": candidate.last_updated,
            "goal_id": candidate.goal_id,
            "metadata": candidate.metadata_json,
            "delete_target": {
                "source_path": source_path,
                "json_pointer": candidate.json_pointer,
                "removal_mode": if candidate.json_pointer.is_empty() { "whole_file" } else { "json_pointer_field" },
            },
        })
    };

    if dry_run {
        let matches: Vec<Value> = top.iter().map(render_match).collect();
        let mut response = json!({
            "status": "ok",
            "mode": "dry_run",
            "query": query,
            "count": matches.len(),
            "matches": matches,
            "confirm_token": confirm_token,
            "backend": meta.backend,
            "note": "Pass the same `query` (and `tier`/`limit` if specified) plus this `confirm_token` with `dry_run: false` to delete these entries.",
        });
        if let Some(reason) = meta.fallback_reason {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("fallback_reason".to_string(), Value::String(reason));
            }
        }
        return Ok(response);
    }

    let provided = provided_confirm.unwrap_or("");
    if provided != confirm_token {
        return Ok(json!({
            "status": "stale_confirm",
            "reason": "Provided `confirm` token doesn't match the current top matches. Memory may have shifted between the dry-run and this commit. Re-issue with `dry_run: true` to get a fresh token.",
            "expected_token": confirm_token,
            "provided_token": provided,
            "current_matches": top.iter().map(render_match).collect::<Vec<_>>(),
        }));
    }

    let storage = memory_service.storage();
    let mut deleted: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut episode_index_dirty = false;
    for ranked in top {
        let RankedMemoryCandidate { ref candidate, .. } = ranked;
        let Some(path) = candidate.source_path.as_ref() else {
            skipped.push(json!({
                "reason": "no_source_path",
                "tier": candidate.tier_name,
                "key": candidate.item_key,
            }));
            continue;
        };
        if candidate.json_pointer.is_empty() {
            match tokio::fs::remove_file(path).await {
                Ok(()) => {
                    episode_index_dirty = true;
                    deleted.push(json!({
                        "kind": "episode",
                        "scope": memory_candidate_scope_label(&candidate.scope),
                        "tier": candidate.tier_name,
                        "key": candidate.item_key,
                        "value_excerpt": truncate_text_for_audit(&candidate.text, 120),
                        "content_hash": candidate.content_hash,
                        "source_path": path.display().to_string(),
                        "goal_id": candidate.goal_id,
                    }));
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    episode_index_dirty = true;
                    deleted.push(json!({
                        "kind": "episode",
                        "scope": memory_candidate_scope_label(&candidate.scope),
                        "tier": candidate.tier_name,
                        "key": candidate.item_key,
                        "source_path": path.display().to_string(),
                        "note": "file_already_absent",
                    }));
                },
                Err(error) => {
                    skipped.push(json!({
                        "reason": format!("episode_file_remove_failed: {error}"),
                        "tier": candidate.tier_name,
                        "key": candidate.item_key,
                        "source_path": path.display().to_string(),
                    }));
                },
            }
            continue;
        }
        let mut document = match storage.read_json_value(path).await {
            Ok(value) => value,
            Err(error) => {
                skipped.push(json!({
                    "reason": format!("read_failed: {error}"),
                    "tier": candidate.tier_name,
                    "key": candidate.item_key,
                    "source_path": path.display().to_string(),
                }));
                continue;
            },
        };
        match remove_at_json_pointer(&mut document, &candidate.json_pointer) {
            Ok(Some(_removed)) => match storage.write_json_value_atomic(path, &document).await {
                Ok(()) => {
                    deleted.push(json!({
                        "kind": "tier_field",
                        "scope": memory_candidate_scope_label(&candidate.scope),
                        "tier": candidate.tier_name,
                        "key": candidate.item_key,
                        "value_excerpt": truncate_text_for_audit(&candidate.text, 120),
                        "content_hash": candidate.content_hash,
                        "source_path": path.display().to_string(),
                        "json_pointer": candidate.json_pointer,
                    }));
                },
                Err(error) => {
                    skipped.push(json!({
                        "reason": format!("write_failed: {error}"),
                        "tier": candidate.tier_name,
                        "key": candidate.item_key,
                        "source_path": path.display().to_string(),
                    }));
                },
            },
            Ok(None) => {
                skipped.push(json!({
                    "reason": "pointer_not_found",
                    "tier": candidate.tier_name,
                    "key": candidate.item_key,
                    "json_pointer": candidate.json_pointer,
                }));
            },
            Err(reason) => {
                skipped.push(json!({
                    "reason": format!("pointer_remove_failed: {reason}"),
                    "tier": candidate.tier_name,
                    "key": candidate.item_key,
                    "json_pointer": candidate.json_pointer,
                }));
            },
        }
    }

    if !deleted.is_empty() {
        if let Err(error) = magician_vector_index::memory_index::record_memory_index_change(
            memory_service.storage(),
            magician_vector_index::memory_index::MemoryIndexChange::FullScope {
                reason: "forget_memory_delete".to_string(),
            },
        )
        .await
        {
            warn!(
                error = %error,
                "failed to persist forget-memory index marker; dirty rebuild remains the fallback"
            );
        }
        crate::magician_v2::analytics::memory_index_maintainer::mark_memory_index_dirty_for_scope(
            &principal,
            &workspace,
            "forget_memory_delete",
        );
    }

    if episode_index_dirty {
        if let Err(error) = memory_service.invalidate_episode_index(&agent_id).await {
            warn!(
                agent_id = %agent_id,
                error = %error,
                "[FORGET_MEMORY] episode index invalidation failed"
            );
        }
    }

    Ok(json!({
        "status": "ok",
        "mode": "committed",
        "query": query,
        "deleted_count": deleted.len(),
        "deleted": deleted,
        "skipped": skipped,
    }))
}
