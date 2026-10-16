//! Phase 3 memory tier interpreter scaffolding.

use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use serde_json::{Map, Value};
use tracing::{debug, warn};

use crate::magician_v2::artifact_v2::memory::{V3EpisodeRecord, V3MemoryTierRecord};

use super::{
    memory::{AgentMemoryError, AgentMemoryService},
    memory_tiers::{
        BuiltinTransform, ConsolidationTransform, ConsolidationTrigger, MemoryConsolidationRule,
        MemoryRenderer, MemoryTierDefinition, SourceRef, StrategyRecord, TierFieldSchema,
        TierScope,
    },
    types::AgentDefinition,
};

#[derive(Debug, Clone, Default)]
pub struct DefaultMemoryRenderer;

impl MemoryRenderer for DefaultMemoryRenderer {
    fn render(&self, tier: &MemoryTierDefinition, data: &V3MemoryTierRecord) -> String {
        let original = &tier.render.template;

        // Build the substitution lookup ONCE, then do a single scan
        // over the template. Previously this was a chain of
        // `String::replace` calls — each pass operated on the OUTPUT
        // of the previous one, so a value that happened to contain
        // `{another_field}` got re-substituted on the next iteration
        // (HashMap-order-dependent, so the rendered output became
        // run-dependent for tiers whose data crossed paths). The
        // single-pass form treats the template as the source of
        // truth and only substitutes its placeholders, never the
        // output's.
        let collection_field = primary_collection_field(tier);
        let last_updated = data.last_updated.to_rfc3339();
        let resolve = |token: &str| -> Option<String> {
            if token == "tier_name" {
                return Some(data.tier_name.clone());
            }
            if token == "last_updated" {
                return Some(last_updated.clone());
            }
            if let Some(value) = data.fields.get(token) {
                return Some(value_to_text(value));
            }
            // Collection-field special case: the placeholder names
            // the schema's collection field but the value lives
            // under the legacy `value` key (older records or tiers
            // whose storage path wrote under the generic name). This
            // ONLY applies to the tier's `primary_collection_field`
            // — other placeholders that miss return None and trigger
            // the unresolved-placeholder fallback below.
            if Some(token) == collection_field {
                if let Some(value) = data.fields.get("value") {
                    return Some(value_to_text(value));
                }
                // Resilience: an LLM consolidation often persists the collection
                // under a NON-canonical key it chose itself (e.g. `research_patterns`
                // instead of the declared `{token}`), and the write-path coercion
                // can't salvage a nested-object shape. When the tier holds exactly
                // ONE field, that field IS the collection regardless of its key —
                // render it instead of falling through to the unresolved-placeholder
                // WARN + structured fallback on EVERY render. Multi-field tiers stay
                // strict so a genuine template/schema bug still surfaces.
                if data.fields.len() == 1 {
                    if let Some((_, value)) = data.fields.iter().next() {
                        return Some(value_to_text(value));
                    }
                }
                if !data.fields.is_empty() {
                    // Some LLM consolidation prompts ask for a collection but emit
                    // a structured object keyed by topic instead of an `entries`
                    // array. Treat that object as the collection payload so prompt
                    // rendering remains useful and does not warn on every load.
                    return Some(render_unstructured_collection_fields(&data.fields));
                }
            }
            None
        };

        let rendered = substitute_template_placeholders(original, resolve);

        if &rendered == original || contains_unresolved_template_placeholder(&rendered) {
            // Empty record (no consolidations yet) is the common case for
            // newly-seeded agents — log at DEBUG and produce the structured
            // empty representation silently. Records WITH fields that still
            // can't resolve the template indicate a real template/data
            // mismatch worth flagging at WARN.
            if data.fields.is_empty() {
                debug!(
                    tier = %data.tier_name,
                    "Memory tier has no consolidated data yet; rendering structured empty fallback"
                );
            } else {
                warn!(
                    tier = %data.tier_name,
                    field_count = data.fields.len(),
                    "Memory tier render template had unresolved placeholders despite present fields; using structured fallback"
                );
            }
            return render_tier_fields_fallback(tier, data);
        }

        rendered
    }
}

pub fn primary_collection_field(tier: &MemoryTierDefinition) -> Option<&str> {
    let mut candidates = tier.schema.iter().filter_map(|(field, schema)| {
        matches!(schema, TierFieldSchema::Collection { .. }).then_some(field.as_str())
    });
    let first = candidates.next()?;
    if candidates.next().is_none() {
        Some(first)
    } else {
        None
    }
}

/// Reserved placeholder names that the renderer fills from
/// `V3MemoryTierRecord` metadata, not from `data.fields`. Kept as a
/// constant so the template-validation pass and the renderer agree
/// on the same set without one drifting from the other.
const RESERVED_TEMPLATE_TOKENS: &[&str] = &["tier_name", "last_updated"];

/// Walk every `MemoryTierDefinition` on the agent and warn-log any
/// `{token}` in a tier's `render.template` that has no resolution
/// path — neither a reserved name (`tier_name` / `last_updated`)
/// nor a key in the tier's `schema`. Surfaces template/schema
/// mismatches at definition load time so the operator sees ONE
/// warning per misconfigured tier instead of one warning per render
/// (which previously spammed logs for every prompt that pulled the
/// affected tier).
///
/// Soft validation — does NOT fail definition load. A mismatched
/// placeholder still falls back to the structured representation at
/// render time, which is correct behavior; this is purely
/// observability.
pub fn warn_template_schema_mismatches(definition: &AgentDefinition) {
    for tier in &definition.memory_tiers {
        let template = &tier.render.template;
        if template.trim().is_empty() {
            continue;
        }
        let mut rest = template.as_str();
        while let Some(start) = rest.find('{') {
            let after_start = &rest[start + 1..];
            let Some(end) = after_start.find('}') else {
                break;
            };
            let token = after_start[..end].trim();
            let is_valid_token = !token.is_empty()
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
            if is_valid_token {
                let resolvable =
                    RESERVED_TEMPLATE_TOKENS.contains(&token) || tier.schema.contains_key(token);
                if !resolvable {
                    warn!(
                        agent_id = %definition.agent_id,
                        tier = %tier.name,
                        placeholder = token,
                        "Memory tier render template references `{{{token}}}` which is neither a \
                         reserved token (tier_name/last_updated) nor a declared schema field; \
                         renderer will use the structured fallback at render time"
                    );
                }
            }
            rest = &after_start[end + 1..];
        }
    }
}

/// Single-pass `{token}` substitution. Walks `template` left-to-right,
/// detects each well-formed `{token}` (token = ASCII alphanumeric +
/// `_`/`-`, identical to `contains_unresolved_template_placeholder`'s
/// notion of a placeholder), and asks `resolve` for its value. When
/// `resolve` returns `None` the original `{token}` is preserved so the
/// downstream unresolved-placeholder check still fires and the
/// renderer falls back to the structured form.
///
/// The output is built into a fresh `String` — substituted values
/// don't get re-scanned, which fixes the chained-`String::replace`
/// flakiness where a value containing `{another_field}` would be
/// re-substituted on a later iteration (with HashMap-order-dependent
/// results).
fn substitute_template_placeholders<F>(template: &str, resolve: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            // Unterminated `{` — preserve the rest verbatim.
            out.push_str(&rest[start..]);
            return out;
        };
        let token = after_start[..end].trim();
        let is_valid_token = !token.is_empty()
            && token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
        if !is_valid_token {
            // Not a placeholder — keep the literal `{...}` text and
            // resume scanning after it.
            out.push_str(&rest[start..=start + end + 1]);
            rest = &after_start[end + 1..];
            continue;
        }
        match resolve(token) {
            Some(value) => out.push_str(&value),
            None => {
                // Preserve the placeholder so the unresolved-template
                // detector can pick it up downstream.
                out.push_str(&rest[start..=start + end + 1]);
            },
        }
        rest = &after_start[end + 1..];
    }
    out.push_str(rest);
    out
}

fn contains_unresolved_template_placeholder(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            return false;
        };
        let token = after_start[..end].trim();
        if !token.is_empty()
            && token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return true;
        }
        rest = &after_start[end + 1..];
    }
    false
}

fn render_tier_fields_fallback(tier: &MemoryTierDefinition, data: &V3MemoryTierRecord) -> String {
    let mut lines = vec![format!("Tier: {}", data.tier_name)];
    if let Some(collection_field) = primary_collection_field(tier) {
        if !data.fields.contains_key(collection_field) {
            if let Some(value) = data.fields.get("value") {
                lines.push(format!("{collection_field}: {}", value_to_text(value)));
            }
        }
    }
    for (field, value) in &data.fields {
        lines.push(format!("{field}: {}", value_to_text(value)));
    }
    lines.join("\n")
}

fn render_unstructured_collection_fields(fields: &HashMap<String, Value>) -> String {
    let mut keys = fields.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys.into_iter()
        .filter_map(|field| {
            fields
                .get(&field)
                .map(|value| format!("{field}: {}", value_to_text(value)))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Debug)]
pub struct MemoryTierInterpreter {
    memory_service: AgentMemoryService,
    renderer: Arc<dyn MemoryRenderer + Send + Sync>,
}

impl MemoryTierInterpreter {
    pub fn new(memory_service: AgentMemoryService) -> Self {
        Self {
            memory_service,
            renderer: Arc::new(DefaultMemoryRenderer),
        }
    }

    pub fn with_renderer(
        memory_service: AgentMemoryService,
        renderer: Arc<dyn MemoryRenderer + Send + Sync>,
    ) -> Self {
        Self {
            memory_service,
            renderer,
        }
    }

    pub async fn load_and_render(
        &self,
        agent_id: &str,
        goal_id: Option<&str>,
        tier_definitions: &[MemoryTierDefinition],
    ) -> Result<HashMap<String, String>, AgentMemoryError> {
        let mut rendered = HashMap::new();
        for tier in tier_definitions {
            let data = self
                .memory_service
                .load_native_tier(agent_id, tier, goal_id)
                .await?
                .unwrap_or_else(|| {
                    missing_native_tier_data(&tier.name, tier.scope.clone(), goal_id)
                });
            rendered.insert(tier.name.clone(), self.renderer.render(tier, &data));
        }
        Ok(rendered)
    }

    pub async fn consolidate_cycle_completed_v3(
        &self,
        definition: &AgentDefinition,
        agent_id: &str,
        goal_id: &str,
        episode: &V3EpisodeRecord,
    ) -> Result<Vec<String>, AgentMemoryError> {
        if episode.outcome_is_paused() {
            return Ok(Vec::new());
        }

        if episode.agent_id != agent_id {
            return Err(AgentMemoryError::Validation(format!(
                "episode.agent_id `{}` does not match consolidation agent_id `{}`",
                episode.agent_id, agent_id
            )));
        }
        if episode.goal_id() != goal_id {
            return Err(AgentMemoryError::Validation(format!(
                "episode.goal_id `{}` does not match consolidation goal_id `{}`",
                episode.goal_id(),
                goal_id
            )));
        }

        let mut updated_targets = Vec::new();
        for rule in &definition.memory_consolidation {
            if !matches!(rule.trigger, ConsolidationTrigger::CycleCompleted) {
                continue;
            }
            if !cycle_rule_applies_to_goal_v3(rule, goal_id) {
                continue;
            }

            let ConsolidationTransform::Structured { builtin } = &rule.transform else {
                continue;
            };

            if matches!(builtin, BuiltinTransform::PromoteSharedInsights) {
                continue;
            }

            let Some((tier_definition, field_path)) =
                (match resolve_target_tier(&definition.memory_tiers, &rule.target) {
                    Ok(resolution) => resolution,
                    Err(err) => {
                        warn!(
                            rule = %rule.name,
                            target = %rule.target,
                            error = %err,
                            "Skipping cycle-completed consolidation rule due to invalid target"
                        );
                        continue;
                    },
                })
            else {
                continue;
            };

            let tier_goal_id = tier_goal_id_for_scope(tier_definition, goal_id);
            let mut tier_data = self
                .memory_service
                .load_native_tier(agent_id, tier_definition, tier_goal_id)
                .await?
                .unwrap_or_else(|| {
                    missing_native_tier_data(
                        &tier_definition.name,
                        tier_definition.scope.clone(),
                        tier_goal_id,
                    )
                });

            apply_structured_transform_v3(builtin, field_path, episode, &mut tier_data)?;
            tier_data.last_updated = episode
                .completed_at_dt()
                .map_err(|err| AgentMemoryError::Validation(err.to_string()))?;

            self.memory_service
                .save_native_tier(agent_id, tier_definition, tier_goal_id, &tier_data)
                .await?;

            updated_targets.push(rule.target.clone());
        }

        updated_targets.sort();
        updated_targets.dedup();
        Ok(updated_targets)
    }
}

fn cycle_rule_applies_to_goal(rule: &MemoryConsolidationRule, goal_id: &str) -> bool {
    match SourceRef::parse(&rule.source) {
        Some(SourceRef::Episodes {
            goal_id: Some(source_goal_id),
            ..
        }) => source_goal_id == goal_id,
        _ => true,
    }
}

fn cycle_rule_applies_to_goal_v3(rule: &MemoryConsolidationRule, goal_id: &str) -> bool {
    cycle_rule_applies_to_goal(rule, goal_id)
}

fn resolve_target_tier<'a>(
    tier_definitions: &'a [MemoryTierDefinition],
    target: &'a str,
) -> Result<Option<(&'a MemoryTierDefinition, Option<&'a str>)>, AgentMemoryError> {
    if target.starts_with("report:") {
        return Err(AgentMemoryError::Validation(format!(
            "consolidation target `{target}` is unsupported for cycle_completed in phase 3"
        )));
    }

    let mut segments = target.splitn(2, '.');
    let tier_name = segments.next().map(str::trim).ok_or_else(|| {
        AgentMemoryError::Validation(format!(
            "consolidation target `{target}` is invalid: missing tier root"
        ))
    })?;
    if tier_name.is_empty() {
        return Err(AgentMemoryError::Validation(format!(
            "consolidation target `{target}` is invalid: empty tier root"
        )));
    }
    if tier_name == "user" {
        return Err(AgentMemoryError::Validation(format!(
            "consolidation target `{target}` is unsupported for cycle_completed in phase 3"
        )));
    }
    let field_path = segments
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let tier_definition = tier_definitions
        .iter()
        .find(|tier| tier.name == tier_name)
        .ok_or_else(|| {
            AgentMemoryError::Validation(format!(
                "consolidation target `{target}` references unknown tier root `{tier_name}`"
            ))
        })?;
    Ok(Some((tier_definition, field_path)))
}

fn tier_goal_id_for_scope<'a>(
    tier_definition: &MemoryTierDefinition,
    goal_id: &'a str,
) -> Option<&'a str> {
    if matches!(tier_definition.scope, TierScope::AgentGoal) {
        Some(goal_id)
    } else {
        None
    }
}

fn apply_structured_transform_v3(
    builtin: &BuiltinTransform,
    field_path: Option<&str>,
    episode: &V3EpisodeRecord,
    tier_data: &mut V3MemoryTierRecord,
) -> Result<(), AgentMemoryError> {
    match builtin {
        BuiltinTransform::MapEpisodeToTask => {
            let mapped = map_episode_to_task_value_v3(episode, &tier_data.fields);
            write_tier_value_v3(tier_data, field_path, mapped)
        },
        BuiltinTransform::AppendStrategyRecord => {
            let path = field_path.unwrap_or("strategy_records");
            let existing = read_tier_value(&tier_data.fields, path);
            let appended = append_strategy_record_value_v3(existing, episode);
            write_tier_value_v3(tier_data, Some(path), appended)
        },
        BuiltinTransform::AppendArchiveSummary => {
            let path = field_path.unwrap_or("summaries");
            let existing = read_tier_value(&tier_data.fields, path);
            let appended = append_archive_summary_value_v3(existing, episode);
            write_tier_value_v3(tier_data, Some(path), appended)
        },
        BuiltinTransform::PromoteSharedInsights => Ok(()),
    }
}

fn map_episode_to_task_value_v3(
    episode: &V3EpisodeRecord,
    existing_fields: &HashMap<String, Value>,
) -> Value {
    let completed_at = episode.completed_at_dt().unwrap_or_else(|_| Utc::now());
    let started_at = episode.started_at_dt().unwrap_or(completed_at);
    let mut notes = string_list_from_value(existing_fields.get("notes"));
    let note = format!(
        "{} seq {}: {}",
        completed_at.to_rfc3339(),
        episode.trigger_seq,
        episode.outcome_summary_text()
    );
    if notes.last().map(|current| current != &note).unwrap_or(true) {
        notes.push(note);
    }
    const MAX_NOTES: usize = 20;
    if notes.len() > MAX_NOTES {
        let drop_count = notes.len() - MAX_NOTES;
        notes.drain(0..drop_count);
    }

    let first_seen = existing_fields
        .get("first_seen")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| started_at.to_rfc3339());

    let status = episode_outcome_status_v3(episode).to_string();
    let items = task_items_for_outcome_v3(episode, existing_fields.get("items"));

    serde_json::json!({
        "goal_id": episode.goal_id(),
        "status": status,
        "first_seen": first_seen,
        "context_summary": episode.outcome_summary_text(),
        "items": items,
        "notes": notes,
    })
}

fn task_items_for_outcome_v3(episode: &V3EpisodeRecord, existing: Option<&Value>) -> Value {
    if episode.outcome_kind == "partial_progress" {
        return Value::Array(vec![serde_json::json!({
            "id": format!("remaining-{}", episode.trigger_seq),
            "description": episode.outcome_remaining.clone().unwrap_or_default(),
            "status": "pending",
        })]);
    }
    if episode.outcome_is_paused() {
        return Value::Array(
            episode
                .outcome_pending_actions()
                .iter()
                .enumerate()
                .map(|(idx, action)| {
                    serde_json::json!({
                        "id": format!("pending-{}-{}", episode.trigger_seq, idx + 1),
                        "description": action,
                        "status": "pending",
                    })
                })
                .collect(),
        );
    }
    if episode.outcome_is_succeeded() && episode.outcome_kind != "partial_progress" {
        return Value::Array(Vec::new());
    }
    existing
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

fn episode_outcome_status_v3(episode: &V3EpisodeRecord) -> &'static str {
    match episode.outcome_kind.as_str() {
        "goal_achieved" => "goal_achieved",
        "partial_progress" => "partial_progress",
        "failed" => "failed",
        "user_intervened" => "user_intervened",
        "budget_exhausted" => "budget_exhausted",
        "paused" => "paused",
        "circuit_open" => "circuit_open",
        _ => "failed",
    }
}

fn append_strategy_record_value_v3(existing: Option<&Value>, episode: &V3EpisodeRecord) -> Value {
    let mut records = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let record = serde_json::to_value(StrategyRecord {
        goal_id: episode.goal_id().to_string(),
        strategy_type: episode
            .strategy_summary
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        succeeded: episode.outcome_is_succeeded(),
        execution_time_ms: episode_duration_ms_v3(episode),
        actions_count: episode.actions_taken.len(),
        timestamp: episode.completed_at_dt().unwrap_or_else(|_| Utc::now()),
    })
    .unwrap_or(Value::Null);
    if !records.iter().any(|existing_item| existing_item == &record) {
        records.push(record);
    }
    Value::Array(records)
}

fn append_archive_summary_value_v3(existing: Option<&Value>, episode: &V3EpisodeRecord) -> Value {
    let mut summaries = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let summary = archive_summary_for_episode_v3(episode);
    let period = summary.get("period").and_then(Value::as_str);
    if let Some(index) = period.and_then(|period| {
        summaries
            .iter()
            .position(|item| item.get("period").and_then(Value::as_str) == Some(period))
    }) {
        summaries[index] = summary;
    } else {
        summaries.push(summary);
    }
    Value::Array(summaries)
}

pub(super) fn archive_summary_for_episode_v3(episode: &V3EpisodeRecord) -> Value {
    let outcome = episode.outcome_summary_text().trim();
    let strategy = episode
        .strategy_summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let body = if !outcome.is_empty() {
        outcome
    } else {
        strategy.unwrap_or("Execution archived")
    };
    let summary = episode
        .task_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .filter(|title| {
            !body
                .to_ascii_lowercase()
                .contains(&title.to_ascii_lowercase())
        })
        .map(|title| format!("{title}: {body}"))
        .unwrap_or_else(|| body.to_string());

    let mut key_events = episode
        .actions_taken
        .iter()
        .map(|action| action.description.trim())
        .filter(|description| !description.is_empty())
        .take(20)
        .map(|description| Value::String(description.to_string()))
        .collect::<Vec<_>>();
    if key_events.is_empty() {
        key_events.extend(
            episode
                .observations
                .iter()
                .map(|observation| observation.trim())
                .filter(|observation| !observation.is_empty())
                .take(10)
                .map(|observation| Value::String(observation.to_string())),
        );
    }

    let mut entity_mentions = Map::new();
    entity_mentions.insert(
        "goal_id".to_string(),
        Value::String(episode.goal_id().to_string()),
    );
    for (name, value) in [
        ("task_id", episode.task_id.as_deref()),
        ("execution_id", episode.execution_id.as_deref()),
        ("root_execution_id", episode.root_execution_id.as_deref()),
        ("task_title", episode.task_title.as_deref()),
    ] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            entity_mentions.insert(name.to_string(), Value::String(value.to_string()));
        }
    }

    serde_json::json!({
        "period": format!("{}..{}", episode.started_at, episode.completed_at),
        "summary": summary,
        "key_events": key_events,
        "entity_mentions": entity_mentions,
    })
}

fn episode_duration_ms_v3(episode: &V3EpisodeRecord) -> u64 {
    let started = episode.started_at_dt().ok();
    let completed = episode.completed_at_dt().ok();
    match (started, completed) {
        (Some(started), Some(completed)) => completed
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0) as u64,
        _ => 0,
    }
}

fn write_tier_value_v3(
    tier_data: &mut V3MemoryTierRecord,
    field_path: Option<&str>,
    value: Value,
) -> Result<(), AgentMemoryError> {
    match field_path {
        Some(path) => write_value_at_path(&mut tier_data.fields, path, value),
        None => match value {
            Value::Object(map) => {
                for (key, entry) in map {
                    tier_data.fields.insert(key, entry);
                }
                Ok(())
            },
            other => Err(AgentMemoryError::Validation(format!(
                "structured consolidation requires object root for tier `{}` but received `{}`",
                tier_data.tier_name, other
            ))),
        },
    }
}

fn write_value_at_path(
    fields: &mut HashMap<String, Value>,
    path: &str,
    value: Value,
) -> Result<(), AgentMemoryError> {
    let segments = path
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(AgentMemoryError::Validation(
            "consolidation target path must not be empty".to_string(),
        ));
    }

    if segments.len() == 1 {
        fields.insert(segments[0].to_string(), value);
        return Ok(());
    }

    let mut current = fields
        .entry(segments[0].to_string())
        .or_insert_with(|| Value::Object(Map::new()));

    for segment in &segments[1..segments.len().saturating_sub(1)] {
        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let Some(map) = current.as_object_mut() else {
            return Err(AgentMemoryError::Validation(
                "failed to materialize object path for consolidation target".to_string(),
            ));
        };
        current = map
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }

    if !current.is_object() {
        *current = Value::Object(Map::new());
    }
    let Some(map) = current.as_object_mut() else {
        return Err(AgentMemoryError::Validation(
            "failed to materialize leaf object for consolidation target".to_string(),
        ));
    };
    map.insert(segments[segments.len() - 1].to_string(), value);
    Ok(())
}

fn read_tier_value<'a>(fields: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    let mut segments = path
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty());
    let first = segments.next()?;
    let mut current = fields.get(first)?;
    for segment in segments {
        match current {
            Value::Object(map) => current = map.get(segment)?,
            _ => return None,
        }
    }
    Some(current)
}

fn string_list_from_value(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn missing_native_tier_data(
    tier_name: &str,
    tier_scope: TierScope,
    goal_id: Option<&str>,
) -> V3MemoryTierRecord {
    let mut record =
        V3MemoryTierRecord::new(tier_name.to_string(), tier_scope, goal_id, None, None, None);
    record.last_updated =
        chrono::DateTime::<Utc>::from_timestamp_millis(0).expect("unix epoch should be valid");
    record
}

fn value_to_text(value: &Value) -> String {
    let mut out = Vec::new();
    flatten_value("", value, &mut out);
    out.join("; ")
}

fn flatten_value(path: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Null => {
            if path.is_empty() {
                out.push("none".to_string());
            } else {
                out.push(format!("{path}: none"));
            }
        },
        Value::Bool(flag) => {
            if path.is_empty() {
                out.push(flag.to_string());
            } else {
                out.push(format!("{path}: {flag}"));
            }
        },
        Value::Number(number) => {
            if path.is_empty() {
                out.push(number.to_string());
            } else {
                out.push(format!("{path}: {number}"));
            }
        },
        Value::String(text) => {
            let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if path.is_empty() {
                out.push(compact);
            } else {
                out.push(format!("{path}: {compact}"));
            }
        },
        Value::Array(items) => {
            if items.is_empty() {
                if path.is_empty() {
                    out.push("none".to_string());
                } else {
                    out.push(format!("{path}: none"));
                }
                return;
            }
            for (idx, item) in items.iter().enumerate() {
                let child = if path.is_empty() {
                    format!("item {}", idx + 1)
                } else {
                    format!("{path} item {}", idx + 1)
                };
                flatten_value(&child, item, out);
            }
        },
        Value::Object(map) => {
            if map.is_empty() {
                if path.is_empty() {
                    out.push("none".to_string());
                } else {
                    out.push(format!("{path}: none"));
                }
                return;
            }
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(value) = map.get(&key) {
                    flatten_value(&child, value, out);
                }
            }
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::magician_v2::agents::{
        memory::EpisodeOutcome,
        memory_tiers::{
            ActionSummary, BuiltinTransform, ConsolidationTransform, ConsolidationTrigger,
            MemoryConsolidationRule, RenderConfig, RetentionMode, TierScope,
        },
        AgentDefinition,
    };
    use chrono::{Duration, Utc};
    use tempfile::tempdir;

    fn tier_definition() -> MemoryTierDefinition {
        MemoryTierDefinition {
            name: "task_progress".to_string(),
            scope: TierScope::Agent,
            description: "Task progress".to_string(),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "text".to_string(),
                template: "Tier {tier_name}: {summary}".to_string(),
            },
            retention: RetentionMode::Forever,
        }
    }

    fn native_episode_record(
        _memory: &AgentMemoryService,
        episode: &V3EpisodeRecord,
    ) -> V3EpisodeRecord {
        episode.clone()
    }

    fn native_tier_record(
        memory: &AgentMemoryService,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
        fields: serde_json::Map<String, Value>,
    ) -> V3MemoryTierRecord {
        let scope = memory.scoped_memory_scope();
        let mut record = V3MemoryTierRecord::new(
            tier_definition.name.clone(),
            tier_definition.scope.clone(),
            goal_id,
            scope.map(|(principal, _)| principal),
            scope.map(|(_, workspace)| workspace),
            if matches!(tier_definition.scope, TierScope::User) {
                None
            } else {
                Some(agent_id)
            },
        );
        record.fields = fields.into_iter().collect();
        record
    }

    #[tokio::test]
    async fn load_and_render_reads_saved_tier() {
        let tmp = tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory.ensure_agent_layout("a1").await.unwrap();

        let def = tier_definition();
        let data_fields = [(
            "summary".to_string(),
            Value::String("2 pending tasks".to_string()),
        )]
        .into_iter()
        .collect();
        memory
            .save_native_tier(
                "a1",
                &def,
                None,
                &native_tier_record(&memory, "a1", &def, None, data_fields),
            )
            .await
            .unwrap();

        let interpreter = MemoryTierInterpreter::new(memory);
        let rendered = interpreter
            .load_and_render("a1", None, std::slice::from_ref(&def))
            .await
            .unwrap();
        assert_eq!(
            rendered.get("task_progress"),
            Some(&"Tier task_progress: 2 pending tasks".to_string())
        );
    }

    #[tokio::test]
    async fn load_and_render_missing_tier_uses_stable_never_updated_timestamp() {
        let tmp = tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        let def = MemoryTierDefinition {
            name: "task_progress".to_string(),
            scope: TierScope::Agent,
            description: "Task progress".to_string(),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "text".to_string(),
                template: "Tier {tier_name} last updated {last_updated}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let interpreter = MemoryTierInterpreter::new(memory);
        let first = interpreter
            .load_and_render("a1", None, std::slice::from_ref(&def))
            .await
            .unwrap();
        let second = interpreter
            .load_and_render("a1", None, &[def])
            .await
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.get("task_progress"),
            Some(&"Tier task_progress last updated 1970-01-01T00:00:00+00:00".to_string())
        );
    }

    #[tokio::test]
    async fn scope_aware_load_and_render_supports_agent_agent_goal_and_user_tiers() {
        let tmp = tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory.ensure_agent_layout("a1").await.unwrap();

        let agent_tier = MemoryTierDefinition {
            name: "agent_context".to_string(),
            scope: TierScope::Agent,
            description: "agent scope".to_string(),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "text".to_string(),
                template: "A:{summary}".to_string(),
            },
            retention: RetentionMode::Forever,
        };
        let agent_goal_tier = MemoryTierDefinition {
            name: "task_progress".to_string(),
            scope: TierScope::AgentGoal,
            description: "goal scope".to_string(),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "text".to_string(),
                template: "G:{summary}".to_string(),
            },
            retention: RetentionMode::GoalLifetime,
        };
        let user_tier = MemoryTierDefinition {
            name: "knowledge".to_string(),
            scope: TierScope::User,
            description: "user scope".to_string(),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "text".to_string(),
                template: "U:{summary}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let agent_fields = [(
            "summary".to_string(),
            Value::String("agent-memory".to_string()),
        )]
        .into_iter()
        .collect();
        memory
            .save_native_tier(
                "a1",
                &agent_tier,
                None,
                &native_tier_record(&memory, "a1", &agent_tier, None, agent_fields),
            )
            .await
            .unwrap();

        let goal_fields = [(
            "summary".to_string(),
            Value::String("goal-memory".to_string()),
        )]
        .into_iter()
        .collect();
        memory
            .save_native_tier(
                "a1",
                &agent_goal_tier,
                Some("g1"),
                &native_tier_record(&memory, "a1", &agent_goal_tier, Some("g1"), goal_fields),
            )
            .await
            .unwrap();

        let user_fields = [(
            "summary".to_string(),
            Value::String("user-memory".to_string()),
        )]
        .into_iter()
        .collect();
        memory
            .save_native_tier(
                "ignored",
                &user_tier,
                None,
                &native_tier_record(&memory, "ignored", &user_tier, None, user_fields),
            )
            .await
            .unwrap();

        let interpreter = MemoryTierInterpreter::new(memory.clone());
        let rendered = interpreter
            .load_and_render(
                "a1",
                Some("g1"),
                &[
                    agent_tier.clone(),
                    agent_goal_tier.clone(),
                    user_tier.clone(),
                ],
            )
            .await
            .unwrap();

        assert_eq!(
            rendered.get("agent_context"),
            Some(&"A:agent-memory".to_string())
        );
        assert_eq!(
            rendered.get("task_progress"),
            Some(&"G:goal-memory".to_string())
        );
        assert_eq!(
            rendered.get("knowledge"),
            Some(&"U:user-memory".to_string())
        );

        let storage = memory.storage();
        assert!(storage
            .agent_tier_path("a1", "agent_context", &TierScope::Agent, None)
            .unwrap()
            .exists());
        assert!(storage
            .agent_tier_path("a1", "task_progress", &TierScope::AgentGoal, Some("g1"))
            .unwrap()
            .exists());
        assert!(storage
            .agent_tier_path("a1", "knowledge", &TierScope::User, None)
            .unwrap()
            .exists());
    }

    #[tokio::test]
    async fn consolidate_cycle_completed_updates_task_progress_and_strategy_records() {
        let tmp = tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory.ensure_agent_layout("a1").await.unwrap();
        let interpreter = MemoryTierInterpreter::new(memory.clone());

        let definition = AgentDefinition::from_yaml_str(
            r#"
agent_id: "a1"
name: "A1"
persona: "Planner"
tools: []
memory_tiers:
  - name: "task_progress"
    scope: "agent_goal"
    description: "Track goal progress"
    schema:
      context_summary: { type: text }
      items: { type: collection }
      notes: { type: collection }
      status: { type: text }
      goal_id: { type: text }
      first_seen: { type: text }
    render:
      format: "text"
      template: "{context_summary}"
    retention: "goal_lifetime"
  - name: "knowledge"
    scope: "agent"
    description: "Track strategy"
    schema:
      strategy_records: { type: collection }
    render:
      format: "text"
      template: "{strategy_records}"
    retention: "forever"
memory_consolidation:
  - name: "update_task_progress"
    trigger: "cycle_completed"
    source: "episodes(g1, limit=1)"
    target: "task_progress"
    transform:
      type: "structured"
      builtin: "map_episode_to_task"
  - name: "record_strategy"
    trigger: "cycle_completed"
    source: "episodes(g1, limit=1)"
    target: "knowledge.strategy_records"
    transform:
      type: "structured"
      builtin: "append_strategy_record"
  - name: "phase4_llm_rule"
    trigger: "cycle_completed"
    source: "episodes(g1, limit=1)"
    target: "knowledge.insights"
    transform:
      type: "llm"
      prompt: "phase4-only"
"#,
        )
        .unwrap();

        let started_at = Utc::now();
        let episode = V3EpisodeRecord::new_memory_episode(
            None,
            "a1".to_string(),
            "cycle-1".to_string(),
            "g1".to_string(),
            "manual".to_string(),
            1,
            started_at,
            None,
            started_at,
            started_at + Duration::milliseconds(2500),
            &EpisodeOutcome::PartialProgress {
                summary: "Gathered two candidates".to_string(),
                remaining: "Submit applications".to_string(),
            },
            vec![ActionSummary {
                action_type: "search".to_string(),
                description: "Searched job boards".to_string(),
                tool: "browser.search".to_string(),
                succeeded: true,
                duration_ms: Some(900),
                metadata: HashMap::new(),
            }],
            Vec::new(),
            Vec::new(),
            Some("GuidedSearch".to_string()),
            None,
            None,
        );

        let updated_targets = interpreter
            .consolidate_cycle_completed_v3(
                &definition,
                "a1",
                "g1",
                &native_episode_record(&memory, &episode),
            )
            .await
            .unwrap();
        assert_eq!(
            updated_targets,
            vec![
                "knowledge.strategy_records".to_string(),
                "task_progress".to_string()
            ]
        );

        let task_def = definition
            .memory_tiers
            .iter()
            .find(|tier| tier.name == "task_progress")
            .unwrap();
        let task_data = memory
            .load_native_tier("a1", task_def, Some("g1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            task_data.fields.get("context_summary"),
            Some(&Value::String("Gathered two candidates".to_string()))
        );
        assert_eq!(
            task_data.fields.get("status"),
            Some(&Value::String("partial_progress".to_string()))
        );
        assert!(task_data
            .fields
            .get("items")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()));

        let knowledge_def = definition
            .memory_tiers
            .iter()
            .find(|tier| tier.name == "knowledge")
            .unwrap();
        let knowledge_data = memory
            .load_native_tier("a1", knowledge_def, None)
            .await
            .unwrap()
            .unwrap();
        let strategy_records = knowledge_data
            .fields
            .get("strategy_records")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(strategy_records.len(), 1);
        assert_eq!(
            strategy_records[0].get("goal_id"),
            Some(&Value::String("g1".to_string()))
        );
        assert_eq!(
            strategy_records[0].get("strategy_type"),
            Some(&Value::String("GuidedSearch".to_string()))
        );

        // Re-applying same episode should not duplicate strategy entries.
        interpreter
            .consolidate_cycle_completed_v3(
                &definition,
                "a1",
                "g1",
                &native_episode_record(&memory, &episode),
            )
            .await
            .unwrap();
        let knowledge_data = memory
            .load_native_tier("a1", knowledge_def, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            knowledge_data
                .fields
                .get("strategy_records")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert!(knowledge_data.fields.get("insights").is_none());
    }

    #[tokio::test]
    async fn consolidate_cycle_completed_skips_paused_episode() {
        let tmp = tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory.ensure_agent_layout("a1").await.unwrap();
        let interpreter = MemoryTierInterpreter::new(memory.clone());

        let definition = AgentDefinition::from_yaml_str(
            r#"
agent_id: "a1"
name: "A1"
persona: "Planner"
tools: []
memory_tiers:
  - name: "task_progress"
    scope: "agent_goal"
    description: "Track goal progress"
    schema:
      context_summary: { type: text }
    render:
      format: "text"
      template: "{context_summary}"
    retention: "goal_lifetime"
memory_consolidation:
  - name: "update_task_progress"
    trigger: "cycle_completed"
    source: "episodes(g1, limit=1)"
    target: "task_progress"
    transform:
      type: "structured"
      builtin: "map_episode_to_task"
"#,
        )
        .unwrap();

        let started_at = Utc::now();
        let paused_episode = V3EpisodeRecord::new_memory_episode(
            None,
            "a1".to_string(),
            "cycle-paused".to_string(),
            "g1".to_string(),
            "manual".to_string(),
            7,
            started_at,
            None,
            started_at,
            started_at + Duration::seconds(1),
            &EpisodeOutcome::Paused {
                pending_actions: vec!["approve write".to_string()],
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some("GuidedSearch".to_string()),
            None,
            None,
        );

        let updated_targets = interpreter
            .consolidate_cycle_completed_v3(
                &definition,
                "a1",
                "g1",
                &native_episode_record(&memory, &paused_episode),
            )
            .await
            .unwrap();
        assert!(updated_targets.is_empty());

        let task_def = definition
            .memory_tiers
            .iter()
            .find(|tier| tier.name == "task_progress")
            .unwrap();
        let task_data = memory
            .load_native_tier("a1", task_def, Some("g1"))
            .await
            .unwrap();
        assert!(task_data.is_none());
    }

    #[tokio::test]
    async fn consolidate_cycle_completed_ignores_unsupported_user_target_but_applies_valid_rules() {
        let tmp = tempdir().unwrap();
        let memory = AgentMemoryService::with_base_path(tmp.path());
        memory.ensure_agent_layout("a1").await.unwrap();
        let interpreter = MemoryTierInterpreter::new(memory.clone());

        let mut definition = AgentDefinition::from_yaml_str(
            r#"
agent_id: "a1"
name: "A1"
persona: "Planner"
tools: []
memory_tiers:
  - name: "task_progress"
    scope: "agent_goal"
    description: "Track goal progress"
    schema:
      context_summary: { type: text }
      notes: { type: collection }
    render:
      format: "text"
      template: "{context_summary}"
    retention: "goal_lifetime"
memory_consolidation:
  - name: "update_task_progress"
    trigger: "cycle_completed"
    source: "episodes(g1, limit=1)"
    target: "task_progress"
    transform:
      type: "structured"
      builtin: "map_episode_to_task"
"#,
        )
        .unwrap();
        // Validation rejects user.* targets for cycle_completed+structured in phase 3.
        // Keep this invalid rule as a defense-in-depth runtime regression check.
        definition.memory_consolidation.insert(
            0,
            MemoryConsolidationRule {
                name: "unsupported_user_target".to_string(),
                trigger: ConsolidationTrigger::CycleCompleted,
                source: "episodes(g1, limit=1)".to_string(),
                target: "user.knowledge".to_string(),
                transform: ConsolidationTransform::Structured {
                    builtin: BuiltinTransform::MapEpisodeToTask,
                },
            },
        );

        let started_at = Utc::now();
        let episode = V3EpisodeRecord::new_memory_episode(
            None,
            "a1".to_string(),
            "cycle-1".to_string(),
            "g1".to_string(),
            "manual".to_string(),
            1,
            started_at,
            None,
            started_at,
            started_at + Duration::seconds(2),
            &EpisodeOutcome::PartialProgress {
                summary: "Collected records".to_string(),
                remaining: "Send summary".to_string(),
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
        );

        let updated_targets = interpreter
            .consolidate_cycle_completed_v3(
                &definition,
                "a1",
                "g1",
                &native_episode_record(&memory, &episode),
            )
            .await
            .unwrap();

        assert_eq!(updated_targets, vec!["task_progress".to_string()]);

        let task_def = definition
            .memory_tiers
            .iter()
            .find(|tier| tier.name == "task_progress")
            .unwrap();
        let task_data = memory
            .load_native_tier("a1", task_def, Some("g1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            task_data.fields.get("context_summary"),
            Some(&Value::String("Collected records".to_string()))
        );
    }

    #[test]
    fn renderer_substitutes_collection_placeholder_from_schema_field_name() {
        // Tier with a collection schema field named `entries` and a
        // template that references `{entries}`. The consolidator
        // writes the merged array under the schema field name
        // (`data.fields["entries"]`) — the renderer must read from
        // that key, not the legacy hardcoded `"value"`.
        let tier = MemoryTierDefinition {
            name: "semantic".to_string(),
            scope: TierScope::Agent,
            description: "Learned patterns".to_string(),
            schema: BTreeMap::from([(
                "entries".to_string(),
                super::super::memory_tiers::TierFieldSchema::Collection {
                    max_items: Some(50),
                    item_schema: None,
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{entries}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let mut record = V3MemoryTierRecord::new(
            tier.name.clone(),
            tier.scope.clone(),
            None,
            None,
            None,
            Some("a1"),
        );
        record.fields.insert(
            "entries".to_string(),
            serde_json::json!([
                {"name": "alpha", "value": "first"},
                {"name": "beta", "value": "second"},
            ]),
        );

        let rendered = DefaultMemoryRenderer.render(&tier, &record);
        // Substitution should have produced the value_to_text form
        // of the array — verify it isn't the structured fallback
        // (which would start with "Tier: semantic\n..." per
        // `render_tier_fields_fallback`).
        assert!(
            !rendered.starts_with("Tier:"),
            "renderer fell through to structured fallback: {rendered}"
        );
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("beta"));
    }

    #[test]
    fn renderer_still_falls_back_to_legacy_value_field() {
        // Tier with a collection schema field but data stored under
        // the legacy `value` key (older records, hand-written
        // fixtures, or tiers where `primary_collection_field` was
        // ambiguous when written). The renderer should still resolve
        // via the `value` fallback rather than warn-logging.
        let tier = MemoryTierDefinition {
            name: "semantic".to_string(),
            scope: TierScope::Agent,
            description: "Learned patterns".to_string(),
            schema: BTreeMap::from([(
                "entries".to_string(),
                super::super::memory_tiers::TierFieldSchema::Collection {
                    max_items: Some(50),
                    item_schema: None,
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{entries}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let mut record = V3MemoryTierRecord::new(
            tier.name.clone(),
            tier.scope.clone(),
            None,
            None,
            None,
            Some("a1"),
        );
        record
            .fields
            .insert("value".to_string(), serde_json::json!([{"name": "legacy"}]));

        let rendered = DefaultMemoryRenderer.render(&tier, &record);
        assert!(
            !rendered.starts_with("Tier:"),
            "renderer fell through to structured fallback: {rendered}"
        );
        assert!(rendered.contains("legacy"));
    }

    #[test]
    fn renderer_resolves_collection_placeholder_from_unstructured_object_fields() {
        let tier = MemoryTierDefinition {
            name: "project_conventions".to_string(),
            scope: TierScope::Agent,
            description: "Learned project conventions".to_string(),
            schema: BTreeMap::from([(
                "entries".to_string(),
                super::super::memory_tiers::TierFieldSchema::Collection {
                    max_items: Some(40),
                    item_schema: None,
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{entries}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let mut record = V3MemoryTierRecord::new(
            tier.name.clone(),
            tier.scope.clone(),
            None,
            None,
            None,
            Some("frontend-engineer"),
        );
        record.fields.insert(
            "framework_setup".to_string(),
            serde_json::json!({
                "framework": "SvelteKit",
                "evidence": ["package.json includes @sveltejs/kit"]
            }),
        );
        record.fields.insert(
            "testing_approach".to_string(),
            serde_json::json!({
                "status": "not specified in source data",
                "evidence": []
            }),
        );

        let rendered = DefaultMemoryRenderer.render(&tier, &record);
        assert!(
            !rendered.starts_with("Tier:"),
            "renderer fell through to structured fallback: {rendered}"
        );
        assert!(rendered.contains("framework_setup"));
        assert!(rendered.contains("framework: SvelteKit"));
        assert!(rendered.contains("testing_approach"));
    }

    #[test]
    fn value_to_text_renders_nested_values_without_json_dump() {
        let value = serde_json::json!({
            "owner": {
                "name": "Ava",
                "roles": ["admin", "reviewer"]
            },
            "active": true
        });

        let rendered = super::value_to_text(&value);
        assert!(rendered.contains("owner.name: Ava"));
        assert!(rendered.contains("owner.roles item 1: admin"));
        assert!(rendered.contains("owner.roles item 2: reviewer"));
        assert!(rendered.contains("active: true"));
        assert!(!rendered.contains('{'));
        assert!(!rendered.contains('}'));
        assert!(!rendered.contains('['));
        assert!(!rendered.contains(']'));
    }
}
