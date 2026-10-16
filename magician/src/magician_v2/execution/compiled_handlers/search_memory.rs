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
    let public_meeting_id = args
        .get("__ui_thread_id")
        .and_then(Value::as_str)
        .and_then(crate::magician_v2::media_seam::meeting_binding_id);
    let surface_untrusted =
        invocation_surface_is_untrusted(args.get("__invocation_surface").and_then(Value::as_str));
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
        true,
        public_meeting_id.as_deref(),
        surface_untrusted,
    )
    .await)
}

/// `meeting` and `public_envoy` are the untrusted audiences. Anything else,
/// including a missing token, is not.
pub fn invocation_surface_is_untrusted(surface: Option<&str>) -> bool {
    matches!(
        surface.map(str::trim),
        Some("meeting") | Some("public_envoy")
    )
}

/// Whether this recall must apply the public-surface filter.
///
/// The owner page passes `public_surface: false`. A trusted agent on an owner
/// surface stays unfiltered. An untrusted agent, or any agent on an untrusted
/// surface, is filtered.
pub fn search_applies_public_filter(
    public_surface: bool,
    agent_untrusted: bool,
    surface_untrusted: bool,
) -> bool {
    public_surface && (agent_untrusted || surface_untrusted)
}

/// Rank memory the way `search_memory` does and return that tool's JSON.
///
/// `record_retrieval` writes a temperature use. The owner Memory page passes
/// `false` so a debug lookup does not warm or cool the rows it is inspecting.
///
/// `public_surface` asks the tool path to drop anything a public conversation
/// must not see. That applies when the agent is untrusted, and also when the
/// call's own surface is untrusted, so a trusted assistant seated on a shared
/// call does not speak the owner's private notes into the room. The owner
/// Memory page passes `public_surface: false` and stays unfiltered. A missing
/// surface token does not filter a trusted agent. A model-supplied scope
/// cannot widen this.
pub async fn search_memory_recall(
    definition_store: &Arc<crate::magician_v2::agents::definition_store::AgentDefinitionStore>,
    resolver: &crate::magician_v2::agents::AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    agent_id: &str,
    query: &str,
    tier_filter: Option<&str>,
    limit: usize,
    retrieval_scope: &crate::magician_v2::agents::RetrievalScope,
    record_retrieval: bool,
    public_surface: bool,
    public_meeting_id: Option<&str>,
    surface_untrusted: bool,
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
    let apply_public = search_applies_public_filter(
        public_surface,
        definition.trust_level.is_untrusted(),
        surface_untrusted,
    );
    let public_scope = crate::magician_v2::agents::RetrievalScope::public_durable(
        public_meeting_id.map(str::to_string),
    );
    // Containment must precede the bounded index shortlist as well as rendering.
    let rank_scope = if apply_public {
        public_scope.clone()
    } else {
        retrieval_scope.clone()
    };

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
        &rank_scope,
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
    let mut eligible = ranked
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
                && (!apply_public
                    || (crate::magician_v2::agents::public_memory::public_surface_shows_candidate(
                        &public_scope,
                        &ranked.candidate,
                    ) && crate::magician_v2::agents::public_memory::public_prompt_line(
                        &ranked.candidate.text,
                    )
                    .is_some()))
        })
        .collect::<Vec<_>>();
    let positions = eligible
        .iter()
        .enumerate()
        .filter(|(_, ranked)| {
            magician_vector_index::episode_candidates::is_native_episode_candidate(
                &ranked.candidate,
            )
        })
        .take(crate::magician_v2::agents::memory_episode_recall::MAX_CANDIDATES)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let candidates = positions
        .iter()
        .map(|index| &eligible[*index].candidate)
        .collect::<Vec<_>>();
    if let Some(order) = crate::magician_v2::agents::memory_episode_recall::rerank_episode_order(
        &memory_service,
        query,
        &candidates,
        &meta.passages,
        &rank_scope,
    )
    .await
    {
        // Move each existing result exactly once, retaining the original score
        // and backend for diagnosis rather than pretending it is a model score.
        let mut originals = std::mem::take(&mut eligible)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut permutation = (0..originals.len()).collect::<Vec<_>>();
        for (destination, source) in positions.iter().zip(order) {
            permutation[*destination] = positions[source];
        }
        eligible = permutation
            .into_iter()
            .map(|index| originals[index].take().expect("validated permutation"))
            .collect();
    }
    let context_corpus = meta
        .context_candidates
        .iter()
        .filter(|candidate| {
            !apply_public
                || crate::magician_v2::agents::public_memory::public_surface_shows_candidate(
                    &public_scope,
                    candidate,
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut source_read_failed = meta.source_read_failed;
    let mut top = Vec::new();
    for ranked in eligible {
        if top.len() == limit {
            break;
        }
        if ranked.candidate.semantic_memory_type
            == crate::magician_v2::agents::SemanticMemoryType::Episode
            && !crate::magician_v2::agents::memory_episode_recall::episode_source_is_current(
                &memory_service,
                &ranked.candidate,
            )
            .await
        {
            source_read_failed = true;
            continue;
        }
        top.push(ranked);
    }
    let context_ids = top
        .iter()
        .flat_map(|ranked| {
            magician_vector_index::episode_candidates::episode_context_neighbors(
                &ranked.candidate,
                &context_corpus,
            )
        })
        .map(|candidate| candidate.item_key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut current_context = Vec::new();
    for candidate in &context_corpus {
        if context_ids.contains(candidate.item_key.as_str())
            && crate::magician_v2::agents::memory_episode_recall::episode_source_is_current(
                &memory_service,
                candidate,
            )
            .await
        {
            current_context.push(candidate.clone());
        }
    }
    // Keep the anchor in the neighborhood universe, preserving its ordered
    // position even when only its adjacent turns carry additional evidence.
    current_context.extend(
        top.iter()
            .filter(|ranked| {
                ranked.candidate.semantic_memory_type
                    == crate::magician_v2::agents::SemanticMemoryType::Episode
            })
            .map(|ranked| ranked.candidate.clone()),
    );
    current_context.sort_by(|left, right| left.item_key.cmp(&right.item_key));
    current_context.dedup_by(|left, right| left.item_key == right.item_key);
    let context_corpus = current_context;
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

    let final_app_memory = memory_service
        .app_memory_prompt_eligibility(
            top.iter()
                .map(|ranked| ranked.candidate.metadata_json.clone()),
            chrono::Utc::now(),
        )
        .await;
    top.retain(
        |ranked| match parse_source_eligibility_envelope(&ranked.candidate.metadata_json) {
            None => true,
            Some(Err(_)) => false,
            Some(Ok(envelope)) => {
                final_app_memory.get(envelope.candidate_id.as_str()) == Some(&true)
            },
        },
    );

    let selected_episode_ids = top
        .iter()
        .map(|ranked| ranked.candidate.item_key.clone())
        .collect::<std::collections::BTreeSet<_>>();
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
            let passage = meta.passages.get(
                &magician_vector_index::memory_index::memory_candidate_index_score_key(&candidate));
            let evidence_text = passage.map(|passage| passage.text.as_str()).unwrap_or(&candidate.text);
            let value_text = if apply_public {
                crate::magician_v2::agents::public_memory::redact_public_surface_text(
                    evidence_text,
                )
            } else {
                evidence_text.to_string()
            };
            let mut context = passage.map(|passage| {
                crate::magician_v2::agents::memory_episode_recall::episode_passage_context(&candidate, passage, 600)
                    .into_iter().map(|adjacent| {
                        let text = if apply_public { crate::magician_v2::agents::public_memory::redact_public_surface_text(&adjacent.text) } else { adjacent.text };
                        json!({"kind":"same_episode_passage","episode_id":candidate.item_key,"session_id":candidate.goal_id,
                            "recorded_at":candidate.last_updated,"content_hash":candidate.content_hash,
                            "text":text,"char_start":adjacent.char_start,"char_end":adjacent.char_end})
                    }).collect::<Vec<_>>()
            }).unwrap_or_default();
            context.extend(magician_vector_index::episode_candidates::episode_context_neighbors(&candidate, &context_corpus)
                .into_iter().filter(|neighbor| !selected_episode_ids.contains(&neighbor.item_key)).map(|neighbor| {
                    let passage = magician_vector_index::memory_index::memory_candidate_query_passage(neighbor, query);
                    let text = if apply_public { crate::magician_v2::agents::public_memory::redact_public_surface_text(&passage.text) } else { passage.text };
                    json!({"kind":"adjacent_session_turn","episode_id":neighbor.item_key,"session_id":neighbor.goal_id,"recorded_at":neighbor.last_updated,
                        "content_hash":neighbor.content_hash,"text":text,"char_start":passage.char_start,"char_end":passage.char_end})
                }));
            let mut item = json!({
                "kind": kind,
                "scope": memory_candidate_scope_label(&candidate.scope),
                "tier": candidate.tier_name,
                "semantic_memory_type": candidate.semantic_memory_type.as_str(),
                "temperature_tier": temperature_tier.as_str(),
                "key": candidate.item_key,
                "value": value_text,
                "score": score,
                "score_backend": if used_hybrid { "lancedb_hybrid" } else { "keyword" },
                "confidence": candidate.confidence,
                "last_updated": candidate.last_updated,
                "goal_id": candidate.goal_id,
                "passage": passage.map(|passage| json!({
                    "char_start": passage.char_start, "char_end": passage.char_end,
                    "chunk_index": passage.chunk_index,
                })),
                "context": context,
            });
            if !apply_public {
                if let Some(object) = item.as_object_mut() {
                    object.insert("source_path".to_string(), json!(source_path));
                    object.insert(
                        "json_pointer".to_string(),
                        Value::String(candidate.json_pointer),
                    );
                    object.insert(
                        "content_hash".to_string(),
                        Value::String(candidate.content_hash),
                    );
                    object.insert("metadata".to_string(), candidate.metadata_json);
                }
            }
            item
        })
        .collect();

    let mut response = json!({
        "status": if source_read_failed { if matches.is_empty() { "error" } else { "partial" } } else { "ok" },
        "source_read_failed": source_read_failed,
        "reason": if source_read_failed { Some("source_read_failed") } else { None },
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

#[cfg(test)]
mod tests {
    use super::{invocation_surface_is_untrusted, search_applies_public_filter};

    #[test]
    fn a_shared_call_filters_search_and_the_owner_page_does_not() {
        assert!(search_applies_public_filter(true, true, false));
        assert!(search_applies_public_filter(true, false, true));
        assert!(!search_applies_public_filter(true, false, false));
        assert!(!search_applies_public_filter(false, true, true));
        assert!(invocation_surface_is_untrusted(Some("meeting")));
        assert!(invocation_surface_is_untrusted(Some("public_envoy")));
        assert!(!invocation_surface_is_untrusted(Some("realtime_voice")));
        assert!(!invocation_surface_is_untrusted(Some("chat")));
        assert!(!invocation_surface_is_untrusted(None));
    }
}
