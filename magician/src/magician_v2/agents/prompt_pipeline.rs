//! Phase 3 prompt pipeline interpreter.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use thiserror::Error;
use tracing::warn;

use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;

use super::{
    condition::condition_allows,
    memory::{AgentMemoryService, Correction},
    memory_tier_interpreter::MemoryTierInterpreter,
    memory_tiers::{SourceRef, TierScope},
    types::{AgentDefinition, PromptPipelineConfig},
};

#[derive(Debug, Clone, Default)]
pub struct PromptPipelineInputs {
    /// Source key -> rendered text payload.
    pub sources: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct PromptPipelineRuntimeInputs<'a> {
    pub definition: &'a AgentDefinition,
    pub agent_id: &'a str,
    pub triggered_goal_id: &'a str,
    pub memory_service: &'a AgentMemoryService,
    pub memory_tier_interpreter: &'a MemoryTierInterpreter,
    /// P6-T3: Feedback injection cache from the previous cycle's transformers.
    /// Keys are source refs (e.g. "feedback.corrections", "strategy_context"),
    /// values are rendered text.
    pub feedback_injections: HashMap<String, String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PromptPipelineError {
    #[error("required section `{section}` resolved to empty content (source: `{source_ref}`)")]
    MissingRequiredSection { section: String, source_ref: String },
    #[error(
        "required section `{section}` exceeds max_context_tokens ({required_tokens} > {max_tokens})"
    )]
    RequiredSectionTruncated {
        section: String,
        required_tokens: usize,
        max_tokens: usize,
    },
    #[error("source resolution failed for `{source_ref}`: {reason}")]
    SourceResolution { source_ref: String, reason: String },
}

#[derive(Debug, Default, Clone)]
pub struct PromptPipelineInterpreter;

fn source_resolution_is_missing_optional_data(reason: &str) -> bool {
    reason.contains("no data for requested scope")
}

impl PromptPipelineInterpreter {
    pub fn assemble(
        &self,
        pipeline: &PromptPipelineConfig,
        inputs: &PromptPipelineInputs,
    ) -> Result<String, PromptPipelineError> {
        #[derive(Clone)]
        struct SectionChunk {
            name: String,
            rendered: String,
            token_count: usize,
            order: usize,
            required: bool,
        }

        let mut chunks = Vec::new();
        for (idx, section) in pipeline.sections.iter().enumerate() {
            if !condition_allows(section.condition.as_deref(), "prompt_pipeline") {
                // Condition gates inclusion — skip unconditionally.
                // `required` only applies to sections that pass the condition gate.
                continue;
            }

            let source_value = if let Some(content) = &section.content {
                content.trim().to_string()
            } else {
                inputs
                    .sources
                    .get(&section.source)
                    .cloned()
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };

            if section.required && source_value.is_empty() {
                return Err(PromptPipelineError::MissingRequiredSection {
                    section: section.name.clone(),
                    source_ref: if section.content.is_some() {
                        "inline content".to_string()
                    } else {
                        section.source.clone()
                    },
                });
            }
            if source_value.is_empty() {
                continue;
            }

            let rendered_body = render_section_body(section.format.as_deref(), source_value);

            let rendered = format!("## {}\n{}\n", section.name, rendered_body);
            chunks.push(SectionChunk {
                name: section.name.clone(),
                token_count: token_count(&rendered),
                rendered,
                order: idx,
                required: section.required,
            });
        }

        if chunks.is_empty() {
            return Ok(String::new());
        }

        let max_tokens = pipeline.output_rules.max_context_tokens as usize;
        let mut token_budget = 0_usize;
        let mut keep = HashSet::new();

        let mut by_name = HashMap::new();
        for chunk in &chunks {
            // Last-writer-wins is intentional here — section names are validated
            // for uniqueness in AgentDefinition::validate().
            by_name.insert(chunk.name.clone(), chunk.clone());
        }

        // Required sections must always survive truncation.
        for chunk in chunks.iter().filter(|chunk| chunk.required) {
            if chunk.token_count > max_tokens || token_budget + chunk.token_count > max_tokens {
                return Err(PromptPipelineError::RequiredSectionTruncated {
                    section: chunk.name.clone(),
                    required_tokens: chunk.token_count,
                    max_tokens,
                });
            }
            if keep.insert(chunk.name.clone()) {
                token_budget += chunk.token_count;
            }
        }

        // Highest-priority sections are considered first.
        // Skip duplicates in truncation_priority to avoid double-counting token budget.
        let mut priority_seen = HashSet::new();
        for name in &pipeline.output_rules.truncation_priority {
            if !priority_seen.insert(name) {
                continue;
            }
            let Some(chunk) = by_name.get(name) else {
                continue;
            };
            if chunk.required {
                continue;
            }
            if token_budget + chunk.token_count <= max_tokens {
                token_budget += chunk.token_count;
                keep.insert(chunk.name.clone());
            }
        }

        // Fill remaining budget with original-order sections.
        for chunk in &chunks {
            if keep.contains(&chunk.name) {
                continue;
            }
            if chunk.required {
                continue;
            }
            if token_budget + chunk.token_count <= max_tokens {
                token_budget += chunk.token_count;
                keep.insert(chunk.name.clone());
            }
        }

        let mut selected = chunks
            .into_iter()
            .filter(|chunk| keep.contains(&chunk.name))
            .collect::<Vec<_>>();
        selected.sort_by_key(|chunk| chunk.order);

        Ok(selected
            .into_iter()
            .map(|chunk| chunk.rendered)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub async fn assemble_runtime(
        &self,
        pipeline: &PromptPipelineConfig,
        inputs: &PromptPipelineRuntimeInputs<'_>,
    ) -> Result<String, PromptPipelineError> {
        #[derive(Clone)]
        struct SectionChunk {
            name: String,
            rendered: String,
            token_count: usize,
            order: usize,
            required: bool,
        }

        let mut chunks = Vec::new();
        for (idx, section) in pipeline.sections.iter().enumerate() {
            if !condition_allows(section.condition.as_deref(), "prompt_pipeline") {
                continue;
            }

            let source_value = if let Some(content) = &section.content {
                content.trim().to_string()
            } else {
                match resolve_source_text(
                    &section.source,
                    section.filter.as_deref(),
                    inputs,
                    "prompt_pipeline",
                )
                .await
                {
                    Ok(value) => value.trim().to_string(),
                    Err(PromptPipelineError::SourceResolution { reason, .. })
                        if !section.required
                            && source_resolution_is_missing_optional_data(&reason) =>
                    {
                        String::new()
                    },
                    Err(err) => return Err(err),
                }
            };

            if section.required && source_value.is_empty() {
                return Err(PromptPipelineError::MissingRequiredSection {
                    section: section.name.clone(),
                    source_ref: if section.content.is_some() {
                        "inline content".to_string()
                    } else {
                        section.source.clone()
                    },
                });
            }
            if source_value.is_empty() {
                continue;
            }

            let rendered_body = render_section_body(section.format.as_deref(), source_value);
            let rendered = format!("## {}\n{}\n", section.name, rendered_body);
            let auto_surface_required = section.source == "definition.auto_surface_guidance"
                && inputs
                    .definition
                    .auto_surface_policy
                    .as_ref()
                    .is_some_and(|p| p.enabled);
            chunks.push(SectionChunk {
                name: section.name.clone(),
                token_count: token_count(&rendered),
                rendered,
                order: idx,
                required: section.required || auto_surface_required,
            });
        }

        // Auto-inject auto_surface_guidance when the agent has an enabled
        // auto_surface_policy and no pipeline section explicitly declares
        // the source.  This ensures new agents with auto_surface_policy get
        // the dashboardable-output guidance without requiring per-agent
        // pipeline config.
        if inputs
            .definition
            .auto_surface_policy
            .as_ref()
            .is_some_and(|p| p.enabled)
        {
            let already_declared = pipeline
                .sections
                .iter()
                .any(|s| s.source == "definition.auto_surface_guidance");
            if !already_declared {
                let guidance = render_auto_surface_guidance(inputs.definition);
                if !guidance.is_empty() {
                    let rendered = format!("{}\n", guidance);
                    let next_order = chunks.len();
                    chunks.push(SectionChunk {
                        name: "Dashboardable Output".to_string(),
                        token_count: token_count(&rendered),
                        rendered,
                        order: next_order,
                        required: true,
                    });
                }
            }
        }

        if chunks.is_empty() {
            return Ok(String::new());
        }

        let max_tokens = pipeline.output_rules.max_context_tokens as usize;
        let mut token_budget = 0_usize;
        let mut keep = HashSet::new();

        let mut by_name = HashMap::new();
        for chunk in &chunks {
            by_name.insert(chunk.name.clone(), chunk.clone());
        }

        // Required sections must always survive truncation.
        for chunk in chunks.iter().filter(|chunk| chunk.required) {
            if chunk.token_count > max_tokens || token_budget + chunk.token_count > max_tokens {
                return Err(PromptPipelineError::RequiredSectionTruncated {
                    section: chunk.name.clone(),
                    required_tokens: chunk.token_count,
                    max_tokens,
                });
            }
            if keep.insert(chunk.name.clone()) {
                token_budget += chunk.token_count;
            }
        }

        let mut priority_seen = HashSet::new();
        for name in &pipeline.output_rules.truncation_priority {
            if !priority_seen.insert(name) {
                continue;
            }
            let Some(chunk) = by_name.get(name) else {
                continue;
            };
            if chunk.required {
                continue;
            }
            if token_budget + chunk.token_count <= max_tokens {
                token_budget += chunk.token_count;
                keep.insert(chunk.name.clone());
            }
        }

        for chunk in &chunks {
            if keep.contains(&chunk.name) {
                continue;
            }
            if chunk.required {
                continue;
            }
            if token_budget + chunk.token_count <= max_tokens {
                token_budget += chunk.token_count;
                keep.insert(chunk.name.clone());
            }
        }

        let mut selected = chunks
            .into_iter()
            .filter(|chunk| keep.contains(&chunk.name))
            .collect::<Vec<_>>();
        selected.sort_by_key(|chunk| chunk.order);

        Ok(selected
            .into_iter()
            .map(|chunk| chunk.rendered)
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

fn render_section_body(format: Option<&str>, source_value: String) -> String {
    match format {
        Some("bullet_points") => source_value
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| format!("- {}", line.trim()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some("compact_summary") => source_value.replace('\n', " "),
        _ => source_value,
    }
}

async fn resolve_source_text(
    source_ref: &str,
    filter: Option<&str>,
    inputs: &PromptPipelineRuntimeInputs<'_>,
    context: &str,
) -> Result<String, PromptPipelineError> {
    let source_ref = source_ref.trim();

    if source_ref == "definition.persona" {
        return Ok(inputs.definition.persona.trim().to_string());
    }

    if source_ref == "definition.auto_surface_guidance" {
        return Ok(render_auto_surface_guidance(inputs.definition));
    }

    // definition.goals[*] source refs are no longer supported — goals moved to Task.
    if parse_definition_goal_source(source_ref).is_some() {
        return Err(PromptPipelineError::SourceResolution {
            source_ref: source_ref.to_string(),
            reason: "goal definitions removed from agents; use Task intent instead".to_string(),
        });
    }

    if source_ref == "memory.user_profile" {
        let profile = inputs
            .memory_service
            .build_context_profile(inputs.agent_id)
            .await
            .map_err(|err| PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: err.to_string(),
            })?;
        return Ok(render_context_profile(&profile));
    }

    if source_ref == "memory.user_knowledge" {
        let knowledge = inputs
            .memory_service
            .load_user_knowledge()
            .await
            .map_err(|err| PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: err.to_string(),
            })?;
        return Ok(render_user_knowledge(&knowledge));
    }

    if source_ref == "memory.corrections" {
        let mut corrections = inputs
            .memory_service
            .load_corrections(inputs.agent_id)
            .await
            .map_err(|err| PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: err.to_string(),
            })?;
        corrections = apply_corrections_filter(corrections, filter, context);
        return Ok(render_corrections(&corrections));
    }

    if episodes_source_uses_unprocessed_selector(source_ref) {
        return Err(PromptPipelineError::SourceResolution {
            source_ref: source_ref.to_string(),
            reason: "unprocessed selector is not supported for prompt pipeline episode sources"
                .to_string(),
        });
    }

    if let Some((goal_selector, limit)) = parse_episodes_source(source_ref) {
        let goal_id = resolve_goal_selector(goal_selector.as_deref(), inputs.triggered_goal_id);
        let mut episodes = load_goal_episodes(inputs, &goal_id, source_ref).await?;
        episodes = apply_episode_filter(episodes, filter, context);
        episodes = take_recent(episodes, limit);
        return Ok(render_episodes(&episodes));
    }

    if let Some((tier_name, field_path)) = parse_memory_tier_source(source_ref) {
        return resolve_tier_source_text(
            source_ref,
            &tier_name,
            field_path.as_deref(),
            inputs,
            context,
            filter,
        )
        .await;
    }

    if let Some(SourceRef::Tiers { tier_refs }) = SourceRef::parse(source_ref) {
        let mut rendered = Vec::new();
        for tier_ref in tier_refs {
            if let Some(agent) = tier_ref.agent.as_deref() {
                if agent != inputs.agent_id {
                    return Err(PromptPipelineError::SourceResolution {
                        source_ref: source_ref.to_string(),
                        reason: format!(
                            "cross-agent tier reference `{agent}.{}` is not supported in prompt pipeline",
                            tier_ref.tier_name
                        ),
                    });
                }
            }
            let tier_text = resolve_tier_source_text(
                source_ref,
                &tier_ref.tier_name,
                None,
                inputs,
                context,
                filter,
            )
            .await?;
            if !tier_text.trim().is_empty() {
                rendered.push(format!("{}:\n{}", tier_ref.tier_name, tier_text));
            }
        }
        return Ok(rendered.join("\n\n"));
    }

    if source_ref == "derived.failure_analysis" {
        let mut episodes = load_goal_episodes(inputs, inputs.triggered_goal_id, source_ref).await?;
        episodes = apply_episode_filter(episodes, filter, context);
        return Ok(render_failure_analysis(episodes));
    }

    if source_ref == "derived.success_patterns" {
        let mut episodes = load_goal_episodes(inputs, inputs.triggered_goal_id, source_ref).await?;
        episodes = apply_episode_filter(episodes, filter, context);
        return Ok(render_success_patterns(episodes));
    }

    if source_ref.starts_with("feedback.") || source_ref == "strategy_context" {
        // P6-T3: Render feedback context from the previous cycle's transformer output.
        return Ok(inputs
            .feedback_injections
            .get(source_ref)
            .cloned()
            .unwrap_or_default());
    }

    Err(PromptPipelineError::SourceResolution {
        source_ref: source_ref.to_string(),
        reason: "unsupported source reference".to_string(),
    })
}

fn parse_definition_goal_source(source_ref: &str) -> Option<String> {
    let inner = source_ref
        .strip_prefix("definition.goals[")
        .and_then(|rest| rest.strip_suffix(']'))?;
    let selector = inner.trim();
    if selector.is_empty() {
        return None;
    }
    Some(selector.to_string())
}

// resolve_goal_definition and render_goal_definition removed —
// goals moved to Task in the unified architecture.

fn render_context_profile(profile: &super::memory::AgentContextProfile) -> String {
    let mut lines = vec![
        format!("Profile ID: {}", profile.base.profile_id),
        format!("Updated: {}", profile.base.updated_at.to_rfc3339()),
    ];

    if !profile.effective_preferences.is_empty() {
        lines.push("Effective preferences:".to_string());
        let mut sorted = profile
            .effective_preferences
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, value) in sorted {
            lines.push(format!("- {}: {}", key, value_to_natural_text(&value)));
        }
    }

    if !profile.corrections.is_empty() {
        lines.push(format!("Corrections: {}", profile.corrections.len()));
    }

    lines.join("\n")
}

fn render_user_knowledge(value: &Value) -> String {
    match value {
        Value::Object(map) if map.is_empty() => String::new(),
        Value::Object(map) => {
            let mut sorted = map.iter().collect::<Vec<_>>();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            sorted
                .into_iter()
                .map(|(key, value)| format!("- {}: {}", key, value_to_natural_text(value)))
                .collect::<Vec<_>>()
                .join("\n")
        },
        _ => value_to_natural_text(value),
    }
}

fn apply_corrections_filter(
    mut corrections: Vec<Correction>,
    filter: Option<&str>,
    context: &str,
) -> Vec<Correction> {
    let Some(filter) = filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return corrections;
    };

    match filter {
        // Corrections currently have no explicit resolved/expired marker at this layer.
        // Treat loaded corrections as active.
        "active_only" => corrections,
        other => {
            warn!(
                filter = other,
                context, "unknown corrections filter — failing closed"
            );
            corrections.clear();
            corrections
        },
    }
}

fn render_corrections(corrections: &[Correction]) -> String {
    corrections
        .iter()
        .map(|correction| {
            format!(
                "- [{}] {} ({:?}): {}",
                correction.timestamp.to_rfc3339(),
                correction.correction_id,
                correction.category,
                correction.correction.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_episodes_source(source_ref: &str) -> Option<(Option<String>, Option<usize>)> {
    let canonical = source_ref.strip_prefix("memory.").unwrap_or(source_ref);
    match SourceRef::parse(canonical) {
        Some(SourceRef::Episodes { goal_id, limit, .. }) => Some((goal_id, limit)),
        _ => None,
    }
}

fn episodes_source_uses_unprocessed_selector(source_ref: &str) -> bool {
    let canonical = source_ref
        .strip_prefix("memory.")
        .unwrap_or(source_ref)
        .trim();
    let Some(inner) = canonical
        .strip_prefix("episodes(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };

    inner
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .any(|token| {
            token
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "unprocessed")
        })
}

fn resolve_goal_selector(goal_selector: Option<&str>, triggered_goal_id: &str) -> String {
    match goal_selector.map(str::trim) {
        None | Some("") | Some("goal_id") | Some("triggered_goal_id") => {
            triggered_goal_id.to_string()
        },
        Some(goal_id) => goal_id.to_string(),
    }
}

async fn load_goal_episodes(
    inputs: &PromptPipelineRuntimeInputs<'_>,
    goal_id: &str,
    source_ref: &str,
) -> Result<Vec<V3EpisodeRecord>, PromptPipelineError> {
    inputs
        .memory_service
        .load_native_episodes_for_goal(inputs.agent_id, goal_id)
        .await
        .map_err(|err| PromptPipelineError::SourceResolution {
            source_ref: source_ref.to_string(),
            reason: err.to_string(),
        })
}

fn apply_episode_filter(
    mut episodes: Vec<V3EpisodeRecord>,
    filter: Option<&str>,
    context: &str,
) -> Vec<V3EpisodeRecord> {
    let Some(filter) = filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return episodes;
    };

    match filter {
        "outcome.is_failed" => episodes
            .into_iter()
            .filter(|episode| episode.outcome_is_failed())
            .collect(),
        "outcome.is_succeeded" => episodes
            .into_iter()
            .filter(|episode| episode.outcome_is_succeeded())
            .collect(),
        // tactical pattern T3: expose partial-success as a distinct filter so
        // agent definitions can target "almost succeeded" episodes
        // directly. `is_failed_or_partial` is the high-signal
        // learning surface — recommended for `failure_context`-style
        // pipeline sections.
        "outcome.is_partial" => episodes
            .into_iter()
            .filter(|episode| episode.outcome_is_partial())
            .collect(),
        "outcome.is_failed_or_partial" => episodes
            .into_iter()
            .filter(|episode| episode.outcome_is_failed_or_partial())
            .collect(),
        other => {
            warn!(
                filter = other,
                context, "unknown episode filter — failing closed"
            );
            episodes.clear();
            episodes
        },
    }
}

fn take_recent(mut episodes: Vec<V3EpisodeRecord>, limit: Option<usize>) -> Vec<V3EpisodeRecord> {
    if let Some(limit) = limit {
        if episodes.len() > limit {
            let offset = episodes.len().saturating_sub(limit);
            episodes = episodes.split_off(offset);
        }
    }
    episodes.reverse();
    episodes
}

fn render_episodes(episodes: &[V3EpisodeRecord]) -> String {
    episodes
        .iter()
        .map(|episode| {
            let status = if episode.outcome_is_succeeded() {
                "succeeded"
            } else if episode.outcome_is_failed() {
                "failed"
            } else if episode.outcome_is_paused() {
                "paused"
            } else {
                "partial"
            };
            format!(
                "- [{}] {}: {} (actions: {})",
                episode
                    .completed_at_dt()
                    .map(|ts| ts.to_rfc3339())
                    .unwrap_or_else(|_| episode.completed_at.clone()),
                status,
                episode.outcome_summary_text().trim(),
                episode.actions_taken.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn resolve_tier_source_text(
    source_ref: &str,
    tier_name: &str,
    field_path: Option<&str>,
    inputs: &PromptPipelineRuntimeInputs<'_>,
    context: &str,
    filter: Option<&str>,
) -> Result<String, PromptPipelineError> {
    if let Some(filter) = filter.map(str::trim).filter(|value| !value.is_empty()) {
        warn!(
            source = source_ref,
            filter, context, "prompt filter unsupported for tier source — failing closed"
        );
        return Err(PromptPipelineError::SourceResolution {
            source_ref: source_ref.to_string(),
            reason: format!("filter `{filter}` is not supported for tier source"),
        });
    }

    let Some(tier_definition) = inputs
        .definition
        .memory_tiers
        .iter()
        .find(|tier| tier.name == tier_name)
    else {
        return Err(PromptPipelineError::SourceResolution {
            source_ref: source_ref.to_string(),
            reason: format!("tier `{tier_name}` is not declared on agent definition"),
        });
    };

    if let Some(field_path) = field_path {
        let tier_goal_id = match tier_definition.scope {
            TierScope::AgentGoal => Some(inputs.triggered_goal_id),
            _ => None,
        };
        let Some(tier_data) = inputs
            .memory_service
            .load_native_tier(inputs.agent_id, tier_definition, tier_goal_id)
            .await
            .map_err(|err| PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: err.to_string(),
            })?
        else {
            return Err(PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: format!("tier `{tier_name}` has no data for requested scope"),
            });
        };

        let root = Value::Object(
            tier_data
                .fields
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        if let Some(direct) = tier_data.fields.get(field_path) {
            return Ok(value_to_natural_text(direct));
        }
        let Some(value) = resolve_json_path(&root, field_path) else {
            return Err(PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: format!("tier `{tier_name}` has no field at path `{field_path}`"),
            });
        };
        return Ok(value_to_natural_text(value));
    }

    let rendered = inputs
        .memory_tier_interpreter
        .load_and_render(
            inputs.agent_id,
            Some(inputs.triggered_goal_id),
            std::slice::from_ref(tier_definition),
        )
        .await
        .map_err(|err| PromptPipelineError::SourceResolution {
            source_ref: source_ref.to_string(),
            reason: err.to_string(),
        })?;

    let rendered_text =
        rendered
            .get(tier_name)
            .ok_or_else(|| PromptPipelineError::SourceResolution {
                source_ref: source_ref.to_string(),
                reason: format!("tier `{tier_name}` is empty or cannot be rendered"),
            })?;
    Ok(rendered_text.clone())
}

fn parse_memory_tier_source(source_ref: &str) -> Option<(String, Option<String>)> {
    let inner = source_ref.strip_prefix("memory.tier[")?;
    let close_idx = inner.find(']')?;
    let tier_name = inner[..close_idx].trim();
    if tier_name.is_empty() {
        return None;
    }
    let suffix = &inner[close_idx + 1..];
    if suffix.is_empty() {
        return Some((tier_name.to_string(), None));
    }
    let field_path = suffix.strip_prefix('.')?.trim();
    if field_path.is_empty() {
        return None;
    }
    Some((tier_name.to_string(), Some(field_path.to_string())))
}

fn resolve_json_path<'a>(value: &'a Value, field_path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in field_path
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
    {
        match current {
            Value::Object(map) => current = map.get(segment)?,
            _ => return None,
        }
    }
    Some(current)
}

fn render_failure_analysis(episodes: Vec<V3EpisodeRecord>) -> String {
    let failed = episodes
        .into_iter()
        .filter(|episode| episode.outcome_is_failed())
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return String::new();
    }
    let recent = take_recent(failed, Some(5));
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for episode in &recent {
        let mut reason = episode.outcome_error_summary();
        if reason.trim().is_empty() {
            reason = episode.outcome_summary_text().to_string();
        }
        let entry = counts.entry(reason.trim().to_string()).or_insert(0);
        *entry += 1;
    }

    let mut lines = vec!["Recent failure analysis:".to_string()];
    let mut summary = counts.into_iter().collect::<Vec<_>>();
    summary.sort_by(|(a_reason, a_count), (b_reason, b_count)| {
        b_count.cmp(a_count).then_with(|| a_reason.cmp(b_reason))
    });
    for (reason, count) in summary {
        lines.push(format!("- {} ({}x)", reason, count));
    }
    lines
        .into_iter()
        .chain(std::iter::once(String::new()))
        .chain(render_episodes(&recent).lines().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_success_patterns(episodes: Vec<V3EpisodeRecord>) -> String {
    let succeeded = episodes
        .into_iter()
        .filter(|episode| episode.outcome_is_succeeded())
        .collect::<Vec<_>>();
    if succeeded.is_empty() {
        return String::new();
    }
    let recent = take_recent(succeeded, Some(8));
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for episode in &recent {
        let pattern = episode
            .strategy_summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| episode.outcome_summary_text());
        let entry = counts.entry(pattern.to_string()).or_insert(0);
        *entry += 1;
    }

    let mut lines = vec!["Recent success patterns:".to_string()];
    let mut summary = counts.into_iter().collect::<Vec<_>>();
    summary.sort_by(|(a_pattern, a_count), (b_pattern, b_count)| {
        b_count.cmp(a_count).then_with(|| a_pattern.cmp(b_pattern))
    });
    for (pattern, count) in summary {
        lines.push(format!("- {} ({}x)", pattern, count));
    }
    lines
        .into_iter()
        .chain(std::iter::once(String::new()))
        .chain(render_episodes(&recent).lines().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render auto-surface guidance for agents with `auto_surface_policy` enabled.
/// Injected into the prompt to teach the agent about dashboardable artifact types.
fn render_auto_surface_guidance(definition: &AgentDefinition) -> String {
    let policy = match &definition.auto_surface_policy {
        Some(p) if p.enabled => p,
        _ => return String::new(),
    };

    format!(
        "## Dashboardable Output\n\
         \n\
         Your agent has auto-surface publication enabled (route: {route}).\n\
         When you produce artifacts, tag them with a dashboardable type to \
         automatically render a visual dashboard.\n\
         \n\
         Available artifact types:\n\
         - `custom:metric_set` — rendered as a metrics grid. Shape: {{\"items\": [{{\"label\": \"...\", \"value\": \"...\", \"trend\": \"up|down|flat\", \"trend_label\": \"...\"}}]}}\n\
         - `custom:record_table` — rendered as a data table. Shape: {{\"rows\": [...], \"columns\": [{{\"key\": \"...\", \"label\": \"...\"}}]}}\n\
         - `custom:activity_feed` — rendered as an activity list. Shape: {{\"items\": [{{\"actor\": \"...\", \"action\": \"...\", \"target\": \"...\"}}]}}\n\
         - `custom:summary_note` — rendered as markdown. Shape: {{\"content\": \"markdown text\"}}\n\
         \n\
         How to tag artifacts:\n\
         - In goal_reached: set `artifact_type` and optional `render_hints` on each artifact:\n\
           {{\"decision\": \"goal_reached\", \"artifacts\": [{{\"name\": \"metrics.json\", \
         \"content_type\": \"application/json\", \"artifact_type\": \"custom:metric_set\", \
         \"render_hints\": {{\"surface_group\": \"overview\", \"display_priority\": 1, \
         \"section_title\": \"Key Metrics\", \"freshness_ttl_secs\": 3600}}, \
         \"data\": \"...\"}}]}}\n\
         \n\
         Optional `render_hints` fields:\n\
         - `surface_group`: group artifacts into separate dashboards\n\
         - `display_priority`: order sections (lower = higher priority)\n\
         - `section_title`: override default section heading\n\
         - `freshness_ttl_secs`: surface expiry in seconds",
        route = policy.route,
    )
}

fn value_to_natural_text(value: &Value) -> String {
    let mut flattened = Vec::new();
    flatten_value("", value, &mut flattened);
    if flattened.is_empty() {
        String::new()
    } else {
        flattened.join("; ")
    }
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

/// Approximate token count for budget decisions.
///
/// Uses `max(word_count, char_count / 4)` which better handles mixed content
/// with URLs, code blocks, and long compound words than pure word-count.
fn token_count(input: &str) -> usize {
    let words = input.split_whitespace().count();
    let chars_est = input.len() / 4;
    words.max(chars_est)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::AutoSurfacePolicy;
    use crate::magician_v2::agents::{
        memory::EpisodeOutcome, AgentDefinition, AgentMemoryService, MemoryTierInterpreter,
    };
    use chrono::Utc;
    use tempfile::tempdir;

    fn parse_pipeline(yaml: &str) -> PromptPipelineConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn assemble_fails_when_required_source_missing() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: persona
    source: definition.persona
    required: true
"#,
        );
        let err = PromptPipelineInterpreter
            .assemble(&pipeline, &PromptPipelineInputs::default())
            .unwrap_err();
        assert_eq!(
            err,
            PromptPipelineError::MissingRequiredSection {
                section: "persona".to_string(),
                source_ref: "definition.persona".to_string()
            }
        );
    }

    #[test]
    fn assemble_respects_simple_conditions() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: include
    source: a
    condition: true
  - name: skip
    source: b
    condition: false
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([
                ("a".to_string(), "hello".to_string()),
                ("b".to_string(), "world".to_string()),
            ]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.contains("include"));
        assert!(!rendered.contains("skip"));
    }

    #[test]
    fn token_count_uses_char_estimate_for_long_tokens() {
        // A URL is 1 word but many chars — char-based estimate is more accurate.
        let url = "https://example.com/very/long/path/to/resource?query=value&other=true";
        let count = super::token_count(url);
        // Word-count would be 1, but char/4 ≈ 17 — the char estimate wins.
        assert!(
            count > 1,
            "expected char-based estimate to dominate, got {count}"
        );

        // Regular prose still uses word count when it's higher.
        let prose = "the cat sat on a mat";
        let count = super::token_count(prose);
        assert_eq!(count, 6); // 6 words > 20/4=5
    }

    #[test]
    fn assemble_fails_closed_for_unknown_condition_expression() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: guarded
    source: a
    condition: episode.outcome.is_succeeded
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([("a".to_string(), "hello".to_string())]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.is_empty());
    }

    #[test]
    fn assemble_truncates_sections_exceeding_token_budget() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: small
    source: small_src
  - name: large
    source: large_src
output_rules:
  max_context_tokens: 20
  truncation_priority: [small]
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([
                ("small_src".to_string(), "brief".to_string()),
                (
                    "large_src".to_string(),
                    "this is a very long section that should exceed the budget easily and be dropped from the output when the small section is prioritized first in truncation ordering"
                        .to_string(),
                ),
            ]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.contains("small"));
        assert!(!rendered.contains("large"));
    }

    #[test]
    fn assemble_applies_bullet_points_format() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: bullets
    source: src
    format: bullet_points
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([(
                "src".to_string(),
                "line one\nline two\n\nline three".to_string(),
            )]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.contains("- line one"));
        assert!(rendered.contains("- line two"));
        assert!(rendered.contains("- line three"));
        // Blank lines should be filtered out, not turned into bullets.
        assert!(!rendered.contains("- \n"));
    }

    #[test]
    fn assemble_applies_compact_summary_format() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: compact
    source: src
    format: compact_summary
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([(
                "src".to_string(),
                "first line\nsecond line\nthird line".to_string(),
            )]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.contains("first line second line third line"));
        // No newlines should remain in the body (newlines between sections are OK).
        let body = rendered.strip_prefix("## compact\n").unwrap().trim_end();
        assert!(!body.contains('\n'));
    }

    #[test]
    fn assemble_deduplicates_truncation_priority_entries() {
        // Duplicate entries in truncation_priority must not double-count token budget.
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: alpha
    source: a
  - name: beta
    source: b
output_rules:
  max_context_tokens: 30
  truncation_priority: [alpha, alpha, beta]
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([
                ("a".to_string(), "hello world".to_string()),
                ("b".to_string(), "goodbye world".to_string()),
            ]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        // Both sections should fit in the budget. Without dedup, alpha would
        // consume budget twice and beta would be excluded.
        assert!(rendered.contains("alpha"), "alpha should be included");
        assert!(rendered.contains("beta"), "beta should be included");
    }

    #[test]
    fn assemble_skips_required_section_when_condition_is_false() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: conditional_required
    source: a
    required: true
    condition: false
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([("a".to_string(), "hello".to_string())]),
        };
        // condition:false gates inclusion — required only applies to included sections.
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.is_empty());
    }

    #[test]
    fn assemble_keeps_required_section_even_when_not_prioritized() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: required_context
    source: req
    required: true
  - name: optional_context
    source: opt
output_rules:
  max_context_tokens: 14
  truncation_priority: [optional_context]
"#,
        );
        let inputs = PromptPipelineInputs {
            sources: HashMap::from([
                ("req".to_string(), "must include".to_string()),
                (
                    "opt".to_string(),
                    "this optional section is intentionally too long".to_string(),
                ),
            ]),
        };
        let rendered = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(rendered.contains("required_context"));
        assert!(!rendered.contains("optional_context"));
    }

    fn parse_agent_with_pipeline(pipeline_yaml: &str) -> AgentDefinition {
        let mut definition = AgentDefinition::from_yaml_str(
            r#"
agent_id: "agent-1"
name: "Runtime Agent"
persona: "You are careful and concise."
tools: []
memory_tiers:
  - name: "task_progress"
    scope: "agent_goal"
    description: "Task status"
    schema:
      summary: { type: text }
    render:
      format: "compact_summary"
      template: "{summary}"
    retention: "goal_lifetime"
"#,
        )
        .unwrap();
        definition.prompt_pipeline = Some(parse_pipeline(pipeline_yaml));
        definition
    }

    fn episode_record(
        episode_id: &str,
        goal_id: &str,
        outcome: EpisodeOutcome,
        summary: Option<&str>,
    ) -> V3EpisodeRecord {
        let now = Utc::now();
        V3EpisodeRecord::new_memory_episode(
            None,
            "agent-1".to_string(),
            episode_id.to_string(),
            goal_id.to_string(),
            "manual".to_string(),
            1,
            now,
            None,
            now,
            now,
            &outcome,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            summary.map(str::to_string),
            None,
            None,
        )
    }

    fn native_episode_record(
        _memory_service: &AgentMemoryService,
        episode: &V3EpisodeRecord,
    ) -> V3EpisodeRecord {
        episode.clone()
    }

    fn native_tier_record(
        memory_service: &AgentMemoryService,
        agent_id: &str,
        tier_definition: &crate::magician_v2::agents::memory_tiers::MemoryTierDefinition,
        goal_id: Option<&str>,
        fields: serde_json::Map<String, Value>,
    ) -> V3MemoryTierRecord {
        let scope = memory_service.scoped_memory_scope();
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
    async fn assemble_runtime_resolves_definition_goal_and_tier_sources() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: persona
    source: definition.persona
  - name: progress
    source: memory.tier[task_progress].summary
output_rules:
  max_context_tokens: 400
  truncation_priority: [persona, progress]
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();

        let tier_fields = [(
            "summary".to_string(),
            Value::String("Completed two checkpoints".to_string()),
        )]
        .into_iter()
        .collect();
        memory_service
            .save_native_tier(
                "agent-1",
                &definition.memory_tiers[0],
                Some("g1"),
                &native_tier_record(
                    &memory_service,
                    "agent-1",
                    &definition.memory_tiers[0],
                    Some("g1"),
                    tier_fields,
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let rendered = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap();
        assert!(rendered.contains("You are careful and concise."));
        assert!(rendered.contains("Completed two checkpoints"));
    }

    #[tokio::test]
    async fn assemble_runtime_applies_episode_filter() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: failed_history
    source: memory.episodes(goal_id, limit=10)
    filter: outcome.is_failed
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();

        memory_service
            .append_native_episode(
                "agent-1",
                &native_episode_record(
                    &memory_service,
                    &episode_record(
                        "e-success",
                        "g1",
                        EpisodeOutcome::GoalAchieved {
                            summary: "Goal complete".to_string(),
                        },
                        Some("baseline_strategy"),
                    ),
                ),
            )
            .await
            .unwrap();
        memory_service
            .append_native_episode(
                "agent-1",
                &native_episode_record(
                    &memory_service,
                    &episode_record(
                        "e-fail",
                        "g1",
                        EpisodeOutcome::Failed {
                            error: "selector missing".to_string(),
                        },
                        None,
                    ),
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let rendered = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap();
        assert!(rendered.contains("selector missing"));
        assert!(!rendered.contains("Goal complete"));
    }

    #[tokio::test]
    async fn assemble_runtime_rejects_unprocessed_episode_selector() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: history
    source: memory.episodes(goal_id, unprocessed=false, limit=10)
output_rules:
  max_context_tokens: 400
  truncation_priority: [history]
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();

        memory_service
            .append_native_episode(
                "agent-1",
                &native_episode_record(
                    &memory_service,
                    &episode_record(
                        "e-1",
                        "g1",
                        EpisodeOutcome::GoalAchieved {
                            summary: "Goal complete".to_string(),
                        },
                        Some("baseline_strategy"),
                    ),
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::SourceResolution { source_ref, reason } => {
                assert_eq!(
                    source_ref,
                    "memory.episodes(goal_id, unprocessed=false, limit=10)"
                );
                assert!(reason.contains("unprocessed selector"));
                assert!(reason.contains("not supported"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_required_missing_source_fails_fast() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: required_progress
    source: memory.tier[task_progress].summary
    required: true
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();
        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::MissingRequiredSection {
                section,
                source_ref,
            } => {
                assert_eq!(section, "required_progress");
                assert_eq!(source_ref, "memory.tier[task_progress].summary");
            },
            PromptPipelineError::SourceResolution { source_ref, reason } => {
                assert_eq!(source_ref, "memory.tier[task_progress].summary");
                assert!(reason.contains("no data"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_fails_when_required_section_exceeds_budget() {
        let mut definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: persona
    source: definition.persona
    required: true
output_rules:
  max_context_tokens: 2
"#,
        );
        definition.persona = "You must include this long required context block.".to_string();
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();
        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::RequiredSectionTruncated {
                section,
                required_tokens,
                max_tokens,
            } => {
                assert_eq!(section, "persona");
                assert!(required_tokens > max_tokens);
                assert_eq!(max_tokens, 2);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_requires_auto_surface_guidance_when_policy_is_enabled() {
        let mut definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: persona
    source: definition.persona
output_rules:
  max_context_tokens: 12
"#,
        );
        definition.auto_surface_policy = Some(AutoSurfacePolicy {
            enabled: true,
            route: "/briefing".to_string(),
            materialize_as: None,
            surface_kind: None,
            placement_kind: None,
            pinned: None,
            title_template: "{agent_name}".to_string(),
        });
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();
        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::RequiredSectionTruncated {
                section,
                max_tokens,
                ..
            } => {
                assert_eq!(section, "Dashboardable Output");
                assert_eq!(max_tokens, 12);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_requires_explicit_auto_surface_guidance_section_when_policy_is_enabled(
    ) {
        let mut definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: dashboard_rules
    source: definition.auto_surface_guidance
output_rules:
  max_context_tokens: 12
"#,
        );
        definition.auto_surface_policy = Some(AutoSurfacePolicy {
            enabled: true,
            route: "/briefing".to_string(),
            materialize_as: None,
            surface_kind: None,
            placement_kind: None,
            pinned: None,
            title_template: "{agent_name}".to_string(),
        });
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();
        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::RequiredSectionTruncated {
                section,
                max_tokens,
                ..
            } => {
                assert_eq!(section, "dashboard_rules");
                assert_eq!(max_tokens, 12);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_tier_filter_fails() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: progress
    source: memory.tier[task_progress].summary
    filter: active_only
output_rules:
  max_context_tokens: 300
  truncation_priority: [progress]
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();

        let tier_fields = [(
            "summary".to_string(),
            Value::String("This would render without the filter".to_string()),
        )]
        .into_iter()
        .collect();
        memory_service
            .save_native_tier(
                "agent-1",
                &definition.memory_tiers[0],
                Some("g1"),
                &native_tier_record(
                    &memory_service,
                    "agent-1",
                    &definition.memory_tiers[0],
                    Some("g1"),
                    tier_fields,
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };
        let rendered = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match rendered {
            PromptPipelineError::SourceResolution { source_ref, reason } => {
                assert_eq!(source_ref, "memory.tier[task_progress].summary");
                assert!(reason.contains("filter"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_requires_existing_tier_reference() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: progress
    source: memory.tier[missing_tier].summary
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::SourceResolution { source_ref, reason } => {
                assert_eq!(source_ref, "memory.tier[missing_tier].summary");
                assert!(reason.contains("not declared"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_requires_existing_tier_field() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: progress
    source: memory.tier[task_progress].summary
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();

        let tier_fields = [("notes".to_string(), Value::String("notes".to_string()))]
            .into_iter()
            .collect();
        memory_service
            .save_native_tier(
                "agent-1",
                &definition.memory_tiers[0],
                Some("g1"),
                &native_tier_record(
                    &memory_service,
                    "agent-1",
                    &definition.memory_tiers[0],
                    Some("g1"),
                    tier_fields,
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::SourceResolution { source_ref, reason } => {
                assert_eq!(source_ref, "memory.tier[task_progress].summary");
                assert!(reason.contains("no field"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_fails_for_cross_agent_tiers_source() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: remote_tier
    source: tiers(other_agent.task_progress)
output_rules:
  max_context_tokens: 300
  truncation_priority: [remote_tier]
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();
        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let err = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap_err();
        match err {
            PromptPipelineError::SourceResolution { source_ref, reason } => {
                assert_eq!(source_ref, "tiers(other_agent.task_progress)");
                assert!(reason.contains("cross-agent tier reference"));
                assert!(reason.contains("not supported"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assemble_runtime_accepts_self_qualified_tiers_source() {
        let definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: local_tier
    source: tiers(agent-1.task_progress)
output_rules:
  max_context_tokens: 300
  truncation_priority: [local_tier]
"#,
        );
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service.ensure_agent_layout("agent-1").await.unwrap();

        let tier_fields = [(
            "summary".to_string(),
            Value::String("Completed two checkpoints".to_string()),
        )]
        .into_iter()
        .collect();
        memory_service
            .save_native_tier(
                "agent-1",
                &definition.memory_tiers[0],
                Some("g1"),
                &native_tier_record(
                    &memory_service,
                    "agent-1",
                    &definition.memory_tiers[0],
                    Some("g1"),
                    tier_fields,
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent-1",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let rendered = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap();
        assert!(rendered.contains("local_tier"));
        assert!(rendered.contains("task_progress:"));
        assert!(rendered.contains("Completed two checkpoints"));
    }

    #[tokio::test]
    async fn assemble_runtime_accepts_dotted_self_qualified_tiers_source() {
        let mut definition = parse_agent_with_pipeline(
            r#"
sections:
  - name: local_tier
    source: tiers(agent.v2.task_progress)
output_rules:
  max_context_tokens: 300
  truncation_priority: [local_tier]
"#,
        );
        definition.agent_id = "agent.v2".to_string();
        let tmp = tempdir().unwrap();
        let memory_service = AgentMemoryService::with_base_path(tmp.path());
        memory_service
            .ensure_agent_layout("agent.v2")
            .await
            .unwrap();

        let tier_fields = [(
            "summary".to_string(),
            Value::String("Completed two checkpoints".to_string()),
        )]
        .into_iter()
        .collect();
        memory_service
            .save_native_tier(
                "agent.v2",
                &definition.memory_tiers[0],
                Some("g1"),
                &native_tier_record(
                    &memory_service,
                    "agent.v2",
                    &definition.memory_tiers[0],
                    Some("g1"),
                    tier_fields,
                ),
            )
            .await
            .unwrap();

        let tier_interpreter = MemoryTierInterpreter::new(memory_service.clone());
        let runtime_inputs = PromptPipelineRuntimeInputs {
            definition: &definition,
            agent_id: "agent.v2",
            triggered_goal_id: "g1",
            memory_service: &memory_service,
            memory_tier_interpreter: &tier_interpreter,
            feedback_injections: HashMap::new(),
        };

        let rendered = PromptPipelineInterpreter
            .assemble_runtime(
                definition.prompt_pipeline.as_ref().unwrap(),
                &runtime_inputs,
            )
            .await
            .unwrap();
        assert!(rendered.contains("local_tier"));
        assert!(rendered.contains("task_progress:"));
        assert!(rendered.contains("Completed two checkpoints"));
    }

    #[test]
    fn assemble_renders_inline_content_section() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: instructions
    content: "Follow these steps carefully."
"#,
        );
        let result = PromptPipelineInterpreter
            .assemble(&pipeline, &PromptPipelineInputs::default())
            .unwrap();
        assert!(result.contains("## instructions"));
        assert!(result.contains("Follow these steps carefully."));
    }

    #[test]
    fn assemble_skips_empty_inline_content_section() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: maybe_empty
    content: "   "
"#,
        );
        let result = PromptPipelineInterpreter
            .assemble(&pipeline, &PromptPipelineInputs::default())
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn assemble_fails_when_required_inline_content_empty() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: critical
    content: "   "
    required: true
"#,
        );
        let err = PromptPipelineInterpreter
            .assemble(&pipeline, &PromptPipelineInputs::default())
            .unwrap_err();
        assert_eq!(
            err,
            PromptPipelineError::MissingRequiredSection {
                section: "critical".to_string(),
                source_ref: "inline content".to_string()
            }
        );
    }

    #[test]
    fn assemble_mixes_source_and_content_sections() {
        let pipeline = parse_pipeline(
            r#"
sections:
  - name: persona
    source: definition.persona
    required: true
  - name: login_steps
    content: "Step 1: Open browser. Step 2: Navigate to site."
"#,
        );
        let mut inputs = PromptPipelineInputs::default();
        inputs.sources.insert(
            "definition.persona".to_string(),
            "You are a helpful bot.".to_string(),
        );
        let result = PromptPipelineInterpreter
            .assemble(&pipeline, &inputs)
            .unwrap();
        assert!(result.contains("## persona"));
        assert!(result.contains("You are a helpful bot."));
        assert!(result.contains("## login_steps"));
        assert!(result.contains("Step 1: Open browser."));
    }
}
