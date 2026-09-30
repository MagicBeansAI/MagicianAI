//! Tier 2: Semantic Matching
//!
//! This module implements Tier 2 similarity scoring over the indexed search
//! service. The backing implementation may be lexical-only or semantic.
//!
//! ## Performance
//! - <200ms typical execution time
//! - Uses indexed tool metadata when available
//! - Falls back to direct description similarity if the index is unavailable
//!
//! ## Scoring
//! - Base weight: 30% of total score
//! - Category bonus: +0.20 for category matches
//! - Similarity score: 0.0-1.0 range
//! - Minimum threshold: 0.3 (configurable)

use std::sync::Arc;

use tracing::{debug, info, warn};

use super::{config::ToolMatcherConfig, types::ToolCandidate};
use runtime_core::SemanticSearch;

/// Semantic matcher for Tier 2
pub struct SemanticMatcher {
    /// Optional similarity search service for indexed matching
    semantic_search: Option<Arc<dyn SemanticSearch>>,

    /// Configuration for matching behavior
    config: ToolMatcherConfig,
}

impl SemanticMatcher {
    /// Create a new semantic matcher
    ///
    /// # Arguments
    /// * `semantic_search` - Optional semantic search service
    /// * `config` - Configuration for matching behavior
    pub fn new(
        semantic_search: Option<Arc<dyn SemanticSearch>>,
        config: ToolMatcherConfig,
    ) -> Self {
        if semantic_search.is_some() {
            info!("[MAGICIAN-V2-SEMANTIC] SemanticMatcher initialized with indexed search support");
        } else {
            info!(
                "[MAGICIAN-V2-SEMANTIC] SemanticMatcher initialized with fallback mode (local \
                 text similarity only)"
            );
        }

        Self {
            semantic_search,
            config,
        }
    }

    /// Score candidates using semantic similarity
    ///
    /// Updates each candidate's semantic_score based on:
    /// - Indexed similarity scores (if available)
    /// - Fallback to text-based similarity
    /// - Category bonus
    /// - Hierarchical context (concatenates parent task for better
    ///   disambiguation)
    ///
    /// # Arguments
    /// * `task` - Task description to match against
    /// * `candidates` - Mutable candidates to score
    /// * `parent_task` - Optional parent task for context
    pub async fn score_candidates(
        &self,
        task: &str,
        candidates: &mut [ToolCandidate],
        parent_task: Option<&str>,
    ) {
        let start_time = std::time::Instant::now();

        // Build context-enhanced task string for semantic matching
        let search_query = if let Some(parent) = parent_task {
            format!("Parent: {} | Current: {}", parent, task)
        } else {
            task.to_string()
        };

        if parent_task.is_some() {
            debug!(
                "[MAGICIAN-V2-MATCHER] Tier 2 - Semantic Matcher: Scoring {} candidates for task \
                 '{}' with parent context",
                candidates.len(),
                search_query
            );
        } else {
            debug!(
                "[MAGICIAN-V2-MATCHER] Tier 2 - Semantic Matcher: Scoring {} candidates for task \
                 '{}'",
                candidates.len(),
                task
            );
        }

        if let Some(semantic_service) = &self.semantic_search {
            // Use indexed search with context-enhanced query
            self.score_with_ranked_search(&search_query, candidates, semantic_service.as_ref())
                .await;
        } else {
            // Fallback to text-based similarity with context-enhanced query
            self.score_with_text_similarity(&search_query, candidates);
        }

        let elapsed = start_time.elapsed();
        let avg_score: f32 = candidates.iter().map(|c| c.semantic_score).sum::<f32>()
            / candidates.len().max(1) as f32;

        info!(
            "[MAGICIAN-V2-SEMANTIC] Tier 2 complete: Scored {} candidates (avg={:.3}) in {:?}",
            candidates.len(),
            avg_score,
            elapsed
        );
    }

    /// Score using indexed similarity search
    async fn score_with_ranked_search(
        &self,
        task: &str,
        candidates: &mut [ToolCandidate],
        semantic_service: &dyn SemanticSearch,
    ) {
        // Use search_all_tools_with_scores() for V2 Tool Matcher
        // This returns ALL tools with their actual similarity scores (no threshold
        // filtering), which is required for the 4-tier selection strategy.
        match semantic_service.search_all_tools_with_scores(task).await {
            Ok(results) => {
                // Create a mapping of tool name -> similarity score
                let similarity_map: std::collections::HashMap<String, f64> = results
                    .into_iter()
                    .map(|result| (result.tool_name, result.similarity_score))
                    .collect();

                // Update candidate scores based on similarity
                for candidate in candidates.iter_mut() {
                    let similarity = similarity_map
                        .get(&candidate.tool_name)
                        .copied()
                        .unwrap_or(0.0) as f32;

                    // Apply similarity score (already 0.0-1.0 range)
                    let mut score = similarity;

                    // Category bonus
                    if candidate.category_matched {
                        let category_bonus = self.config.category_bonuses.semantic_category_bonus;
                        score += category_bonus;
                        score = score.min(1.0); // Cap at 1.0
                    }

                    candidate.semantic_score = score;

                    if score > 0.3 {
                        debug!(
                            "[MAGICIAN-V2-MATCHER]   {} → semantic_score={:.3} (similarity={:.3}, \
                             category_bonus={})",
                            candidate.tool_name, score, similarity, candidate.category_matched
                        );
                    }
                }
            },
            Err(e) => {
                warn!(
                    "[MAGICIAN-V2-SEMANTIC] Semantic search failed, falling back to text \
                     similarity: {}",
                    e
                );
                self.score_with_text_similarity(task, candidates);
            },
        }
    }

    /// Fallback: Score using text-based similarity
    fn score_with_text_similarity(&self, task: &str, candidates: &mut [ToolCandidate]) {
        let task_lower = task.to_lowercase();

        for candidate in candidates.iter_mut() {
            let tool_name_lower = candidate.tool_name.to_lowercase();
            let description_lower = candidate.tool_info.description.to_lowercase();

            // Calculate similarity based on common words
            let name_similarity = Self::calculate_text_similarity(&task_lower, &tool_name_lower);
            let desc_similarity = Self::calculate_text_similarity(&task_lower, &description_lower);

            // Weight: 60% from description, 40% from name
            let mut score = (desc_similarity * 0.6) + (name_similarity * 0.4);

            // Category bonus
            if candidate.category_matched {
                let category_bonus = self.config.category_bonuses.semantic_category_bonus;
                score += category_bonus;
                score = score.min(1.0);
            }

            candidate.semantic_score = score;

            if score > 0.3 {
                debug!(
                    "[MAGICIAN-V2-MATCHER]   {} → semantic_score={:.3} (text_similarity, \
                     category_bonus={})",
                    candidate.tool_name, score, candidate.category_matched
                );
            }
        }
    }

    /// Calculate text similarity using Jaccard similarity (intersection /
    /// union)
    fn calculate_text_similarity(text1: &str, text2: &str) -> f32 {
        // Extract words (alphanumeric tokens)
        let words1: std::collections::HashSet<&str> = text1
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .collect();

        let words2: std::collections::HashSet<&str> = text2
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .collect();

        if words1.is_empty() || words2.is_empty() {
            return 0.0;
        }

        // Jaccard similarity: |intersection| / |union|
        let intersection = words1.intersection(&words2).count();
        let union = words1.union(&words2).count();

        if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::{magician_v2::tool_matcher::config::ToolMatcherConfig, ToolInfo};

    fn create_test_tool(
        name: &str,
        description: &str,
        category: &str,
        category_matched: bool,
    ) -> ToolCandidate {
        let tool_info = ToolInfo {
            name: name.to_string(),
            description: description.to_string(),
            category: category.to_string(),
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            categories: vec![category.to_string()], // Populate with primary category
            composition_category: None,
            providing_agent_id: None,
        };
        ToolCandidate::new(tool_info, category_matched)
    }

    #[tokio::test]
    async fn test_text_similarity_scoring() {
        let config = ToolMatcherConfig::default();
        let matcher = SemanticMatcher::new(None, config); // No semantic service

        let mut candidates = vec![
            create_test_tool(
                "network_ping",
                "Ping network hosts to check connectivity",
                "network",
                false,
            ),
            create_test_tool("file_reader", "Read file contents from disk", "file", false),
        ];

        matcher
            .score_candidates("ping google.com to check network", &mut candidates, None)
            .await;

        // First tool should score higher due to description similarity
        assert!(candidates[0].semantic_score > candidates[1].semantic_score);
        assert!(candidates[0].semantic_score > 0.0);
    }

    #[tokio::test]
    async fn test_category_bonus() {
        let mut config = ToolMatcherConfig::default();
        config.category_bonuses.semantic_category_bonus = 0.20;
        let matcher = SemanticMatcher::new(None, config);

        let mut candidates = vec![
            create_test_tool(
                "network_tool",
                "Network operations",
                "network",
                true, // category matched
            ),
            create_test_tool(
                "network_tool2",
                "Network operations",
                "network",
                false, // no category match
            ),
        ];

        matcher
            .score_candidates("network test", &mut candidates, None)
            .await;

        // First tool should have category bonus
        assert!(candidates[0].semantic_score > candidates[1].semantic_score);
        // Category bonus should be approximately 0.20
        let bonus_diff = candidates[0].semantic_score - candidates[1].semantic_score;
        assert!((bonus_diff - 0.20).abs() < 0.05); // Allow some variance from
                                                   // text similarity
    }

    #[test]
    fn test_jaccard_similarity() {
        let sim1 =
            SemanticMatcher::calculate_text_similarity("read file content", "read file from disk");
        // Common words: "read", "file" (2)
        // Union: "read", "file", "content", "from", "disk" (5)
        // Similarity: 2/5 = 0.4
        assert!((sim1 - 0.4).abs() < 0.01);

        let sim2 =
            SemanticMatcher::calculate_text_similarity("completely different", "nothing in common");
        // No common words
        assert!(sim2 < 0.1);
    }

    #[test]
    fn test_empty_text_similarity() {
        let sim = SemanticMatcher::calculate_text_similarity("", "some text");
        assert_eq!(sim, 0.0);

        let sim2 = SemanticMatcher::calculate_text_similarity("some text", "");
        assert_eq!(sim2, 0.0);
    }
}
