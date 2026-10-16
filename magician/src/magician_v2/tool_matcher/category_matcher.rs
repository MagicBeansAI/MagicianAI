//! Category Fuzzy Matcher
//!
//! This module implements fuzzy matching between LLM-suggested categories and
//! actual tool categories. It uses a two-stage approach:
//! 1. Similarity matching: Find likely category neighbors from the local index
//! 2. LLM validation: Ask LLM to confirm if categories are semantically related
//!
//! Example:
//! - LLM suggests: "connectivity-testing"
//! - Semantic finds: "http-requests" (0.85), "network-testing" (0.78)
//! - LLM validates: "http-requests" is related (0.90)
//! - Combined score: (0.85 * 0.5) + (0.90 * 0.5) = 0.875

use std::sync::Arc;

use anyhow::Result;
use futures_util::future::join_all;
use tracing::{debug, info, warn};

use crate::magician_v2::query_analysis::operation_llm_router::QueryAnalysisLLM;
use runtime_core::SemanticSearch;

/// Configuration for category fuzzy matching
#[derive(Debug, Clone)]
pub struct CategoryMatchConfig {
    /// Minimum combined score threshold (0.0-1.0)
    pub min_match_threshold: f32,

    /// Weight for semantic score (0.0-1.0)
    pub semantic_weight: f32,

    /// Weight for LLM score (0.0-1.0)
    pub llm_weight: f32,

    /// Number of semantic candidates to consider
    pub semantic_top_k: usize,

    /// Whether to enable LLM validation (can disable for speed)
    pub enable_llm_validation: bool,
}

impl Default for CategoryMatchConfig {
    fn default() -> Self {
        Self {
            min_match_threshold: 0.7,
            semantic_weight: 0.5,
            llm_weight: 0.5,
            semantic_top_k: 10,
            enable_llm_validation: true,
        }
    }
}

/// Result of matching a single LLM-suggested category to an actual category
#[derive(Debug, Clone)]
pub struct CategoryMatch {
    /// LLM-suggested category name
    pub suggested_category: String,

    /// Actual category name from tool metadata
    pub matched_category: String,

    /// Semantic similarity score (0.0-1.0)
    pub semantic_score: f32,

    /// LLM validation score (0.0-1.0)
    pub llm_score: f32,

    /// Combined weighted score
    pub combined_score: f32,

    /// LLM reasoning for the match
    pub reasoning: String,
}

/// Category fuzzy matcher service
pub struct CategoryFuzzyMatcher {
    /// Similarity search service for category candidate retrieval
    semantic_service: Arc<dyn SemanticSearch>,

    /// LLM service for validation
    llm_service: Arc<dyn QueryAnalysisLLM>,

    /// Configuration
    config: CategoryMatchConfig,

    /// Event broadcaster for progress tracking (optional)
    event_broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl CategoryFuzzyMatcher {
    /// Create a new category fuzzy matcher
    pub fn new(
        semantic_service: Arc<dyn SemanticSearch>,
        llm_service: Arc<dyn QueryAnalysisLLM>,
        config: CategoryMatchConfig,
        event_broadcaster: Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    ) -> Self {
        Self {
            semantic_service,
            llm_service,
            config,
            event_broadcaster,
        }
    }

    /// Match LLM-suggested categories to actual tool categories
    ///
    /// # Arguments
    /// * `suggested_categories` - Categories recommended by LLM query analysis
    /// * `execution_id` - Optional execution ID for progress broadcasting
    /// * `correlation_id` - Optional correlation ID for progress broadcasting
    ///
    /// # Returns
    /// * Vector of CategoryMatch results, sorted by combined score (descending)
    pub async fn match_categories(
        &self,
        suggested_categories: &[String],
        execution_id: Option<&str>,
        correlation_id: Option<&str>,
    ) -> Result<Vec<CategoryMatch>> {
        info!(
            "[MAGICIAN-V2-CAT] 🔄 Fuzzy category matching: {} suggested categories",
            suggested_categories.len()
        );

        // Broadcast semantic search progress
        if let (Some(broadcaster), Some(tid), Some(cid)) =
            (&self.event_broadcaster, execution_id, correlation_id)
        {
            broadcaster.exploration_progress(
                tid,
                cid,
                0,
                0,
                0.0,
                format!(
                    "Semantic search for {} categories",
                    suggested_categories.len()
                ),
                10.0,
            );
        }

        // Stage 1: Collect all semantic candidates for all suggested categories
        let mut validation_tasks = Vec::new();

        for (idx, suggested) in suggested_categories.iter().enumerate() {
            debug!(
                "[MAGICIAN-V2-CAT] Matching suggested category: '{}'",
                suggested
            );

            // Semantic matching
            let semantic_candidates = self
                .semantic_service
                .search_categories(suggested, self.config.semantic_top_k)
                .await?;

            debug!(
                "[MAGICIAN-V2-CAT]   Found {} semantic candidates for '{}'",
                semantic_candidates.len(),
                suggested
            );

            // Progress update after each semantic search
            if let (Some(broadcaster), Some(tid), Some(cid)) =
                (&self.event_broadcaster, execution_id, correlation_id)
            {
                let progress = 10.0 + (idx + 1) as f64 / suggested_categories.len() as f64 * 20.0;
                broadcaster.exploration_progress(
                    tid,
                    cid,
                    validation_tasks.len(),
                    0,
                    0.0,
                    format!(
                        "Semantic search: {}/{} categories",
                        idx + 1,
                        suggested_categories.len()
                    ),
                    progress,
                );
            }

            // Collect validation tasks (to be run in parallel)
            for (actual_category, semantic_score) in semantic_candidates {
                validation_tasks.push((suggested.clone(), actual_category, semantic_score));
            }
        }

        let llm_start = std::time::Instant::now();
        info!(
            "[MAGICIAN-V2-CAT] 🚀 Starting {} LLM validation calls in parallel \
             (enable_llm_validation={})",
            validation_tasks.len(),
            self.config.enable_llm_validation
        );

        // Broadcast LLM validation start
        if let (Some(broadcaster), Some(tid), Some(cid)) =
            (&self.event_broadcaster, execution_id, correlation_id)
        {
            broadcaster.exploration_progress(
                tid,
                cid,
                validation_tasks.len(),
                0,
                0.0,
                format!("LLM validation: {} category pairs", validation_tasks.len()),
                30.0,
            );
        }

        // Stage 2: Run all LLM validations in parallel
        let llm_service = self.llm_service.clone();
        let enable_llm = self.config.enable_llm_validation;

        let validation_futures: Vec<_> = validation_tasks
            .into_iter()
            .map(|(suggested, actual, semantic_score)| {
                let llm_service = llm_service.clone();
                async move {
                    if enable_llm {
                        let call_start = std::time::Instant::now();
                        // Build LLM validation prompt
                        let prompt = format!(
                            r#"Evaluate if these two tool categories are semantically related.

**Suggested Category**: "{}"
**Actual Category**: "{}"

Consider:
- Do they describe similar operations or domains?
- Would tools in one category be useful for tasks in the other?
- Are they synonyms or closely related concepts?

**CRITICAL INSTRUCTIONS:**
- Return ONLY valid JSON. No markdown code blocks, no explanations, no prose.
- Your response MUST start with `{{` and end with `}}`
- Do NOT wrap in ```json``` code blocks

Required JSON format:
{{
  "match_score": 0.85,
  "reasoning": "Both categories relate to network connectivity testing"
}}"#,
                            suggested, actual
                        );

                        match llm_service.generate_analysis(&prompt).await {
                            Ok(llm_response) => {
                                let call_duration = call_start.elapsed();
                                debug!(
                                    "[MAGICIAN-V2-CAT] LLM call '{}' → '{}' completed in {:.2}s",
                                    suggested,
                                    actual,
                                    call_duration.as_secs_f64()
                                );
                                // Parse JSON response
                                if let Ok(parsed) =
                                    serde_json::from_str::<serde_json::Value>(&llm_response.content)
                                {
                                    let llm_score = parsed["match_score"]
                                        .as_f64()
                                        .unwrap_or(semantic_score as f64)
                                        as f32;
                                    let reasoning = parsed["reasoning"]
                                        .as_str()
                                        .unwrap_or("No reasoning provided")
                                        .to_string();
                                    (suggested, actual, semantic_score, llm_score, reasoning)
                                } else {
                                    warn!(
                                        "[MAGICIAN-V2-CAT] Failed to parse LLM response for '{}' \
                                         → '{}', using semantic score",
                                        suggested, actual
                                    );
                                    (
                                        suggested,
                                        actual,
                                        semantic_score,
                                        semantic_score,
                                        "LLM validation parse error".to_string(),
                                    )
                                }
                            },
                            Err(e) => {
                                let call_duration = call_start.elapsed();
                                warn!(
                                    "[MAGICIAN-V2-CAT] LLM validation failed for '{}' → '{}' \
                                     after {:.2}s: {}",
                                    suggested,
                                    actual,
                                    call_duration.as_secs_f64(),
                                    e
                                );
                                (
                                    suggested,
                                    actual,
                                    semantic_score,
                                    semantic_score,
                                    "LLM validation skipped due to error".to_string(),
                                )
                            },
                        }
                    } else {
                        (
                            suggested,
                            actual,
                            semantic_score,
                            semantic_score,
                            "LLM validation disabled".to_string(),
                        )
                    }
                }
            })
            .collect();

        // Execute all validations concurrently
        let validation_results = join_all(validation_futures).await;

        let llm_duration = llm_start.elapsed();
        info!(
            "[MAGICIAN-V2-CAT] ✅ All {} LLM validation calls completed in {:.2}s",
            validation_results.len(),
            llm_duration.as_secs_f64()
        );

        // Broadcast LLM validation complete
        if let (Some(broadcaster), Some(tid), Some(cid)) =
            (&self.event_broadcaster, execution_id, correlation_id)
        {
            broadcaster.exploration_progress(
                tid,
                cid,
                validation_results.len(),
                0,
                0.0,
                format!(
                    "LLM validation complete ({:.1}s)",
                    llm_duration.as_secs_f64()
                ),
                90.0,
            );
        }

        // Stage 3: Calculate combined scores and filter
        let mut all_matches = Vec::new();
        for (suggested, actual_category, semantic_score, llm_score, reasoning) in validation_results
        {
            // Calculate combined score
            let combined_score = (semantic_score * self.config.semantic_weight)
                + (llm_score * self.config.llm_weight);

            debug!(
                "[MAGICIAN-V2-CAT]   {} → {} | sem={:.3}, llm={:.3}, combined={:.3}",
                suggested, actual_category, semantic_score, llm_score, combined_score
            );

            // Only include if above threshold
            if combined_score >= self.config.min_match_threshold {
                all_matches.push(CategoryMatch {
                    suggested_category: suggested,
                    matched_category: actual_category,
                    semantic_score,
                    llm_score,
                    combined_score,
                    reasoning,
                });
            }
        }

        // Sort by combined score (descending)
        all_matches.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        info!(
            "[MAGICIAN-V2-CAT] ✅ Category matching complete: {} total matches above threshold \
             {:.2}",
            all_matches.len(),
            self.config.min_match_threshold
        );

        // Log top matches - increased to 20 for better visibility
        for (i, m) in all_matches.iter().take(20).enumerate() {
            info!(
                "[MAGICIAN-V2-CAT]   {}. {} → {} (combined={:.3}, sem={:.3}, llm={:.3})",
                i + 1,
                m.suggested_category,
                m.matched_category,
                m.combined_score,
                m.semantic_score,
                m.llm_score
            );
        }

        Ok(all_matches)
    }

    /// Validate category match using LLM
    ///
    /// Asks LLM: "Are these two categories semantically related?"
    ///
    /// # Returns
    /// * (score, reasoning) tuple
    #[allow(dead_code)]
    async fn validate_with_llm(&self, suggested: &str, actual: &str) -> Result<(f32, String)> {
        // Build prompt for LLM with STRICT JSON requirements
        let prompt = format!(
            r#"Evaluate if these two tool categories are semantically related.

**Suggested Category**: "{}"
**Actual Category**: "{}"

Consider:
- Do they describe similar operations or domains?
- Would tools in one category be useful for tasks in the other?
- Are they synonyms or closely related concepts?

Examples:
- 'connectivity-testing' → 'http-requests' (STRONG match, 0.9)
- 'health-checks' → 'network-testing' (STRONG match, 0.85)
- 'api-calls' → 'http-requests' (STRONG match, 0.95)
- 'database-queries' → 'file-operations' (WEAK match, 0.1)

**CRITICAL INSTRUCTIONS:**
- Return ONLY valid JSON. No markdown code blocks, no explanations, no prose.
- Your response MUST start with `{{` and end with `}}`
- Do NOT wrap in ```json``` code blocks
- Invalid JSON will cause system failure

Required JSON format:
{{
  "match_score": 0.85,
  "reasoning": "Brief explanation"
}}

Respond now with ONLY the JSON object:
"#,
            suggested, actual
        );

        // Call LLM
        let llm_response = self.llm_service.generate_analysis(&prompt).await?;

        // Extract JSON from response (handle markdown code blocks if present)
        let json_str = if let Some(start) = llm_response.content.find('{') {
            if let Some(end) = llm_response.content.rfind('}') {
                &llm_response.content[start..=end]
            } else {
                &llm_response.content
            }
        } else {
            &llm_response.content
        };

        // Parse JSON response
        #[derive(serde::Deserialize)]
        struct LLMCategoryMatch {
            match_score: f32,
            reasoning: String,
        }

        let parsed: LLMCategoryMatch = serde_json::from_str(json_str).map_err(|e| {
            warn!(
                "[MAGICIAN-V2-CAT] Failed to parse LLM category validation response as JSON: {}",
                e
            );
            debug!(
                "[MAGICIAN-V2-CAT] LLM Response (first 500 chars): {}",
                &llm_response.content.chars().take(500).collect::<String>()
            );
            anyhow::anyhow!(
                "Failed to parse LLM response: {}. Response was not valid JSON.",
                e
            )
        })?;

        Ok((parsed.match_score, parsed.reasoning))
    }

    /// Get all unique matched categories above threshold
    ///
    /// Useful for getting the list of actual categories to search for tools
    pub fn get_matched_categories(&self, matches: &[CategoryMatch]) -> Vec<String> {
        let mut categories: Vec<String> =
            matches.iter().map(|m| m.matched_category.clone()).collect();

        // Remove duplicates
        categories.sort();
        categories.dedup();

        categories
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    // TODO: Add unit tests for CategoryFuzzyMatcher
    // - Test semantic-only matching (LLM disabled)
    // - Test full fuzzy matching
    // - Test threshold filtering
    // - Test score weighting
}
