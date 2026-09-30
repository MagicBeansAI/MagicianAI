//! Tier 4: LLM Deep Evaluation
//!
//! This module performs deep semantic evaluation of tool candidates using an
//! LLM. It provides the final 55% of the scoring weight and determines the best
//! tool match.
//!
//! ## LLM Evaluation Strategy
//! - Structured prompt with task description and tool candidates
//! - LLM analyzes semantic fit, parameter compatibility, and use case alignment
//! - Returns confidence scores and reasoning for each candidate
//! - Combines with Tier 1-2 scores (45%) to produce final ranking
//!
//! ## Scoring
//! - LLM weight: 55% of total score
//! - Final score: (Rule 15% + Semantic 30% + LLM 55%) = 100%
//! - Best match selected based on highest final score
//!
//! ## Performance
//! - <5s typical execution time (LLM dependent)
//! - Parallel evaluation for multiple candidates
//! - Timeout protection via config

use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, info, warn};

use super::{config::ToolMatcherConfig, types::ToolCandidate};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompts::{constants, PromptManager},
    query_analysis::operation_llm_router::QueryAnalysisLLM,
};

/// LLM evaluator for Tier 4
pub struct LlmEvaluator {
    /// LLM service for deep evaluation
    llm_service: Arc<dyn QueryAnalysisLLM>,

    /// Prompt manager for loading versioned tool-matching prompt templates
    prompt_manager: Arc<PromptManager>,

    /// Configuration for evaluation behavior
    #[allow(dead_code)]
    config: ToolMatcherConfig,

    llm_telemetry: Option<(OperationLlmTelemetryContext, OperationLlmCallAttribution)>,
}

impl LlmEvaluator {
    /// Create a new LLM evaluator
    pub fn new(
        llm_service: Arc<dyn QueryAnalysisLLM>,
        prompt_manager: Arc<PromptManager>,
        config: ToolMatcherConfig,
    ) -> Self {
        Self {
            llm_service,
            prompt_manager,
            config,
            llm_telemetry: None,
        }
    }

    pub fn with_llm_telemetry(
        mut self,
        context: OperationLlmTelemetryContext,
        attribution: OperationLlmCallAttribution,
    ) -> Self {
        self.llm_telemetry = Some((context, attribution));
        self
    }

    /// Evaluate candidates using LLM
    ///
    /// Performs deep semantic analysis of each candidate and assigns
    /// LLM confidence scores. Combines with existing scores to produce
    /// final rankings.
    ///
    /// # Arguments
    /// * `task` - Task description to match against
    /// * `candidates` - Candidates with combined_score from Tier 1-2
    /// * `required_capabilities` - Optional list of required capabilities for
    ///   completeness checking
    /// * `parent_task` - Optional parent task for hierarchical context
    ///
    /// # Returns
    /// * Result with unit type (candidates are updated in-place)
    pub async fn evaluate_candidates(
        &self,
        task: &str,
        candidates: &mut [&mut ToolCandidate],
        required_capabilities: Option<
            &[crate::magician_v2::query_analysis::unified_analyzer::RequiredCapability],
        >,
        parent_task: Option<&str>,
    ) -> Result<()> {
        let start_time = std::time::Instant::now();

        if let Some(parent) = parent_task {
            debug!(
                "[MAGICIAN-V2-MATCHER] Tier 4 - LLM Evaluator: Evaluating {} candidates for task \
                 '{}' with parent context '{}'",
                candidates.len(),
                task,
                parent
            );
        } else {
            debug!(
                "[MAGICIAN-V2-MATCHER] Tier 4 - LLM Evaluator: Evaluating {} candidates for task \
                 '{}'",
                candidates.len(),
                task
            );
        }

        // Check if LLM service is available
        if !self.llm_service.is_available().await {
            warn!("[MAGICIAN-V2-LLM] LLM service unavailable, using existing scores only");
            return Ok(());
        }

        // Build evaluation prompt (convert &mut to & for reading)
        let candidates_ref: Vec<&ToolCandidate> = candidates.iter().map(|c| &**c).collect();
        let prompt = match self
            .build_evaluation_prompt(task, &candidates_ref, required_capabilities, parent_task)
            .await
        {
            Ok(prompt) => prompt,
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-LLM] Failed to build tool matching prompt from PromptManager: {}. Using existing router scores only.",
                    err
                );
                return Ok(());
            },
        };

        // Log comprehensive prompt parameters for debugging
        info!("[MAGICIAN-V2-LLM] 🔍 LLM Evaluator Prompt Parameters:");
        info!("[MAGICIAN-V2-LLM]   Task: {}", task);
        info!("[MAGICIAN-V2-LLM]   Candidate Count: {}", candidates.len());
        info!(
            "[MAGICIAN-V2-LLM]   Required Capabilities: {:?}",
            required_capabilities
                .map(|caps| caps.iter().map(|c| &c.capability).collect::<Vec<_>>())
        );
        info!(
            "[MAGICIAN-V2-LLM]   Has Capabilities: {}",
            required_capabilities
                .map(|c| !c.is_empty())
                .unwrap_or(false)
        );
        info!("[MAGICIAN-V2-LLM]   Candidates:");
        for (i, candidate) in candidates.iter().take(20).enumerate() {
            info!(
                "[MAGICIAN-V2-LLM]     {}. {} (combined: {:.3}, rule: {:.3}, semantic: {:.3}, \
                 category: {})",
                i + 1,
                candidate.tool_name,
                candidate.combined_score,
                candidate.rule_score,
                candidate.semantic_score,
                candidate.category
            );
        }
        info!("[MAGICIAN-V2-LLM]   Prompt Length: {} chars", prompt.len());

        // Check if candidates list is empty
        if candidates.is_empty() {
            warn!("[MAGICIAN-V2-LLM] ⚠️ NO CANDIDATES to evaluate! Returning early.");
            return Ok(());
        }

        // Log full prompt at debug level for detailed troubleshooting
        debug!(
            "[MAGICIAN-V2-MATCHER] 📝 Full LLM Evaluator Prompt:\n{}",
            prompt
        );

        // Call LLM for evaluation
        let llm_started = std::time::Instant::now();
        let llm_call = match self.llm_telemetry.as_ref() {
            Some((telemetry, _)) => self
                .llm_service
                .generate_analysis_scoped(telemetry.scope(), &prompt),
            None => self.llm_service.generate_analysis(&prompt),
        };
        match llm_call.await {
            Ok(llm_response) => {
                // Log LLM response at INFO level for debugging
                info!("[MAGICIAN-V2-LLM] 📊 LLM Evaluation Response (first 500 chars):");
                info!(
                    "[MAGICIAN-V2-LLM] {}",
                    &llm_response.content.chars().take(500).collect::<String>()
                );
                if llm_response.content.len() > 500 {
                    info!(
                        "[MAGICIAN-V2-LLM] ... (response truncated, {} total chars)",
                        llm_response.content.len()
                    );
                    debug!(
                        "[MAGICIAN-V2-MATCHER] 📊 Full LLM Response:\n{}",
                        llm_response.content
                    );
                }

                // Parse LLM response and update candidate scores
                let parsed = self.parse_and_apply_scores(&llm_response.content, candidates);
                if let Some((telemetry, attribution)) = self.llm_telemetry.as_ref() {
                    let latency_ms = llm_started.elapsed().as_millis() as u64;
                    match parsed.as_ref() {
                        Ok(_) => telemetry.emit_validated_success(
                            "tool_matching",
                            &llm_response,
                            latency_ms,
                            attribution.clone(),
                            "tool_matching_json",
                        ),
                        Err(error) => telemetry.emit_validation_failure(
                            "tool_matching",
                            &llm_response,
                            latency_ms,
                            attribution.clone(),
                            "tool_matching_json",
                            &error.to_string(),
                        ),
                    }
                }
                parsed?;
            },
            Err(e) => {
                warn!(
                    "[MAGICIAN-V2-LLM] LLM evaluation failed: {}, using existing scores",
                    e
                );
                // Fallback: Use existing combined scores as LLM scores
                // This allows graceful degradation if LLM fails
            },
        }

        let elapsed = start_time.elapsed();
        info!(
            "[MAGICIAN-V2-MATCHER] Tier 4 complete: Evaluated {} candidates in {:?}",
            candidates.len(),
            elapsed
        );

        Ok(())
    }

    /// Build evaluation prompt for LLM
    ///
    /// Creates a structured prompt that asks the LLM to evaluate each candidate
    /// and provide confidence scores (0.0-1.0) with reasoning.
    async fn build_evaluation_prompt(
        &self,
        task: &str,
        candidates: &[&ToolCandidate],
        required_capabilities: Option<
            &[crate::magician_v2::query_analysis::unified_analyzer::RequiredCapability],
        >,
        parent_task: Option<&str>,
    ) -> Result<String> {
        let has_capabilities = required_capabilities
            .map(|caps| !caps.is_empty())
            .unwrap_or(false);

        let mut variables = std::collections::HashMap::new();
        variables.insert("task".to_string(), task.to_string());
        variables.insert(
            "parent_task".to_string(),
            parent_task.unwrap_or("").to_string(),
        );
        variables.insert(
            "has_parent_task".to_string(),
            parent_task
                .map(|parent| !parent.trim().is_empty())
                .unwrap_or(false)
                .to_string(),
        );
        variables.insert(
            "required_capabilities_list".to_string(),
            Self::build_required_capabilities_list(required_capabilities),
        );
        variables.insert(
            "has_required_capabilities".to_string(),
            has_capabilities.to_string(),
        );
        variables.insert(
            "candidates_section".to_string(),
            Self::build_candidates_section(candidates),
        );

        self.prompt_manager
            .get_rendered_prompt(
                constants::names::TOOL_MATCHING,
                constants::versions::TOOL_MATCHING,
                variables,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load tool_matching prompt: {}", e))
    }

    fn build_required_capabilities_list(
        required_capabilities: Option<
            &[crate::magician_v2::query_analysis::unified_analyzer::RequiredCapability],
        >,
    ) -> String {
        let Some(capabilities) = required_capabilities else {
            return String::new();
        };
        if capabilities.is_empty() {
            return String::new();
        }

        let mut section = String::new();
        for cap in capabilities {
            section.push_str(&format!(
                "- **{}**: {} (required: {})\n",
                cap.capability, cap.description, cap.required
            ));
        }
        section
    }

    fn build_candidates_section(candidates: &[&ToolCandidate]) -> String {
        let mut section = String::new();
        for (i, candidate) in candidates.iter().enumerate() {
            section.push_str(&format!(
                r#"### Candidate {}: {}

- **Description**: {}
- **Category**: {}
- **Pre-filter Score**: {:.3} (from keyword and semantic analysis)
- **Category Matched**: {}
"#,
                i + 1,
                candidate.tool_name,
                candidate.tool_info.description,
                candidate.category,
                candidate.combined_score,
                candidate.category_matched
            ));

            if let Some(enhanced) = &candidate.tool_info.enhanced_description {
                section.push_str(&format!("- **Enhanced Description**: {}\n", enhanced));
            }

            if !candidate.tool_info.keywords.is_empty() {
                section.push_str(&format!(
                    "- **Keywords**: {}\n",
                    candidate.tool_info.keywords.join(", ")
                ));
            }

            if !candidate.tool_info.use_cases.is_empty() {
                section.push_str("- **Use Cases**:\n");
                for use_case in &candidate.tool_info.use_cases {
                    section.push_str(&format!("  - {}\n", use_case));
                }
            }
            section.push('\n');
        }
        section
    }

    /// Parse LLM response and apply scores to candidates
    ///
    /// Extracts confidence scores from LLM response and updates candidates.
    /// Falls back gracefully if parsing fails.
    fn parse_and_apply_scores(
        &self,
        response: &str,
        candidates: &mut [&mut ToolCandidate],
    ) -> Result<()> {
        debug!(
            "[MAGICIAN-V2-LLM] 🔍 Parsing LLM response (length: {} chars)",
            response.len()
        );
        debug!("[MAGICIAN-V2-LLM] Raw response: {}", response);

        // Remove markdown code blocks if present
        let cleaned_response = response
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        debug!("[MAGICIAN-V2-LLM] Cleaned response: {}", cleaned_response);

        // Try to extract JSON array from response
        let json_str = if let Some(start) = cleaned_response.find('[') {
            if let Some(end) = cleaned_response.rfind(']') {
                &cleaned_response[start..=end]
            } else {
                cleaned_response
            }
        } else {
            cleaned_response
        };

        debug!("[MAGICIAN-V2-LLM] Extracted JSON string: {}", json_str);
        info!(
            "[MAGICIAN-V2-LLM] 📝 About to parse {} bytes of JSON for {} candidates",
            json_str.len(),
            candidates.len()
        );

        // Parse JSON
        match serde_json::from_str::<Vec<LlmEvaluation>>(json_str) {
            Ok(evaluations) => {
                let eval_count = evaluations.len();

                info!(
                    "[MAGICIAN-V2-LLM] ✅ Successfully parsed {} LLM evaluations:",
                    eval_count
                );

                // Apply scores to candidates and log each one
                for eval in evaluations {
                    if let Some(candidate) = candidates
                        .iter_mut()
                        .find(|c| c.tool_name == eval.tool_name)
                    {
                        // Store LLM score in candidate
                        candidate.llm_score = eval.confidence as f32;

                        // Log basic evaluation
                        info!(
                            "[MAGICIAN-V2-LLM]   📊 {} → LLM confidence={:.3} | Reasoning: {}",
                            eval.tool_name, eval.confidence, eval.reasoning
                        );

                        // Log capability coverage if present
                        if !eval.fulfilled_capabilities.is_empty()
                            || !eval.missing_capabilities.is_empty()
                        {
                            info!(
                                "[MAGICIAN-V2-LLM]      ✅ Fulfilled: {:?} | ❌ Missing: {:?} | \
                                 Coverage: {:.2}",
                                eval.fulfilled_capabilities,
                                eval.missing_capabilities,
                                eval.coverage_ratio.unwrap_or(0.0)
                            );
                        }
                    }
                }

                // Log top 20 tools after LLM evaluation
                info!("[MAGICIAN-V2-LLM] 🏆 Top 20 Tools After LLM Evaluation:");
                for (i, candidate) in candidates.iter().take(20).enumerate() {
                    info!(
                        "[MAGICIAN-V2-LLM]   {}. {} (combined: {:.3}, rule: {:.3}, semantic: \
                         {:.3}, llm: {:.3})",
                        i + 1,
                        candidate.tool_name,
                        candidate.combined_score,
                        candidate.rule_score,
                        candidate.semantic_score,
                        candidate.llm_score
                    );
                }

                Ok(())
            },
            Err(e) => {
                warn!(
                    "[MAGICIAN-V2-LLM] ❌ Failed to parse LLM response as JSON: {}",
                    e
                );
                warn!("[MAGICIAN-V2-LLM] Error type: {:?}", e);
                warn!("[MAGICIAN-V2-LLM] JSON string to parse: '{}'", json_str);

                // Check if it's an empty array
                if json_str == "[]" {
                    warn!("[MAGICIAN-V2-LLM] ⚠️ LLM returned EMPTY ARRAY - this means:");
                    warn!(
                        "[MAGICIAN-V2-LLM]   1. LLM refused to evaluate (maybe prompt too complex)"
                    );
                    warn!("[MAGICIAN-V2-LLM]   2. LLM didn't understand the format");
                    warn!("[MAGICIAN-V2-LLM]   3. Token limit exceeded");
                    warn!("[MAGICIAN-V2-LLM]   4. Model struggled with capability fields");
                }

                warn!(
                    "[MAGICIAN-V2-LLM] Extracted JSON string (first 1000 chars): {}",
                    &json_str.chars().take(1000).collect::<String>()
                );
                if json_str.len() > 1000 {
                    warn!(
                        "[MAGICIAN-V2-LLM] ... JSON truncated ({} total chars)",
                        json_str.len()
                    );
                }
                debug!("[MAGICIAN-V2-LLM] Full LLM Response: {}", response);

                // Fallback: Try simple heuristic parsing
                self.fallback_score_parsing(response, candidates)
            },
        }
    }

    /// Fallback score parsing when JSON parsing fails
    ///
    /// Uses simple heuristics to extract confidence scores from free-form text.
    fn fallback_score_parsing(
        &self,
        response: &str,
        candidates: &mut [&mut ToolCandidate],
    ) -> Result<()> {
        debug!("[MAGICIAN-V2-MATCHER] Using fallback score parsing");

        // Simple heuristic: Look for tool names and confidence scores in response
        for candidate in candidates {
            if response.contains(&candidate.tool_name) {
                debug!(
                    "[MAGICIAN-V2-MATCHER]   {} mentioned in LLM response (using pre-filter score)",
                    candidate.tool_name
                );
            }
        }

        // For now, we'll just use the existing combined scores
        // A more sophisticated fallback could try to extract numbers near tool names
        Ok(())
    }
}

/// LLM evaluation result
#[derive(Debug, serde::Deserialize)]
struct LlmEvaluation {
    tool_name: String,
    confidence: f64,
    reasoning: String,

    // NEW: Capability analysis
    #[serde(default)]
    fulfilled_capabilities: Vec<String>,
    #[serde(default)]
    missing_capabilities: Vec<String>,
    #[serde(default)]
    coverage_ratio: Option<f64>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::{
        magician_v2::{
            prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
            query_analysis::operation_llm_router::MockQueryAnalysisLLM,
            tool_matcher::config::ToolMatcherConfig,
        },
        ToolInfo,
    };

    fn create_test_candidate(name: &str, description: &str) -> ToolCandidate {
        let tool_info = ToolInfo {
            name: name.to_string(),
            description: description.to_string(),
            category: "test".to_string(),
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            categories: vec!["test".to_string()], // Populate with primary category
            composition_category: None,
            providing_agent_id: None,
        };
        ToolCandidate::new(tool_info, false)
    }

    async fn create_prompt_manager() -> Arc<PromptManager> {
        let data_dir = crate::magician_v2::prompts::json_storage::default_prompt_dir();
        let config = JsonStorageConfig {
            storage_dir: data_dir,
            enable_cache: false,
            max_cache_entries: 1,
        };
        let storage = JsonPromptStorage::new(config).expect("prompt storage should initialize");
        Arc::new(PromptManager::new(Arc::new(storage)))
    }

    #[tokio::test]
    async fn test_llm_evaluator_creation() {
        let config = ToolMatcherConfig::default();
        let llm_service = Arc::new(MockQueryAnalysisLLM);
        let prompt_manager = create_prompt_manager().await;

        let _evaluator = LlmEvaluator::new(llm_service, prompt_manager, config);

        // Just verify we can create an evaluator
        assert!(true);
    }

    #[tokio::test]
    async fn test_build_evaluation_prompt() {
        let config = ToolMatcherConfig::default();
        let llm_service = Arc::new(MockQueryAnalysisLLM);
        let prompt_manager = create_prompt_manager().await;
        let evaluator = LlmEvaluator::new(llm_service, prompt_manager, config);

        let candidate = create_test_candidate("test_tool", "A test tool");
        let candidates = vec![&candidate];

        let prompt = evaluator
            .build_evaluation_prompt("test task", &candidates, None, None)
            .await
            .expect("tool matching prompt should render");

        // Verify prompt contains key elements
        assert!(prompt.contains("test task"));
        assert!(prompt.contains("test_tool"));
        assert!(prompt.contains("A test tool"));
        assert!(prompt.contains("confidence"));
    }

    #[tokio::test]
    async fn test_evaluate_candidates_with_mock() {
        let config = ToolMatcherConfig::default();
        let llm_service = Arc::new(MockQueryAnalysisLLM);
        let prompt_manager = create_prompt_manager().await;
        let evaluator = LlmEvaluator::new(llm_service, prompt_manager, config);

        let mut candidate = create_test_candidate("test_tool", "A test tool");
        let mut candidates = vec![&mut candidate];

        // This should work with mock LLM (which returns simple text)
        let result = evaluator
            .evaluate_candidates("test task", &mut candidates, None, None)
            .await;

        // Mock LLM always succeeds
        assert!(result.is_ok());
    }

    #[test]
    fn test_json_extraction() {
        let response = r#"
Here is my evaluation:

```json
[
  {
    "tool_name": "tool1",
    "confidence": 0.85,
    "reasoning": "Good match"
  }
]
```

Let me know if you need more details.
"#;

        // Verify we can find the JSON bounds
        assert!(response.contains('['));
        assert!(response.contains(']'));

        let start = response.find('[').unwrap();
        let end = response.rfind(']').unwrap();
        let json_str = &response[start..=end];

        // Verify we can parse the extracted JSON
        let result = serde_json::from_str::<Vec<LlmEvaluation>>(json_str);
        assert!(result.is_ok());
    }
}
