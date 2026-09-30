use std::collections::HashSet;
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompts::PromptManager,
    query_analysis::operation_llm_router::QueryAnalysisLLM,
};
use runtime_core::{SemanticSearch, ToolCatalog};

use super::config::ToolMatcherConfig;
use super::llm_evaluator::LlmEvaluator;
use super::types::{
    self, MatchingTier, TierScores, ToolCandidate, ToolMatchError, ToolMatchRequest,
    ToolMatchResult, ToolMetadata,
};

/// Default success rate constant for tools (exposed publicly for helper usage)
pub const DEFAULT_SUCCESS_RATE: f32 = 0.9;

/// V2 Tool Matcher with compact routing and optional LLM disambiguation.
pub struct V2ToolMatcher {
    /// Security-filtered tool catalog service
    tool_catalog: Arc<dyn ToolCatalog>,

    /// Optional semantic search for Tier 2
    semantic_search: Option<Arc<dyn SemanticSearch>>,

    /// LLM service for disambiguation fallback
    llm_service: Arc<dyn QueryAnalysisLLM>,

    /// Prompt manager for loading versioned tool-matching prompts
    prompt_manager: Arc<PromptManager>,

    /// Configuration for matching behavior
    config: ToolMatcherConfig,

    /// Event broadcaster for progress tracking (optional)
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl V2ToolMatcher {
    /// Create a new V2 Tool Matcher
    pub fn new(
        tool_catalog: Arc<dyn ToolCatalog>,
        semantic_search: Option<Arc<dyn SemanticSearch>>,
        llm_service: Arc<dyn QueryAnalysisLLM>,
        prompt_manager: Arc<PromptManager>,
        config: ToolMatcherConfig,
    ) -> Result<Self, String> {
        Self::new_with_broadcaster(
            tool_catalog,
            semantic_search,
            llm_service,
            prompt_manager,
            config,
            None, // No event broadcaster by default
        )
    }

    /// Create a new V2 Tool Matcher with event broadcaster for progress
    /// tracking
    pub fn new_with_broadcaster(
        tool_catalog: Arc<dyn ToolCatalog>,
        semantic_search: Option<Arc<dyn SemanticSearch>>,
        llm_service: Arc<dyn QueryAnalysisLLM>,
        prompt_manager: Arc<PromptManager>,
        config: ToolMatcherConfig,
        event_broadcaster: Option<
            Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
        >,
    ) -> Result<Self, String> {
        // Validate configuration
        config.weights.validate()?;

        info!(
            "[MAGICIAN-V2-MATCHER] Initializing compact V2ToolMatcher: max_llm_candidates={}, \
             fail_fast={}, progress_tracking={}",
            config.limits.max_llm_candidates,
            config.fail_fast,
            event_broadcaster.is_some()
        );

        Ok(Self {
            tool_catalog,
            semantic_search,
            llm_service,
            prompt_manager,
            config,
            event_broadcaster,
        })
    }

    /// Match a tool for the given request.
    ///
    /// Pipeline:
    /// 1. Infer coarse route hints (`browser`, `files`, `search`, `shell`)
    /// 2. Score all allowed tools with route + lexical (+ optional semantic) signals
    /// 3. Run LLM disambiguation on top-N candidates
    /// 4. Return best match by final confidence
    pub async fn match_tool(
        &self,
        request: &ToolMatchRequest,
    ) -> Result<ToolMatchResult, ToolMatchError> {
        let start_time = std::time::Instant::now();

        info!(
            "[MAGICIAN-V2-MATCHER] Tool matching started: task='{}', suggested_categories={:?}",
            request.task, request.suggested_categories
        );

        let route_hints = infer_route_hints(&request.task, &request.suggested_categories);
        debug!(
            "[MAGICIAN-V2-MATCHER] Route hints inferred: {:?}",
            route_hints
        );

        if let Some(ref broadcaster) = self.event_broadcaster {
            if let (Some(ref execution_id), Some(ref correlation_id)) =
                (&request.execution_id, &request.correlation_id)
            {
                let tier_candidates = route_hints
                    .iter()
                    .map(|hint| crate::magician_v2::realtime_events::TierCandidate {
                        tool_name: hint.clone(),
                        score: 1.0,
                        category: hint.clone(),
                    })
                    .collect();

                broadcaster.tool_matching_tier_started(
                    execution_id,
                    correlation_id,
                    0,
                    "Route Inference".to_string(),
                    "Inferring coarse tool route hints".to_string(),
                );
                broadcaster.tool_matching_tier_completed(
                    execution_id,
                    correlation_id,
                    0,
                    "Route Inference".to_string(),
                    route_hints.len(),
                    0,
                    tier_candidates,
                );
            }
        }

        let all_tools = self
            .tool_catalog
            .all_tools(&request.execution_context)
            .await
            .map_err(|e| ToolMatchError::SecurityFilteringFailed(e.to_string()))?;

        if all_tools.is_empty() {
            return Err(ToolMatchError::NoMatchingCategories {
                categories: vec!["all".to_string()],
            });
        }

        let semantic_scores = self.semantic_scores(&request.task).await;

        let mut candidates: Vec<ToolCandidate> = all_tools
            .into_iter()
            .map(|tool_info| {
                let route_match = route_hints
                    .iter()
                    .any(|hint| tool_matches_hint(&tool_info, hint));
                let mut candidate =
                    ToolCandidate::new(tool_info, route_hints.is_empty() || route_match);

                let lexical = lexical_score(&request.task, &candidate);
                let semantic = semantic_scores
                    .get(&candidate.tool_name)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                let route = if route_match { 1.0 } else { 0.0 };

                candidate.rule_score = lexical;
                candidate.semantic_score = semantic;

                let mut combined = if route_hints.is_empty() {
                    0.25 + (lexical * 0.55) + (semantic * 0.20)
                } else {
                    0.10 + (route * 0.60) + (lexical * 0.20) + (semantic * 0.10)
                };

                if request
                    .parent_tool_name
                    .as_deref()
                    .map(|parent| parent.eq_ignore_ascii_case(&candidate.tool_name))
                    .unwrap_or(false)
                {
                    combined += 0.1;
                }

                candidate.combined_score = combined.clamp(0.0, 1.0);
                candidate
            })
            .collect();

        candidates.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some(ref broadcaster) = self.event_broadcaster {
            if let (Some(ref execution_id), Some(ref correlation_id)) =
                (&request.execution_id, &request.correlation_id)
            {
                let top_candidates = candidates
                    .iter()
                    .take(5)
                    .map(
                        |candidate| crate::magician_v2::realtime_events::TierCandidate {
                            tool_name: candidate.tool_name.clone(),
                            score: candidate.combined_score as f64,
                            category: candidate.category.clone(),
                        },
                    )
                    .collect();

                broadcaster.tool_matching_tier_started(
                    execution_id,
                    correlation_id,
                    1,
                    "Route + Lexical Scoring".to_string(),
                    format!("Scoring {} tools", candidates.len()),
                );
                broadcaster.tool_matching_tier_completed(
                    execution_id,
                    correlation_id,
                    1,
                    "Route + Lexical Scoring".to_string(),
                    candidates.len(),
                    0,
                    top_candidates,
                );
            }
        }

        let llm_limit = self.config.limits.max_llm_candidates.clamp(1, 4);
        let mut selected: Vec<&mut ToolCandidate> = candidates.iter_mut().take(llm_limit).collect();

        if selected.is_empty() {
            return Err(ToolMatchError::NoConfidentMatches {
                threshold: self.config.limits.min_confidence_threshold,
                best_confidence: 0.0,
            });
        }

        if let Some(ref broadcaster) = self.event_broadcaster {
            if let (Some(ref execution_id), Some(ref correlation_id)) =
                (&request.execution_id, &request.correlation_id)
            {
                broadcaster.tool_matching_tier_started(
                    execution_id,
                    correlation_id,
                    2,
                    "LLM Disambiguation".to_string(),
                    format!("Evaluating top {} candidates", selected.len()),
                );
            }
        }

        let mut llm_evaluator = LlmEvaluator::new(
            self.llm_service.clone(),
            self.prompt_manager.clone(),
            self.config.clone(),
        );
        if let Some(broadcaster) = self.event_broadcaster.as_ref() {
            llm_evaluator = llm_evaluator.with_llm_telemetry(
                OperationLlmTelemetryContext::new(
                    Arc::clone(broadcaster),
                    request.execution_context.principal.clone(),
                    request.execution_context.workspace.clone(),
                    "tool_matching",
                ),
                OperationLlmCallAttribution {
                    execution_id: request.execution_id.clone(),
                    task_id: request.execution_context.metadata.get("task_id").cloned(),
                    agent_id: request.execution_context.metadata.get("agent_id").cloned(),
                    ..OperationLlmCallAttribution::default()
                },
            );
        }
        llm_evaluator
            .evaluate_candidates(
                &request.task,
                &mut selected,
                request.required_capabilities.as_deref(),
                request.parent_task.as_deref(),
            )
            .await
            .map_err(|e| ToolMatchError::LlmEvaluationFailed(e.to_string()))?;

        let llm_used = selected.iter().any(|candidate| candidate.llm_score > 0.0);
        for candidate in &mut selected {
            candidate.final_score = if candidate.llm_score > 0.0 {
                (candidate.llm_score * self.config.weights.llm_weight)
                    + (candidate.combined_score * (1.0 - self.config.weights.llm_weight))
            } else {
                candidate.combined_score
            }
            .clamp(0.0, 1.0);

            candidate.score_reasoning = if candidate.llm_score > 0.0 {
                format!(
                    "llm:{:.2} + router:{:.2}",
                    candidate.llm_score, candidate.combined_score
                )
            } else {
                format!(
                    "router:{:.2} (LLM unavailable/fallback)",
                    candidate.combined_score
                )
            };
        }

        selected.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some(ref broadcaster) = self.event_broadcaster {
            if let (Some(ref execution_id), Some(ref correlation_id)) =
                (&request.execution_id, &request.correlation_id)
            {
                let top_candidates = selected
                    .iter()
                    .take(5)
                    .map(
                        |candidate| crate::magician_v2::realtime_events::TierCandidate {
                            tool_name: candidate.tool_name.clone(),
                            score: candidate.final_score as f64,
                            category: candidate.category.clone(),
                        },
                    )
                    .collect();

                broadcaster.tool_matching_tier_completed(
                    execution_id,
                    correlation_id,
                    2,
                    "LLM Disambiguation".to_string(),
                    selected.len(),
                    0,
                    top_candidates,
                );
            }
        }

        let best_match = selected.first().ok_or(ToolMatchError::NoConfidentMatches {
            threshold: self.config.limits.min_confidence_threshold,
            best_confidence: 0.0,
        })?;

        let mut threshold = request
            .confidence_threshold
            .max(self.config.limits.min_confidence_threshold);
        if !llm_used {
            threshold = threshold.min(0.35);
        }

        if best_match.final_score < threshold {
            return Err(ToolMatchError::NoConfidentMatches {
                threshold,
                best_confidence: best_match.final_score,
            });
        }

        let match_time_ms = start_time.elapsed().as_millis() as u64;

        let result = ToolMatchResult {
            tool_name: best_match.tool_name.clone(),
            confidence: best_match.final_score,
            matching_tier: if llm_used {
                MatchingTier::LlmEvaluation
            } else {
                MatchingTier::RuleBased
            },
            tier_scores: TierScores {
                rule_score: best_match.rule_score,
                semantic_score: best_match.semantic_score,
                llm_score: best_match.llm_score,
                combined_pre_llm: best_match.combined_score,
            },
            tool_metadata: ToolMetadata {
                name: best_match.tool_name.clone(),
                category: best_match.category.clone(),
                description: best_match.tool_info.description.clone(),
                parameters: convert_parameters_to_v2(&best_match.tool_info.parameters),
                enhanced_description: best_match.tool_info.enhanced_description.clone(),
                keywords: best_match.tool_info.keywords.clone(),
                use_cases: best_match.tool_info.use_cases.clone(),
            },
            category_matched: best_match.category_matched,
            match_time_ms,
            providing_agent_id: best_match.tool_info.providing_agent_id.clone(),
        };

        info!(
            "[MAGICIAN-V2-MATCHER] Match complete: {} (confidence={:.3}, llm_used={})",
            result.tool_name, result.confidence, llm_used
        );

        Ok(result)
    }

    async fn semantic_scores(&self, task: &str) -> std::collections::HashMap<String, f32> {
        let Some(semantic_search) = &self.semantic_search else {
            return std::collections::HashMap::new();
        };

        match semantic_search.search_all_tools_with_scores(task).await {
            Ok(matches) => matches
                .into_iter()
                .map(|item| (item.tool_name, item.similarity_score as f32))
                .collect(),
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-MATCHER] Semantic scoring failed, continuing without it: {}",
                    err
                );
                std::collections::HashMap::new()
            },
        }
    }
}

fn infer_route_hints(task: &str, suggested_categories: &[String]) -> HashSet<String> {
    let mut hints = HashSet::new();

    for category in suggested_categories {
        if let Some(mapped) = canonical_hint(category) {
            hints.insert(mapped.to_string());
        }
    }

    let lower = task.to_lowercase();
    if lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("www.")
        || ["browser", "web", "site", "page", "tab", "click", "navigate"]
            .iter()
            .any(|token| lower.contains(token))
    {
        hints.insert("browser".to_string());
    }

    if [
        "shell", "command", "terminal", "cli", "jq", "python", "python3", "script",
    ]
    .iter()
    .any(|token| lower.contains(token))
    {
        hints.insert("shell".to_string());
    }

    if [
        "file",
        "folder",
        "directory",
        "path",
        "read",
        "write",
        "append",
        "copy",
        "move",
        "rename",
        "delete",
        "mkdir",
    ]
    .iter()
    .any(|token| lower.contains(token))
    {
        hints.insert("files".to_string());
    }

    if ["search", "find", "grep", "rg", "lookup", "scan", "match"]
        .iter()
        .any(|token| lower.contains(token))
    {
        hints.insert("search".to_string());
    }

    hints
}

fn canonical_hint(raw: &str) -> Option<&'static str> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() {
        return None;
    }

    if value.contains("browser") || value.contains("web") || value.contains("navigate") {
        return Some("browser");
    }
    if value.contains("shell") || value.contains("command") || value.contains("terminal") {
        return Some("shell");
    }
    if value.contains("file") || value.contains("filesystem") || value.contains("workspace") {
        return Some("files");
    }
    if value.contains("search") || value.contains("grep") || value.contains("find") {
        return Some("search");
    }

    None
}

fn tool_matches_hint(tool: &runtime_core::ToolInfo, hint: &str) -> bool {
    tool.name.eq_ignore_ascii_case(hint)
        || tool.category.eq_ignore_ascii_case(hint)
        || tool
            .categories
            .iter()
            .any(|category| category.eq_ignore_ascii_case(hint))
        || tool
            .composition_category
            .as_deref()
            .map(|category| category.to_lowercase().contains(hint))
            .unwrap_or(false)
}

fn lexical_score(task: &str, candidate: &ToolCandidate) -> f32 {
    let query_tokens = tokenize(task);
    if query_tokens.is_empty() {
        return 0.0;
    }

    let corpus = format!(
        "{} {} {} {} {}",
        candidate.tool_name,
        candidate.category,
        candidate.tool_info.description,
        candidate.tool_info.keywords.join(" "),
        candidate.tool_info.use_cases.join(" ")
    );
    let corpus_tokens = tokenize(&corpus);
    if corpus_tokens.is_empty() {
        return 0.0;
    }

    let overlap = query_tokens
        .iter()
        .filter(|token| corpus_tokens.contains(*token))
        .count();

    (overlap as f32 / query_tokens.len() as f32).clamp(0.0, 1.0)
}

fn tokenize(value: &str) -> HashSet<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(|token| token.trim().to_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

/// Convert V1 ParameterDefinition to V2 ToolParameter format
fn convert_parameters_to_v2(
    parameters: &[runtime_core::ParameterDefinition],
) -> Vec<types::ToolParameter> {
    parameters
        .iter()
        .map(|param| types::ToolParameter {
            name: param.name.clone(),
            param_type: param.param_type.clone(),
            description: param.description.clone(),
            required: param.required,
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_config_validation() {
        let config = ToolMatcherConfig::default();
        assert!(config.weights.validate().is_ok());
    }

    #[test]
    fn test_route_hint_inference() {
        let hints = infer_route_hints("open https://google.com and click search", &[]);
        assert!(hints.contains("browser"));
    }

    #[test]
    fn test_route_hint_from_category() {
        let hints = infer_route_hints("do task", &["filesystem".to_string()]);
        assert!(hints.contains("files"));
    }
}
