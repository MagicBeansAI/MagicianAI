//! Tier 0: Category Pre-Filter
//!
//! This module implements the first filtering stage in the V2 tool matching
//! pipeline. It reduces the tool set from potentially thousands to 50-200 tools
//! by filtering based on categories suggested by the TaskDecomposer.
//!
//! ## Security Architecture
//! - Uses ToolCatalog trait (security-filtered)
//! - Security filtering happens FIRST, category filtering SECOND
//! - Never accesses RegistryService directly
//!
//! ## Performance
//! - Fast category-based filtering
//! - <50ms typical execution time
//! - Fail-fast error handling (no silent fallbacks)

use std::sync::Arc;

use tracing::{debug, info, warn};

use super::types::{ToolCandidate, ToolMatchError};
use runtime_core::{ExecutionContext, ToolCatalog};

/// Category pre-filter for Tier 0
pub struct CategoryFilter {
    /// Security-filtered tool catalog service
    tool_catalog: Arc<dyn ToolCatalog>,
}

impl CategoryFilter {
    /// Create a new category filter
    pub fn new(tool_catalog: Arc<dyn ToolCatalog>) -> Self {
        Self { tool_catalog }
    }

    /// Apply category pre-filtering
    ///
    /// Reduces tool set from potentially 8000+ to 50-200 tools by filtering
    /// on categories suggested by TaskDecomposer.
    ///
    /// # Arguments
    /// * `categories` - Suggested categories from SubTask
    /// * `context` - Execution context for security filtering
    ///
    /// # Returns
    /// * Vector of ToolCandidate with category_matched flag set
    ///
    /// # Errors
    /// * `EmptyCategories` - No categories provided
    /// * `NoMatchingCategories` - No tools match requested categories
    /// * `InsufficientTools` - Too few tools after filtering
    pub async fn filter_by_categories(
        &self,
        categories: &[String],
        context: &ExecutionContext,
        min_tools_required: usize,
    ) -> Result<Vec<ToolCandidate>, ToolMatchError> {
        let start_time = std::time::Instant::now();

        debug!(
            "[MAGICIAN-V2-CAT] Tier 0 - Category Pre-Filter: categories={:?}, min_required={}",
            categories, min_tools_required
        );

        // Fail-fast: Empty categories
        if categories.is_empty() {
            warn!("[MAGICIAN-V2-CAT] Tier 0 failed: Empty categories provided");
            return Err(ToolMatchError::EmptyCategories);
        }

        // Get security-filtered tools by categories
        let filtered_tools = self
            .tool_catalog
            .filtered_tools_by_categories(categories, context)
            .await
            .map_err(|e| ToolMatchError::SecurityFilteringFailed(e.to_string()))?;

        // Check if we got any matches
        if filtered_tools.is_empty() {
            warn!(
                "[MAGICIAN-V2-CAT] Tier 0 failed: No tools match categories {:?}",
                categories
            );
            return Err(ToolMatchError::NoMatchingCategories {
                categories: categories.to_vec(),
            });
        }

        // Check if we have enough tools
        if filtered_tools.len() < min_tools_required {
            warn!(
                "[MAGICIAN-V2-CAT] Tier 0 failed: Only {} tools match (need at least {})",
                filtered_tools.len(),
                min_tools_required
            );
            return Err(ToolMatchError::InsufficientTools {
                count: filtered_tools.len(),
                required: min_tools_required,
            });
        }

        // Convert to ToolCandidate with category_matched flag
        let candidates: Vec<ToolCandidate> = filtered_tools
            .into_iter()
            .map(|tool_info| {
                let category_matched = categories
                    .iter()
                    .any(|cat| tool_info.category.eq_ignore_ascii_case(cat));

                ToolCandidate::new(tool_info, category_matched)
            })
            .collect();

        let elapsed = start_time.elapsed();
        info!(
            "[MAGICIAN-V2-CAT] Tier 0 complete: {} tools matched categories {:?} in {:?}",
            candidates.len(),
            categories,
            elapsed
        );

        Ok(candidates)
    }

    /// Get category distribution for telemetry
    ///
    /// Returns mapping of category → tool count for the given context.
    /// Useful for understanding category availability and planning.
    pub async fn get_category_distribution(
        &self,
        context: &ExecutionContext,
    ) -> Result<std::collections::HashMap<String, usize>, ToolMatchError> {
        self.tool_catalog
            .category_tool_counts(context)
            .await
            .map_err(|e| ToolMatchError::ToolDiscoveryError(e.to_string()))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use runtime_core::{
        ExecutionContext, MultipleToolMatchResult, ToolDiscovery, ToolInfo, ToolMatchResult,
    };

    // Mock ToolDiscovery for testing
    struct MockToolDiscovery {
        tools: Vec<ToolInfo>,
    }

    #[async_trait::async_trait]
    impl ToolDiscovery for MockToolDiscovery {
        async fn find_best_match_with_context(
            &self,
            _task: &str,
            _context: &ExecutionContext,
        ) -> ToolMatchResult {
            ToolMatchResult::default()
        }

        async fn find_multiple_matches_with_context(
            &self,
            _task: &str,
            _context: &ExecutionContext,
        ) -> MultipleToolMatchResult {
            MultipleToolMatchResult::default()
        }

        async fn is_tool_available(&self, _tool: &str, _context: &ExecutionContext) -> bool {
            true
        }

        async fn get_tool_metadata(
            &self,
            _tool: &str,
            _context: &ExecutionContext,
        ) -> Option<HashMap<String, serde_json::Value>> {
            None
        }

        async fn get_available_tools(&self, _context: &ExecutionContext) -> Vec<String> {
            self.tools.iter().map(|t| t.name.clone()).collect()
        }

        async fn get_filtered_tools_by_categories(
            &self,
            categories: &[String],
            _context: &ExecutionContext,
        ) -> anyhow::Result<Vec<ToolInfo>> {
            let filtered: Vec<ToolInfo> = self
                .tools
                .iter()
                .filter(|tool| {
                    categories
                        .iter()
                        .any(|cat| tool.category.eq_ignore_ascii_case(cat))
                })
                .cloned()
                .collect();
            Ok(filtered)
        }

        async fn get_all_filtered_tools(
            &self,
            _context: &ExecutionContext,
        ) -> anyhow::Result<Vec<ToolInfo>> {
            Ok(self.tools.clone())
        }

        async fn get_category_tool_counts(
            &self,
            _context: &ExecutionContext,
        ) -> anyhow::Result<HashMap<String, usize>> {
            let mut counts = HashMap::new();
            for tool in &self.tools {
                *counts.entry(tool.category.clone()).or_insert(0) += 1;
            }
            Ok(counts)
        }
    }

    #[async_trait::async_trait]
    impl ToolCatalog for MockToolDiscovery {
        async fn list_tool_names(&self, _context: &ExecutionContext) -> Vec<String> {
            self.tools.iter().map(|t| t.name.clone()).collect()
        }

        async fn available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
            let mut categories: Vec<String> = self
                .tools
                .iter()
                .map(|tool| tool.category.clone())
                .collect();
            categories.sort();
            categories.dedup();
            categories
        }

        async fn get_tool_metadata(
            &self,
            tool_name: &str,
            _context: &ExecutionContext,
        ) -> Option<HashMap<String, serde_json::Value>> {
            self.tools
                .iter()
                .find(|tool| tool.name == tool_name)
                .map(|tool| {
                    let mut map = HashMap::new();
                    map.insert("category".to_string(), serde_json::json!(tool.category));
                    map
                })
        }

        async fn filtered_tools_by_categories(
            &self,
            categories: &[String],
            context: &ExecutionContext,
        ) -> anyhow::Result<Vec<ToolInfo>> {
            self.get_filtered_tools_by_categories(categories, context)
                .await
        }

        async fn all_tools(&self, _context: &ExecutionContext) -> anyhow::Result<Vec<ToolInfo>> {
            Ok(self.tools.clone())
        }

        async fn category_tool_counts(
            &self,
            _context: &ExecutionContext,
        ) -> anyhow::Result<HashMap<String, usize>> {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for tool in &self.tools {
                *counts.entry(tool.category.clone()).or_insert(0) += 1;
            }
            Ok(counts)
        }

    }

    fn create_test_tool(name: &str, category: &str) -> ToolInfo {
        ToolInfo {
            name: name.to_string(),
            description: format!("Test tool: {}", name),
            category: category.to_string(),
            categories: vec![category.to_string()], // Already populated correctly - support fuzzy category matching
            parameters: vec![],
            enhanced_description: None,
            keywords: vec![],
            use_cases: vec![],
            composition_category: None,
            providing_agent_id: None,
        }
    }

    #[tokio::test]
    async fn test_category_filter_success() {
        let mock_discovery = MockToolDiscovery {
            tools: vec![
                create_test_tool("network_tool", "network"),
                create_test_tool("file_tool", "file"),
                create_test_tool("database_tool", "database"),
            ],
        };

        let filter = CategoryFilter::new(Arc::new(mock_discovery));
        let context = ExecutionContext::default();

        let result = filter
            .filter_by_categories(&vec!["network".to_string()], &context, 1)
            .await;

        assert!(result.is_ok());
        let candidates = result.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tool_name, "network_tool");
        assert!(candidates[0].category_matched);
    }

    #[tokio::test]
    async fn test_empty_categories_error() {
        let mock_discovery = MockToolDiscovery { tools: vec![] };
        let filter = CategoryFilter::new(Arc::new(mock_discovery));
        let context = ExecutionContext::default();

        let result = filter.filter_by_categories(&vec![], &context, 1).await;

        assert!(matches!(result, Err(ToolMatchError::EmptyCategories)));
    }

    #[tokio::test]
    async fn test_no_matching_categories_error() {
        let mock_discovery = MockToolDiscovery {
            tools: vec![create_test_tool("network_tool", "network")],
        };

        let filter = CategoryFilter::new(Arc::new(mock_discovery));
        let context = ExecutionContext::default();

        let result = filter
            .filter_by_categories(&vec!["nonexistent".to_string()], &context, 1)
            .await;

        assert!(matches!(
            result,
            Err(ToolMatchError::NoMatchingCategories { .. })
        ));
    }

    #[tokio::test]
    async fn test_insufficient_tools_error() {
        let mock_discovery = MockToolDiscovery {
            tools: vec![create_test_tool("network_tool", "network")],
        };

        let filter = CategoryFilter::new(Arc::new(mock_discovery));
        let context = ExecutionContext::default();

        // Require 5 tools, but only 1 matches
        let result = filter
            .filter_by_categories(&vec!["network".to_string()], &context, 5)
            .await;

        assert!(matches!(
            result,
            Err(ToolMatchError::InsufficientTools { .. })
        ));
    }

    #[tokio::test]
    async fn test_category_distribution() {
        let mock_discovery = MockToolDiscovery {
            tools: vec![
                create_test_tool("net1", "network"),
                create_test_tool("net2", "network"),
                create_test_tool("file1", "file"),
            ],
        };

        let filter = CategoryFilter::new(Arc::new(mock_discovery));
        let context = ExecutionContext::default();

        let distribution = filter.get_category_distribution(&context).await.unwrap();

        assert_eq!(distribution.get("network"), Some(&2));
        assert_eq!(distribution.get("file"), Some(&1));
    }
}
