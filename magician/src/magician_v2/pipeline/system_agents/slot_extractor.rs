//! Pipeline agent wrapper for [`SlotExtractor`].
//!
//! Reads the query from [`PipelineContext`], delegates to the existing
//! slot extractor, and stores the result as an [`ArtifactType::SlotGraph`]
//! artifact.
//!
//! D-04: if a `QueryAnalysis` artifact is present in the store, its
//! extracted entities are pre-seeded as `existing_slots` in the
//! `ConversationContext` so the extractor can avoid re-extracting already
//! known entities.
//!
//! D-10: after provisional extraction, a scoped memory lookup may add or
//! upgrade slots from the agent's `entities`, `environment_knowledge`, and
//! `insights` tiers before elicitation decides whether the user needs to be
//! asked anything.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Map, Value};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution;
use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
use crate::magician_v2::pipeline::agent::{
    PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext,
    AGENT_ID_SLOT_EXTRACTOR,
};
use crate::magician_v2::pipeline::artifact::{
    AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION,
};
use crate::magician_v2::query_analysis::unified_analyzer::UnifiedQueryAnalysis;
use crate::magician_v2::slot_graph::{
    extraction::{ConversationContext, SlotExtractor},
    types::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType},
};

/// Wraps [`SlotExtractor`] behind the [`PipelineAgent`] trait.
pub struct SlotExtractorAgent {
    extractor: Arc<SlotExtractor>,
    memory_resolver: AgentMemoryResolver,
}

impl SlotExtractorAgent {
    /// Create a new wrapper around the given slot extractor.
    pub fn new(extractor: Arc<SlotExtractor>, memory_resolver: AgentMemoryResolver) -> Self {
        Self {
            extractor,
            memory_resolver,
        }
    }
}

#[async_trait]
impl PipelineAgent for SlotExtractorAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_SLOT_EXTRACTOR
    }

    fn required_inputs(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::QueryAnalysis]
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::SlotGraph]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        let query_analysis = load_query_analysis(store);
        let existing_slots = query_analysis
            .as_ref()
            .map(build_query_analysis_seed_slots)
            .unwrap_or_default();

        let conversation_context = if existing_slots.is_empty() {
            None
        } else {
            Some(ConversationContext {
                previous_messages: vec![],
                existing_slots,
                screenshots: vec![],
            })
        };

        let extracted_slots = self
            .extractor
            .extract_slots_with_scope(
                &context.query,
                conversation_context.as_ref(),
                context.execution_id.as_deref(),
                context.correlation_id.as_deref(),
                context.principal.as_deref(),
                context.workspace.as_deref(),
                OperationLlmCallAttribution {
                    execution_id: context.execution_id.clone(),
                    task_id: context.task_id.clone(),
                    agent_id: context.agent_id.clone(),
                    ..OperationLlmCallAttribution::default()
                },
            )
            .await
            .map_err(|e| PipelineAgentError::ServiceError(e.to_string()))?;

        let mut slots: Vec<SlotRecord> = extracted_slots
            .into_iter()
            .map(|slot| SlotRecord::from_provisional(&context.workflow_id, slot))
            .collect();

        let memory_slots = resolve_slots_from_memory(
            &self.memory_resolver,
            context,
            query_analysis.as_ref(),
            &slots,
        )
        .await;
        if !memory_slots.is_empty() {
            debug!(
                "[PIPELINE:slot-extractor] resolved {} slot(s) from memory for workflow {}",
                memory_slots.len(),
                context.workflow_id
            );
            merge_memory_slots(&mut slots, memory_slots);
        }

        let content = serde_json::to_value(&slots)
            .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?;

        let artifact_id = Uuid::new_v4().to_string();
        let artifact = AgentArtifact {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::SlotGraph,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        };

        store.put(artifact);
        Ok(PipelineAgentResult::Completed {
            artifact_ids: vec![artifact_id],
        })
    }
}

fn load_query_analysis(store: &ArtifactStore) -> Option<UnifiedQueryAnalysis> {
    store
        .latest_of_type(&ArtifactType::QueryAnalysis)
        .and_then(|artifact| {
            artifact
                .deserialize_content::<UnifiedQueryAnalysis>()
                .map_err(|error| {
                    warn!(
                        "[PIPELINE:slot-extractor] QueryAnalysis deserialize failed: {}",
                        error
                    );
                    error
                })
                .ok()
        })
}

fn build_query_analysis_seed_slots(analysis: &UnifiedQueryAnalysis) -> Vec<SlotRecord> {
    let now = Utc::now();
    let confidence = analysis.extracted_entities.extraction_confidence as f64;
    analysis
        .extracted_entities
        .entities
        .iter()
        .map(|(name, value)| SlotRecord {
            id: format!("entity_{}", name),
            slot_type: SlotType::Entity,
            value: Value::String(value.clone()),
            confidence,
            provenance: vec![],
            evidence_links: vec![],
            created_at: now,
            updated_at: now,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct CandidateText {
    text: String,
    exact_source: bool,
}

async fn resolve_slots_from_memory(
    memory_resolver: &AgentMemoryResolver,
    context: &PipelineContext,
    analysis: Option<&UnifiedQueryAnalysis>,
    extracted_slots: &[SlotRecord],
) -> Vec<SlotRecord> {
    let Some(agent_id) = context
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    let (Some(principal), Some(workspace)) = (
        context
            .principal
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
        context
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) else {
        return Vec::new();
    };
    if context.tier_definitions.is_empty() {
        return Vec::new();
    }

    let memory_service = match memory_resolver.resolve_for_scope(principal, workspace) {
        Ok(service) => service,
        Err(error) => {
            warn!(
                "[PIPELINE:slot-extractor] scoped memory resolution failed for {principal}/{workspace}: {}",
                error
            );
            return Vec::new();
        },
    };

    let candidates = collect_candidate_texts(&context.query, analysis, extracted_slots);
    let mut resolved = Vec::new();

    if let Ok(Some(entity_tier)) = memory_service
        .load_native_tier_by_name(agent_id, "entities", &context.tier_definitions, None)
        .await
    {
        resolved.extend(resolve_entity_slots_from_tier(&entity_tier, &candidates));
    }

    if let Ok(Some(environment_tier)) = memory_service
        .load_native_tier_by_name(
            agent_id,
            "environment_knowledge",
            &context.tier_definitions,
            None,
        )
        .await
    {
        resolved.extend(resolve_environment_slots_from_tier(
            &environment_tier,
            &candidates,
        ));
    }

    if let Ok(Some(insights_tier)) = memory_service
        .load_native_tier_by_name(agent_id, "insights", &context.tier_definitions, None)
        .await
    {
        resolved.extend(resolve_insight_slots_from_tier(
            &insights_tier,
            &context.query,
        ));
    }

    resolved
}

fn collect_candidate_texts(
    query: &str,
    analysis: Option<&UnifiedQueryAnalysis>,
    extracted_slots: &[SlotRecord],
) -> Vec<CandidateText> {
    let mut candidates = BTreeMap::<String, CandidateText>::new();

    if let Some(analysis) = analysis {
        for value in analysis.extracted_entities.entities.values() {
            insert_candidate(&mut candidates, value, true);
        }
        for values in analysis.extracted_entities.typed_entities.values() {
            for value in values {
                insert_candidate(&mut candidates, value, true);
            }
        }
    }

    for slot in extracted_slots {
        let mut fragments = Vec::new();
        collect_string_leaves(&slot.value, &mut fragments);
        for fragment in fragments {
            insert_candidate(&mut candidates, &fragment, true);
        }
    }

    insert_candidate(&mut candidates, query, false);

    candidates.into_values().collect()
}

fn insert_candidate(
    candidates: &mut BTreeMap<String, CandidateText>,
    raw: &str,
    exact_source: bool,
) {
    let trimmed = raw.trim();
    let Some(normalized) = normalize_lookup_text(trimmed) else {
        return;
    };
    candidates
        .entry(normalized)
        .and_modify(|candidate| {
            if exact_source && !candidate.exact_source {
                candidate.text = trimmed.to_string();
                candidate.exact_source = true;
            }
        })
        .or_insert_with(|| CandidateText {
            text: trimmed.to_string(),
            exact_source,
        });
}

fn collect_string_leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !text.trim().is_empty() {
                out.push(text.trim().to_string());
            }
        },
        Value::Array(items) => {
            for item in items {
                collect_string_leaves(item, out);
            }
        },
        Value::Object(map) => {
            for item in map.values() {
                collect_string_leaves(item, out);
            }
        },
        _ => {},
    }
}

fn resolve_entity_slots_from_tier(
    tier: &V3MemoryTierRecord,
    candidates: &[CandidateText],
) -> Vec<SlotRecord> {
    let mut resolved = Vec::new();
    let Some(entries) = array_field(tier, &["entities", "value"]) else {
        return resolved;
    };
    let mut seen = BTreeSet::new();

    for entry in entries {
        let Some(name) = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some((confidence, matched_from)) = best_memory_match(candidates, name, true) else {
            continue;
        };
        if confidence < 0.75 {
            continue;
        }

        let Some(dedupe_key) = normalize_lookup_text(name) else {
            continue;
        };
        if !seen.insert(dedupe_key.clone()) {
            continue;
        }

        let entity_type = entry
            .get("type")
            .or_else(|| entry.get("entity_type"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let attributes = entry
            .get("attributes")
            .or_else(|| entry.get("facts"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let source_episodes = entry
            .get("source_episodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();

        let mut value = Map::new();
        value.insert("name".to_string(), Value::String(name.to_string()));
        if let Some(entity_type) = entity_type {
            value.insert("type".to_string(), Value::String(entity_type));
        }
        if !attributes.is_empty() {
            value.insert("attributes".to_string(), Value::Object(attributes));
        }
        value.insert(
            "matched_from".to_string(),
            Value::String(matched_from.to_string()),
        );
        value.insert(
            "memory_tier".to_string(),
            Value::String("entities".to_string()),
        );

        resolved.push(memory_slot_record(
            &make_memory_slot_id("entity", &dedupe_key),
            SlotType::Entity,
            Value::Object(value),
            confidence,
            source_episodes,
        ));
    }

    resolved
}

fn resolve_environment_slots_from_tier(
    tier: &V3MemoryTierRecord,
    candidates: &[CandidateText],
) -> Vec<SlotRecord> {
    let mut resolved = Vec::new();
    let Some(entries) = array_field(tier, &["environments", "value"]) else {
        return resolved;
    };
    let mut seen = BTreeSet::new();

    for entry in entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let environment_key = entry
            .get("environment_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let simplified_key = environment_key.and_then(|value| value.split(':').next_back());

        let mut best: Option<(f64, String, String)> = None;
        for identifier in [name, environment_key, simplified_key]
            .into_iter()
            .flatten()
        {
            let Some((score, matched_from)) = best_memory_match(candidates, identifier, false)
            else {
                continue;
            };
            if score < 0.75 {
                continue;
            }
            if best
                .as_ref()
                .map(|(current, _, _)| score > *current)
                .unwrap_or(true)
            {
                best = Some((score, identifier.to_string(), matched_from));
            }
        }

        let Some((confidence, identifier, matched_from)) = best else {
            continue;
        };
        let Some(dedupe_key) = normalize_lookup_text(&identifier) else {
            continue;
        };
        if !seen.insert(dedupe_key.clone()) {
            continue;
        }

        let mut value = Map::new();
        if let Some(name) = name {
            value.insert("name".to_string(), Value::String(name.to_string()));
        }
        if let Some(environment_key) = environment_key {
            value.insert(
                "environment_key".to_string(),
                Value::String(environment_key.to_string()),
            );
        }
        for field in [
            "kind",
            "page_type",
            "layout_notes",
            "successful_patterns",
            "auth_required",
        ] {
            if let Some(text) = entry.get(field).and_then(Value::as_str) {
                value.insert(field.to_string(), Value::String(text.to_string()));
            }
        }
        if let Some(blockers) = entry.get("known_blockers") {
            value.insert("known_blockers".to_string(), blockers.clone());
        }
        value.insert(
            "matched_from".to_string(),
            Value::String(matched_from.to_string()),
        );
        value.insert(
            "memory_tier".to_string(),
            Value::String("environment_knowledge".to_string()),
        );

        let mut evidence_links = Vec::new();
        if let Some(environment_key) = environment_key {
            evidence_links.push(environment_key.to_string());
        }

        resolved.push(memory_slot_record(
            &make_memory_slot_id("environment", &dedupe_key),
            SlotType::Resource,
            Value::Object(value),
            confidence,
            evidence_links,
        ));
    }

    resolved
}

fn resolve_insight_slots_from_tier(tier: &V3MemoryTierRecord, query: &str) -> Vec<SlotRecord> {
    let query_lower = query.to_lowercase();
    let Some(entries) = array_field(tier, &["distilled_insights", "insights", "value"]) else {
        return Vec::new();
    };
    let mut resolved = Vec::new();

    for entry in entries {
        let Some(insight_text) = entry
            .get("insight")
            .or_else(|| entry.get("description"))
            .or_else(|| entry.get("pattern"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let confidence = entry
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.65)
            .max(0.65)
            .min(0.8);
        let insight_lower = insight_text.to_lowercase();

        if insight_lower.contains("markdown") && query_requests_formatted_output(&query_lower) {
            resolved.push(memory_slot_record(
                "insight_markdown_format",
                SlotType::Modifier,
                json!({
                    "format": "markdown",
                    "insight": insight_text,
                    "memory_tier": "insights"
                }),
                confidence,
                vec!["insights:markdown".to_string()],
            ));
        } else if insight_lower.contains("dashboard")
            && query_requests_formatted_output(&query_lower)
        {
            resolved.push(memory_slot_record(
                "insight_dashboard_preference",
                SlotType::Modifier,
                json!({
                    "presentation": "dashboard",
                    "insight": insight_text,
                    "memory_tier": "insights"
                }),
                confidence,
                vec!["insights:dashboard".to_string()],
            ));
        } else if insight_lower.contains("morning")
            && query_requests_schedule_defaults(&query_lower)
        {
            resolved.push(memory_slot_record(
                "insight_morning_schedule",
                SlotType::Temporal,
                json!({
                    "preferred_window": "morning",
                    "insight": insight_text,
                    "memory_tier": "insights"
                }),
                confidence,
                vec!["insights:morning".to_string()],
            ));
        }
    }

    resolved
}

fn query_requests_formatted_output(query_lower: &str) -> bool {
    [
        "summary",
        "summarize",
        "report",
        "write",
        "draft",
        "format",
        "output",
        "table",
        "dashboard",
        "document",
    ]
    .iter()
    .any(|needle| query_lower.contains(needle))
}

fn query_requests_schedule_defaults(query_lower: &str) -> bool {
    [
        "schedule",
        "when",
        "time",
        "calendar",
        "remind",
        "tomorrow",
        "morning",
        "afternoon",
        "evening",
    ]
    .iter()
    .any(|needle| query_lower.contains(needle))
}

fn array_field<'a>(tier: &'a V3MemoryTierRecord, keys: &[&str]) -> Option<&'a Vec<Value>> {
    for key in keys {
        if let Some(Value::Array(items)) = tier.fields.get(*key) {
            return Some(items);
        }
    }
    None
}

fn best_memory_match(
    candidates: &[CandidateText],
    target: &str,
    exact_match_eligible: bool,
) -> Option<(f64, String)> {
    let target_normalized = normalize_lookup_text(target)?;
    let mut best: Option<(f64, String)> = None;

    for candidate in candidates {
        let Some(candidate_normalized) = normalize_lookup_text(&candidate.text) else {
            continue;
        };
        let score = if candidate.exact_source
            && exact_match_eligible
            && candidate_normalized == target_normalized
        {
            0.95
        } else if contains_whole_phrase(&candidate_normalized, &target_normalized)
            || contains_whole_phrase(&target_normalized, &candidate_normalized)
        {
            0.75
        } else if candidate.exact_source
            && token_overlap_ratio(&candidate_normalized, &target_normalized) >= 0.8
        {
            0.75
        } else {
            0.0
        };

        if score <= 0.0 {
            continue;
        }
        if best
            .as_ref()
            .map(|(current, _)| score > *current)
            .unwrap_or(true)
        {
            best = Some((score, candidate.text.clone()));
        }
    }

    best
}

fn normalize_lookup_text(raw: &str) -> Option<String> {
    let mut normalized = String::with_capacity(raw.len());
    let mut last_was_space = false;

    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | ':' | '/' | '.') {
            Some(' ')
        } else {
            None
        };

        let Some(mapped) = mapped else {
            continue;
        };
        if mapped == ' ' {
            if !last_was_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            last_was_space = true;
        } else {
            normalized.push(mapped);
            last_was_space = false;
        }
    }

    let normalized = normalized.trim().to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn contains_whole_phrase(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack == needle
        || haystack.contains(&format!(" {needle} "))
        || haystack.starts_with(&format!("{needle} "))
        || haystack.ends_with(&format!(" {needle}"))
}

fn token_overlap_ratio(left: &str, right: &str) -> f64 {
    let left_tokens: BTreeSet<&str> = left.split_whitespace().collect();
    let right_tokens: BTreeSet<&str> = right.split_whitespace().collect();
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let overlap = left_tokens.intersection(&right_tokens).count() as f64;
    overlap / left_tokens.len().max(right_tokens.len()) as f64
}

fn make_memory_slot_id(prefix: &str, raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>();
    format!("{prefix}_{sanitized}")
}

fn memory_slot_record(
    id: &str,
    slot_type: SlotType,
    value: Value,
    confidence: f64,
    evidence_links: Vec<String>,
) -> SlotRecord {
    let now = Utc::now();
    SlotRecord {
        id: id.to_string(),
        slot_type,
        value,
        confidence,
        provenance: vec![ProvenanceRecord {
            source: ProvenanceSource::MemoryLookup,
            timestamp: now,
        }],
        evidence_links,
        created_at: now,
        updated_at: now,
    }
}

fn merge_memory_slots(slots: &mut Vec<SlotRecord>, memory_slots: Vec<SlotRecord>) {
    for memory_slot in memory_slots {
        if let Some(index) = slots
            .iter()
            .position(|existing| slot_records_match(existing, &memory_slot))
        {
            merge_memory_slot(&mut slots[index], memory_slot);
        } else {
            slots.push(memory_slot);
        }
    }
}

fn slot_records_match(left: &SlotRecord, right: &SlotRecord) -> bool {
    if left.slot_type != right.slot_type {
        return false;
    }

    let left_keys = slot_lookup_keys(left);
    let right_keys = slot_lookup_keys(right);
    if left_keys.is_empty() || right_keys.is_empty() {
        return false;
    }

    left_keys.iter().any(|left_key| {
        right_keys.iter().any(|right_key| {
            left_key == right_key
                || (left_key.len() >= 4 && right_key.contains(left_key))
                || (right_key.len() >= 4 && left_key.contains(right_key))
        })
    })
}

fn slot_lookup_keys(slot: &SlotRecord) -> Vec<String> {
    let mut keys = Vec::new();
    collect_string_leaves(&slot.value, &mut keys);
    keys.into_iter()
        .filter_map(|key| normalize_lookup_text(&key))
        .collect()
}

fn merge_memory_slot(existing: &mut SlotRecord, memory_slot: SlotRecord) {
    existing.confidence = existing.confidence.max(memory_slot.confidence);
    existing.value = merge_slot_values(existing.value.clone(), memory_slot.value);
    existing.provenance.extend(memory_slot.provenance);
    existing.evidence_links.extend(memory_slot.evidence_links);
    existing.evidence_links.sort();
    existing.evidence_links.dedup();
    existing.touch();
}

fn merge_slot_values(existing: Value, incoming: Value) -> Value {
    match (existing, incoming) {
        (Value::Object(mut existing), Value::Object(incoming)) => {
            for (key, value) in incoming {
                existing.insert(key, value);
            }
            Value::Object(existing)
        },
        (_, incoming) => incoming,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::*;
    use crate::magician_v2::agents::memory_tiers::{
        MemoryTierDefinition, RenderConfig, RetentionMode, TierScope,
    };
    use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
    use crate::magician_v2::pipeline::artifact::{ArtifactStore, ArtifactType};
    use crate::magician_v2::prompts::{
        constants, storage::PromptStore, types::PromptCategory, Prompt, PromptManager,
    };
    use crate::magician_v2::slot_graph::extraction::{
        ExtractionConfig, LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService,
    };

    struct MockLlmService {
        responses: Mutex<Vec<String>>,
    }

    impl MockLlmService {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl LlmService for MockLlmService {
        async fn call_function(
            &self,
            _request: LlmFunctionCallRequest,
        ) -> Result<LlmFunctionCallResponse> {
            let mut guard = self
                .responses
                .lock()
                .expect("mock responses mutex poisoned");
            guard
                .pop()
                .map(|raw_arguments| LlmFunctionCallResponse {
                    raw_arguments,
                    telemetry: None,
                })
                .ok_or_else(|| anyhow!("no mock response available"))
        }
    }

    struct StaticPromptStore {
        prompt: Prompt,
    }

    #[async_trait]
    impl PromptStore for StaticPromptStore {
        async fn get_prompt(&self, name: &str, version: &str) -> Result<Prompt> {
            if name == self.prompt.name && version == self.prompt.version {
                Ok(self.prompt.clone())
            } else {
                Err(anyhow!("prompt '{}' version '{}' not found", name, version))
            }
        }

        async fn list_versions(&self, name: &str) -> Result<Vec<String>> {
            if name == self.prompt.name {
                Ok(vec![self.prompt.version.clone()])
            } else {
                Ok(vec![])
            }
        }

        async fn list_prompt_names(&self) -> Result<Vec<String>> {
            Ok(vec![self.prompt.name.clone()])
        }

        async fn save_prompt(&self, _prompt: &Prompt) -> Result<()> {
            Ok(())
        }

        async fn prompt_exists(&self, name: &str, version: &str) -> Result<bool> {
            Ok(name == self.prompt.name && version == self.prompt.version)
        }

        async fn latest_version(&self, name: &str) -> Result<String> {
            if name == self.prompt.name {
                Ok(self.prompt.version.clone())
            } else {
                Err(anyhow!("prompt '{}' not found", name))
            }
        }

        async fn delete_prompt(&self, _name: &str, _version: &str) -> Result<()> {
            Ok(())
        }

        async fn initialize(&self) -> Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
    }

    fn make_prompt_manager() -> Arc<PromptManager> {
        let content = serde_json::json!({
            "system_prompt": "You are an information extraction assistant. Extract all relevant structured information.",
            "function_schema": {
                "name": "extract_slots",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "slots": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "slot_type": {"type": "string"},
                                    "value": {"type": "object"},
                                    "confidence": {"type": "number"},
                                    "rationale": {"type": "string"}
                                }
                            }
                        }
                    }
                }
            }
        })
        .to_string();

        let prompt = Prompt::new(
            constants::names::SLOT_EXTRACTION.to_string(),
            constants::versions::SLOT_EXTRACTION.to_string(),
            content,
            PromptCategory::General,
            "Test slot extraction prompt".to_string(),
            "unit-test".to_string(),
        );

        Arc::new(PromptManager::new(Arc::new(StaticPromptStore { prompt })))
    }

    fn test_tier_def(name: &str) -> MemoryTierDefinition {
        MemoryTierDefinition {
            name: name.to_string(),
            scope: TierScope::Agent,
            description: format!("test tier {name}"),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{}".to_string(),
            },
            retention: RetentionMode::Forever,
        }
    }

    async fn seed_tier(
        base_root: &std::path::Path,
        tier_name: &str,
        tier_definitions: &[MemoryTierDefinition],
        fields: HashMap<String, Value>,
    ) {
        let memory_service = AgentMemoryResolver::new(base_root)
            .resolve_for_scope("anonymous", "default")
            .expect("memory scope");
        let mut record = V3MemoryTierRecord::new(
            tier_name,
            TierScope::Agent,
            None,
            Some("anonymous"),
            Some("default"),
            Some("personal-assistant"),
        );
        record.fields = fields;
        memory_service
            .save_native_tier_by_name(
                "personal-assistant",
                tier_name,
                tier_definitions,
                None,
                &record,
            )
            .await
            .expect("save tier");
    }

    fn test_context(tier_definitions: Vec<MemoryTierDefinition>) -> PipelineContext {
        PipelineContext {
            chain_id: "chain-1".to_string(),
            cycle_id: "cycle-1".to_string(),
            workflow_id: "wf-1".to_string(),
            query: "test query".to_string(),
            iteration: 0,
            agent_id: Some("personal-assistant".to_string()),
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            tier_definitions,
            ..PipelineContext::default()
        }
    }

    #[test]
    fn slot_extractor_agent_compiles_as_trait_object() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<SlotExtractorAgent>();
    }

    #[tokio::test]
    async fn slot_extractor_auto_resolves_entity_from_scoped_memory() {
        let tmp = tempdir().unwrap();
        let tier_definitions = vec![test_tier_def("entities")];
        seed_tier(
            tmp.path(),
            "entities",
            &tier_definitions,
            HashMap::from([(
                "value".to_string(),
                json!([{
                    "name": "Jordan",
                    "type": "person",
                    "attributes": {"email": "nainisha@example.com"},
                    "source_episodes": ["ep-1"]
                }]),
            )]),
        )
        .await;

        let extractor = Arc::new(SlotExtractor::new(
            Arc::new(MockLlmService::new(vec![r#"{"slots":[]}"#.to_string()])),
            make_prompt_manager(),
            ExtractionConfig::default(),
        ));
        let agent = SlotExtractorAgent::new(extractor, AgentMemoryResolver::new(tmp.path()));
        let mut store = ArtifactStore::new("chain-1".to_string());
        let mut context = test_context(tier_definitions);
        context.query = "Is Jordan up?".to_string();

        let _ = agent.execute(&mut store, &context).await.unwrap();

        let artifact = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("slot graph artifact");
        let slots: Vec<SlotRecord> = serde_json::from_value(artifact.content.clone()).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot_type, SlotType::Entity);
        assert_eq!(
            slots[0].value.get("name").and_then(Value::as_str),
            Some("Jordan")
        );
        assert!(slots[0].confidence >= 0.75);
        assert!(slots[0]
            .provenance
            .iter()
            .any(|record| record.source == ProvenanceSource::MemoryLookup));
    }

    #[tokio::test]
    async fn slot_extractor_merges_memory_match_into_low_confidence_entity_slot() {
        let tmp = tempdir().unwrap();
        let tier_definitions = vec![test_tier_def("entities")];
        seed_tier(
            tmp.path(),
            "entities",
            &tier_definitions,
            HashMap::from([(
                "value".to_string(),
                json!([{
                    "name": "Jordan",
                    "type": "person",
                    "attributes": {"email": "nainisha@example.com"}
                }]),
            )]),
        )
        .await;

        let extractor = Arc::new(SlotExtractor::new(
            Arc::new(MockLlmService::new(vec![r#"{
                "slots":[
                    {
                        "slot_type":"entity",
                        "value":{"name":"Jordan"},
                        "confidence":0.2,
                        "rationale":"User mentioned someone"
                    }
                ]
            }"#
            .to_string()])),
            make_prompt_manager(),
            ExtractionConfig::default(),
        ));
        let agent = SlotExtractorAgent::new(extractor, AgentMemoryResolver::new(tmp.path()));
        let mut store = ArtifactStore::new("chain-1".to_string());
        let mut context = test_context(tier_definitions);
        context.query = "Is Jordan up?".to_string();

        let _ = agent.execute(&mut store, &context).await.unwrap();

        let artifact = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("slot graph artifact");
        let slots: Vec<SlotRecord> = serde_json::from_value(artifact.content.clone()).unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot_type, SlotType::Entity);
        assert!(slots[0].confidence >= 0.95);
        assert_eq!(
            slots[0]
                .value
                .get("attributes")
                .and_then(Value::as_object)
                .and_then(|attrs| attrs.get("email"))
                .and_then(Value::as_str),
            Some("nainisha@example.com")
        );
    }

    #[tokio::test]
    async fn slot_extractor_resolves_environment_and_insight_preferences() {
        let tmp = tempdir().unwrap();
        let tier_definitions = vec![
            test_tier_def("environment_knowledge"),
            test_tier_def("insights"),
        ];
        seed_tier(
            tmp.path(),
            "environment_knowledge",
            &tier_definitions,
            HashMap::from([(
                "value".to_string(),
                json!([{
                    "name": "executive assistant",
                    "environment_key": "tool:executive-assistant",
                    "kind": "tool",
                    "page_type": "agent delegation",
                    "successful_patterns": "Delegate work to the executive assistant"
                }]),
            )]),
        )
        .await;
        seed_tier(
            tmp.path(),
            "insights",
            &tier_definitions,
            HashMap::from([(
                "distilled_insights".to_string(),
                json!([{
                    "insight": "Output is consistently delivered in Markdown format.",
                    "confidence": 0.7
                }]),
            )]),
        )
        .await;

        let extractor = Arc::new(SlotExtractor::new(
            Arc::new(MockLlmService::new(vec![r#"{"slots":[]}"#.to_string()])),
            make_prompt_manager(),
            ExtractionConfig::default(),
        ));
        let agent = SlotExtractorAgent::new(extractor, AgentMemoryResolver::new(tmp.path()));
        let mut store = ArtifactStore::new("chain-1".to_string());
        let mut context = test_context(tier_definitions);
        context.query =
            "Ask the executive assistant to check the calendar and send me a summary".to_string();

        let _ = agent.execute(&mut store, &context).await.unwrap();

        let artifact = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("slot graph artifact");
        let slots: Vec<SlotRecord> = serde_json::from_value(artifact.content.clone()).unwrap();
        assert!(slots.iter().any(|slot| {
            slot.slot_type == SlotType::Resource
                && slot.value.get("environment_key").and_then(Value::as_str)
                    == Some("tool:executive-assistant")
        }));
        assert!(slots.iter().any(|slot| {
            slot.slot_type == SlotType::Modifier
                && slot.value.get("format").and_then(Value::as_str) == Some("markdown")
        }));
    }

    #[tokio::test]
    async fn slot_extractor_skips_memory_lookup_without_scope() {
        let tmp = tempdir().unwrap();
        let tier_definitions = vec![test_tier_def("entities")];
        seed_tier(
            tmp.path(),
            "entities",
            &tier_definitions,
            HashMap::from([(
                "value".to_string(),
                json!([{
                    "name": "Jordan",
                    "type": "person"
                }]),
            )]),
        )
        .await;

        let extractor = Arc::new(SlotExtractor::new(
            Arc::new(MockLlmService::new(vec![r#"{
                "slots":[
                    {
                        "slot_type":"entity",
                        "value":{"name":"Jordan"},
                        "confidence":0.2,
                        "rationale":"User mentioned someone"
                    }
                ]
            }"#
            .to_string()])),
            make_prompt_manager(),
            ExtractionConfig::default(),
        ));
        let agent = SlotExtractorAgent::new(extractor, AgentMemoryResolver::new(tmp.path()));
        let mut store = ArtifactStore::new("chain-1".to_string());
        let mut context = test_context(tier_definitions);
        context.query = "Is Jordan up?".to_string();
        context.principal = None;
        context.workspace = None;

        let _ = agent.execute(&mut store, &context).await.unwrap();

        let artifact = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("slot graph artifact");
        let slots: Vec<SlotRecord> = serde_json::from_value(artifact.content.clone()).unwrap();
        assert_eq!(slots.len(), 1);
        assert!(slots[0].confidence < 0.95);
        assert!(!slots[0]
            .provenance
            .iter()
            .any(|record| record.source == ProvenanceSource::MemoryLookup));
    }
}
