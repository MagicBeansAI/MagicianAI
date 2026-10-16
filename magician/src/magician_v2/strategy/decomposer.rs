//! Intelligent Task Decomposition Service
//!
//! Replaces naive string-splitting decomposition with LLM-based intelligent
//! task breakdown. Handles recursive analysis with proper context propagation
//! and cycle detection to prevent infinite loops.

use std::{collections::HashSet, sync::Arc};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::types::ResourceBudget;
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompt_identity::render_prompt_identity_from_metadata,
    prompts::{
        constants::{names, versions},
        manager::PromptManager,
    },
    query_analysis::{
        operation_llm_router::QueryAnalysisLLM, UnifiedQueryAnalysis, UnifiedQueryAnalyzer,
    },
};
use runtime_core::{ExecutionContext, ToolCatalog};

// =============================================================================
// DECOMPOSITION TYPES
// =============================================================================

/// Configuration limits for decomposition to prevent runaway recursion
#[derive(Debug, Clone)]
pub struct DecompositionLimits {
    /// Maximum recursion depth
    pub max_depth: u32,
    /// Maximum subtasks per decomposition
    pub max_subtasks_per_step: usize,
    /// Maximum total subtasks across all levels
    pub max_total_subtasks: usize,
    /// Similarity threshold for cycle detection (0.0-1.0)
    pub similarity_threshold: f32,
}

impl Default for DecompositionLimits {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_subtasks_per_step: 5,
            max_total_subtasks: 50,
            similarity_threshold: 0.85,
        }
    }
}

/// Context passed down during recursive decomposition
#[derive(Debug, Clone)]
pub struct DecompositionContext {
    /// Original root task
    pub root_task: String,
    /// Current decomposition depth
    pub depth: u32,
    /// Task hierarchy from root to current
    pub task_path: Vec<String>,
    /// Accumulated context from parent tasks
    pub parent_context: String,
    /// Resource budget inherited from parent
    pub resource_budget: ResourceBudget,
    /// Execution context for principal/workspace scoping
    pub execution_context: ExecutionContext,
    /// Execution ID for event emission
    pub execution_id: Option<String>,
    /// Correlation ID for request tracing
    pub correlation_id: Option<String>,
}

impl DecompositionContext {
    /// Create initial context for root task
    pub fn new_root(
        task: String,
        resource_budget: ResourceBudget,
        execution_context: ExecutionContext,
    ) -> Self {
        Self {
            root_task: task.clone(),
            depth: 0,
            task_path: vec![task],
            parent_context: String::new(),
            resource_budget,
            execution_context,
            execution_id: None,
            correlation_id: None,
        }
    }

    /// Create initial context with execution and correlation IDs
    pub fn new_root_with_ids(
        task: String,
        resource_budget: ResourceBudget,
        execution_context: ExecutionContext,
        execution_id: Option<String>,
        correlation_id: Option<String>,
    ) -> Self {
        Self {
            root_task: task.clone(),
            depth: 0,
            task_path: vec![task],
            parent_context: String::new(),
            resource_budget,
            execution_context,
            execution_id,
            correlation_id,
        }
    }

    /// Create child context for subtask
    pub fn create_child(&self, subtask: String, allocated_budget: ResourceBudget) -> Self {
        let mut child_path = self.task_path.clone();
        child_path.push(subtask.clone());

        // Build accumulated context
        let mut context_parts = vec![];
        if !self.parent_context.is_empty() {
            context_parts.push(self.parent_context.clone());
        }
        if !self.task_path.is_empty() {
            context_parts.push(format!("Parent task: {}", self.task_path.last().unwrap()));
        }
        let accumulated_context = context_parts.join(" | ");

        Self {
            root_task: self.root_task.clone(),
            depth: self.depth + 1,
            task_path: child_path,
            parent_context: accumulated_context,
            resource_budget: allocated_budget,
            execution_context: self.execution_context.clone(),
            execution_id: self.execution_id.clone(),
            correlation_id: self.correlation_id.clone(),
        }
    }

    /// Check if we've seen a similar task in our ancestry (cycle detection)
    /// Excludes the last element (current task) to avoid comparing task to
    /// itself
    pub fn has_similar_ancestor(&self, task: &str, threshold: f32) -> bool {
        // Skip the last element if it exists (current task comparing to itself)
        let ancestors = if self.task_path.is_empty() {
            &self.task_path[..]
        } else {
            &self.task_path[..self.task_path.len() - 1]
        };

        for ancestor in ancestors {
            if calculate_text_similarity(task, ancestor) > threshold {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Cycle detected: '{}' similar to ancestor '{}'",
                    task, ancestor
                );
                return true;
            }
        }
        false
    }
}

/// Strategy for how to decompose a task
#[derive(Debug, Clone, Copy)]
pub enum DecompositionStrategy {
    /// Simple sequential breakdown (A then B then C)
    Sequential,
    /// Parallel breakdown (A and B and C simultaneously)
    Parallel,
    /// Hierarchical breakdown (A contains B which contains C)
    Hierarchical,
    /// Single task - no decomposition needed
    Single,
}

/// Result of analyzing whether a task needs decomposition
#[derive(Debug, Clone)]
pub struct DecompositionAnalysis {
    /// Whether this task should be decomposed further
    pub needs_decomposition: bool,
    /// Suggested decomposition strategy
    pub strategy: DecompositionStrategy,
    /// Confidence in the decomposition decision (0.0-1.0)
    pub confidence: f32,
    /// Reasoning for the decision
    pub reasoning: String,
    /// Estimated number of subtasks if decomposed
    pub estimated_subtasks: usize,
}

/// A single subtask resulting from decomposition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    /// The subtask description
    pub description: String,
    /// Dependency relationships with other subtasks
    pub dependencies: Vec<usize>, // Indices of other subtasks this depends on
    /// Estimated complexity (0.0-1.0)
    pub complexity: f32,
    /// Priority (higher = more important)
    pub priority: f32,
    /// Suggested categories for tool filtering (reduces 8000 tools → 15-50)
    /// Example: ["network", "diagnostics"] narrows tool search space
    pub suggested_categories: Vec<String>,
    /// Analysis results if already analyzed
    pub analysis: Option<UnifiedQueryAnalysis>,
}

// =============================================================================
// TASK DECOMPOSER SERVICE
// =============================================================================

/// Intelligent task decomposition service using LLM analysis
pub struct TaskDecomposer {
    /// LLM service for generating decompositions
    llm_service: Arc<dyn QueryAnalysisLLM>,
    /// Query analyzer for understanding subtasks
    query_analyzer: Arc<UnifiedQueryAnalyzer>,
    /// Prompt manager for versioned prompts
    prompt_manager: Arc<PromptManager>,
    /// Tool catalog for category filtering
    tool_catalog: Arc<dyn ToolCatalog>,
    /// Limits to prevent runaway decomposition
    limits: DecompositionLimits,
    /// Cache of similar tasks we've seen
    #[allow(dead_code)]
    task_cache: std::sync::RwLock<HashSet<String>>,
    /// Event broadcaster for real-time updates
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl TaskDecomposer {
    /// Create a new task decomposer
    pub fn new(
        llm_service: Arc<dyn QueryAnalysisLLM>,
        query_analyzer: Arc<UnifiedQueryAnalyzer>,
        prompt_manager: Arc<PromptManager>,
        tool_catalog: Arc<dyn ToolCatalog>,
        limits: Option<DecompositionLimits>,
    ) -> Self {
        Self {
            llm_service,
            query_analyzer,
            prompt_manager,
            tool_catalog,
            limits: limits.unwrap_or_default(),
            task_cache: std::sync::RwLock::new(HashSet::new()),
            event_broadcaster: None,
        }
    }

    /// Attach event broadcaster for real-time updates
    pub fn with_event_broadcaster(
        mut self,
        broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
    ) -> Self {
        self.event_broadcaster = Some(broadcaster);
        self
    }

    /// Analyze if a task needs decomposition
    pub async fn analyze_decomposition_need(
        &self,
        task: &str,
        context: &DecompositionContext,
    ) -> Result<DecompositionAnalysis> {
        // Check depth limits
        if context.depth >= self.limits.max_depth {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Decomposition stopped: max depth {} reached",
                self.limits.max_depth
            );
            return Ok(DecompositionAnalysis {
                needs_decomposition: false,
                strategy: DecompositionStrategy::Single,
                confidence: 1.0,
                reasoning: "Maximum recursion depth reached".to_string(),
                estimated_subtasks: 0,
            });
        }

        // Check for cycles
        if context.has_similar_ancestor(task, self.limits.similarity_threshold) {
            return Ok(DecompositionAnalysis {
                needs_decomposition: false,
                strategy: DecompositionStrategy::Single,
                confidence: 1.0,
                reasoning: "Similar task found in ancestry - preventing cycle".to_string(),
                estimated_subtasks: 0,
            });
        }

        // Analyze the task with LLM using the execution context
        let analysis = self
            .query_analyzer
            .analyze_query(task, &context.execution_context, None, None, None)
            .await?;

        // Use complexity and dependencies to determine if decomposition is needed
        let needs_decomposition = analysis.complexity.score > 0.3
            && (analysis.dependencies.is_multi_step
                || analysis.dependencies.workflow_steps.len() > 1);

        let strategy = if analysis.dependencies.workflow_steps.len() > 1 {
            if analysis
                .dependencies
                .dependencies
                .iter()
                .any(|dep| dep.contains("parallel"))
            {
                DecompositionStrategy::Parallel
            } else {
                DecompositionStrategy::Sequential
            }
        } else {
            DecompositionStrategy::Single
        };

        Ok(DecompositionAnalysis {
            needs_decomposition,
            strategy,
            confidence: analysis.complexity.score,
            reasoning: analysis.complexity.reasoning.clone(),
            estimated_subtasks: analysis.dependencies.workflow_steps.len().max(1),
        })
    }

    /// Decompose a task into intelligent subtasks
    pub async fn decompose_task(
        &self,
        task: &str,
        context: &DecompositionContext,
    ) -> Result<Vec<SubTask>> {
        info!(
            "[MAGICIAN-V2-STRATEGY] Decomposing task at depth {}: '{}'",
            context.depth, task
        );

        // First analyze if decomposition is needed
        let decomp_analysis = self.analyze_decomposition_need(task, context).await?;

        if !decomp_analysis.needs_decomposition {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Task doesn't need decomposition: {}",
                decomp_analysis.reasoning
            );
            return Ok(vec![SubTask {
                description: task.to_string(),
                dependencies: vec![],
                complexity: 0.5,
                priority: 1.0,
                suggested_categories: vec![],
                analysis: None,
            }]);
        }

        // Generate subtasks using LLM
        let subtasks = self
            .generate_subtasks_with_llm(task, context, &decomp_analysis)
            .await?;

        // Analyze each subtask
        let mut analyzed_subtasks = Vec::new();
        for mut subtask in subtasks {
            // Create child context with allocated budget
            let child_budget = self.allocate_budget_for_child(&context.resource_budget);
            let _child_context = context.create_child(subtask.description.clone(), child_budget);

            // Analyze the subtask
            if subtask.analysis.is_none() {
                match self
                    .query_analyzer
                    .analyze_query(
                        &subtask.description,
                        &context.execution_context,
                        None,
                        None,
                        None,
                    )
                    .await
                {
                    Ok(analysis) => {
                        subtask.complexity = analysis.complexity.score;
                        subtask.analysis = Some(analysis);
                    },
                    Err(e) => {
                        warn!(
                            "[MAGICIAN-V2-STRATEGY] Failed to analyze subtask '{}': {}",
                            subtask.description, e
                        );
                        // Continue with default values
                    },
                }
            }

            analyzed_subtasks.push(subtask);

            // Check limits
            if analyzed_subtasks.len() >= self.limits.max_subtasks_per_step {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Reached max subtasks per step limit: {}",
                    self.limits.max_subtasks_per_step
                );
                break;
            }
        }

        info!(
            "[MAGICIAN-V2-STRATEGY] Decomposed '{}' into {} subtasks",
            task,
            analyzed_subtasks.len()
        );
        for (i, subtask) in analyzed_subtasks.iter().enumerate() {
            debug!(
                "[MAGICIAN-V2-STRATEGY]   {}: '{}' (complexity: {:.2})",
                i, subtask.description, subtask.complexity
            );
        }

        Ok(analyzed_subtasks)
    }

    /// Generate subtasks using LLM analysis with versioned prompts
    async fn generate_subtasks_with_llm(
        &self,
        task: &str,
        context: &DecompositionContext,
        decomp_analysis: &DecompositionAnalysis,
    ) -> Result<Vec<SubTask>> {
        // Get security-filtered categories for this user's execution context
        let available_categories = self
            .tool_catalog
            .available_categories(&context.execution_context)
            .await;

        // Build variables for prompt template
        let mut variables = std::collections::HashMap::new();

        // Required variables
        variables.insert("task".to_string(), task.to_string());
        variables.insert(
            "strategy".to_string(),
            format!("{:?}", decomp_analysis.strategy),
        );
        variables.insert(
            "estimated_subtasks".to_string(),
            decomp_analysis.estimated_subtasks.to_string(),
        );

        // Optional: Add parent context
        if !context.parent_context.is_empty() {
            variables.insert(
                "context_section".to_string(),
                format!("CONTEXT: {}\n", context.parent_context),
            );
        }

        // Optional: Add categories section if available
        if !available_categories.is_empty() {
            let mut cat_section = String::from("AVAILABLE TOOL CATEGORIES:\n");
            cat_section
                .push_str("(Suggest ONE category per subtask to narrow tool search space)\n");
            for category in &available_categories {
                cat_section.push_str(&format!("- {}\n", category));
            }
            cat_section.push('\n');

            variables.insert("categories_section".to_string(), cat_section);
            variables.insert(
                "category_requirement".to_string(),
                "- For EACH subtask, specify which CATEGORY has tools for it\n".to_string(),
            );

            // Use category-aware format examples
            variables.insert(
                "format_line_1".to_string(),
                "1. [SUBTASK NAME] | Category: [category] | Complexity: X.X | Dependencies: [list]"
                    .to_string(),
            );
            variables.insert(
                "format_line_2".to_string(),
                "2. [SUBTASK NAME] | Category: [category] | Complexity: X.X | Dependencies: [list]"
                    .to_string(),
            );
            variables.insert(
                "example_line_1".to_string(),
                "1. Resolve DNS for target | Category: network | Complexity: 0.3 | Dependencies: \
                 []"
                .to_string(),
            );
            variables.insert(
                "example_line_2".to_string(),
                "2. Test TCP connection | Category: network | Complexity: 0.4 | Dependencies: [1]"
                    .to_string(),
            );
            variables.insert(
                "example_line_3".to_string(),
                "3. Verify response status | Category: diagnostics | Complexity: 0.3 | \
                 Dependencies: [2]"
                    .to_string(),
            );
        }
        // If no categories, default format examples are used from JSON defaults

        // Get rendered prompt from versioned JSON storage
        let prompt = self
            .prompt_manager
            .get_rendered_prompt(
                names::TASK_DECOMPOSITION,
                versions::TASK_DECOMPOSITION,
                variables,
            )
            .await?;

        let mut system_variables = std::collections::HashMap::new();
        system_variables.insert(
            "identity_section".to_string(),
            render_prompt_identity_from_metadata(&context.execution_context.metadata, false),
        );
        let system_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                names::TASK_DECOMPOSITION_SYSTEM,
                versions::TASK_DECOMPOSITION_SYSTEM,
                system_variables,
            )
            .await?;

        // Emit LLM analysis started event
        if let (Some(ref tid), Some(ref cid), Some(ref broadcaster)) = (
            &context.execution_id,
            &context.correlation_id,
            &self.event_broadcaster,
        ) {
            broadcaster.llm_analysis_started(
                tid,
                cid,
                self.llm_service.provider_name(),
                "plan_generation".to_string(),
                prompt.len(),
            );
        }

        let start = std::time::Instant::now();
        let llm_response = match self
            .llm_service
            .generate_analysis_with_system_scoped(
                magicllm::LlmScope::new(
                    context.execution_context.principal.clone(),
                    context.execution_context.workspace.clone(),
                ),
                Some(&system_prompt),
                &prompt,
            )
            .await
        {
            Ok(resp) => {
                let duration = start.elapsed();

                // Emit LLM analysis completed event
                if let (Some(ref tid), Some(ref cid), Some(ref broadcaster)) = (
                    &context.execution_id,
                    &context.correlation_id,
                    &self.event_broadcaster,
                ) {
                    broadcaster.llm_analysis_completed(
                        tid,
                        cid,
                        self.llm_service.provider_name(),
                        "plan_generation".to_string(),
                        resp.content.len(),
                        duration.as_millis() as u64,
                    );
                }
                resp
            },
            Err(e) => {
                // Emit LLM analysis failed event
                if let (Some(ref tid), Some(ref cid), Some(ref broadcaster)) = (
                    &context.execution_id,
                    &context.correlation_id,
                    &self.event_broadcaster,
                ) {
                    let error_type = if e.to_string().contains("timeout") {
                        "timeout"
                    } else if e.to_string().contains("api_key") {
                        "api_key_missing"
                    } else if e.to_string().contains("network") {
                        "network_error"
                    } else {
                        "invalid_response"
                    };

                    broadcaster.llm_analysis_failed(
                        tid,
                        cid,
                        self.llm_service.provider_name(),
                        "plan_generation".to_string(),
                        error_type.to_string(),
                        e.to_string(),
                    );
                }
                return Err(e);
            },
        };

        let parsed = self.parse_llm_decomposition_response(&llm_response.content, decomp_analysis);
        if let Some(broadcaster) = self.event_broadcaster.as_ref() {
            let telemetry = OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                context.execution_context.principal.clone(),
                context.execution_context.workspace.clone(),
                "task_decomposition",
            );
            let attribution = OperationLlmCallAttribution {
                execution_id: context.execution_id.clone(),
                task_id: context.execution_context.metadata.get("task_id").cloned(),
                agent_id: context.execution_context.metadata.get("agent_id").cloned(),
                ..OperationLlmCallAttribution::default()
            };
            let latency_ms = start.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_validated_success(
                    "task_decomposition",
                    &llm_response,
                    latency_ms,
                    attribution,
                    "task_decomposition_json",
                ),
                Err(error) => telemetry.emit_validation_failure(
                    "task_decomposition",
                    &llm_response,
                    latency_ms,
                    attribution,
                    "task_decomposition_json",
                    &error.to_string(),
                ),
            }
        }

        // Parse LLM response into subtasks
        parsed
    }

    /// Parse LLM response into structured subtasks
    fn parse_llm_decomposition_response(
        &self,
        response: &str,
        _decomp_analysis: &DecompositionAnalysis,
    ) -> Result<Vec<SubTask>> {
        let mut subtasks = Vec::new();

        for line in response.lines() {
            let line = line.trim();
            if line.is_empty() || !line.chars().next().unwrap_or(' ').is_ascii_digit() {
                continue;
            }

            // Parse format: "1. Task name (complexity: 0.5, dependencies: [1,2])"
            if let Some(subtask) = self.parse_subtask_line(line) {
                subtasks.push(subtask);
            }
        }

        // Fallback if parsing failed - use simple decomposition
        if subtasks.is_empty() {
            warn!(
                "[MAGICIAN-V2-STRATEGY] Failed to parse LLM response, using fallback decomposition"
            );
            subtasks = self.fallback_decomposition(response)?;
        }

        Ok(subtasks)
    }

    /// Parse a single subtask line from LLM response
    fn parse_subtask_line(&self, line: &str) -> Option<SubTask> {
        // Handle two formats:
        // 1. "N. Task | Category: cat | Complexity: X.X | Dependencies: [...]"
        // 2. "N. Task (complexity: X.X, dependencies: [...])"

        let parts: Vec<&str> = line.splitn(2, ". ").collect();
        if parts.len() != 2 {
            return None;
        }

        let rest = parts[1];

        // Try pipe-separated format first (new format with categories)
        if rest.contains(" | ") {
            let segments: Vec<&str> = rest.split(" | ").collect();
            if segments.is_empty() {
                return None;
            }

            let description = segments[0].trim().to_string();
            let mut complexity = 0.5;
            let mut suggested_categories = vec![];
            let mut dependencies = vec![];

            // Parse each segment
            for segment in segments.iter().skip(1) {
                let segment = segment.trim();

                if let Some(cat_str) = segment.strip_prefix("Category: ") {
                    let category = cat_str.trim().to_string();
                    if !category.is_empty() {
                        suggested_categories.push(category);
                    }
                } else if let Some(comp_str) = segment.strip_prefix("Complexity: ") {
                    complexity = comp_str.trim().parse::<f32>().unwrap_or(0.5);
                } else if segment.starts_with("Dependencies: ") {
                    dependencies = Self::parse_dependency_indices(segment);
                }
            }

            return Some(SubTask {
                description,
                dependencies,
                complexity,
                priority: 1.0,
                suggested_categories,
                analysis: None,
            });
        }

        // Fall back to parentheses format (old format without categories)
        if let Some(paren_start) = rest.rfind(" (") {
            let description = rest[..paren_start].trim().to_string();
            let metadata = &rest[paren_start..];

            // Extract complexity
            let complexity = if let Some(comp_start) = metadata.find("complexity: ") {
                let comp_str = &metadata[comp_start + 12..];
                if let Some(comp_end) = comp_str.find(',') {
                    comp_str[..comp_end].trim().parse::<f32>().unwrap_or(0.5)
                } else {
                    0.5
                }
            } else {
                0.5
            };

            // Extract dependencies (simplified - could be enhanced)
            let dependencies = if let Some(dep_start) = metadata.find("dependencies: ") {
                let dep_str = &metadata[dep_start + "dependencies: ".len()..];
                Self::parse_dependency_indices(dep_str)
            } else {
                vec![]
            };

            return Some(SubTask {
                description,
                dependencies,
                complexity,
                priority: 1.0,
                suggested_categories: vec![],
                analysis: None,
            });
        }

        None
    }

    fn parse_dependency_indices(segment: &str) -> Vec<usize> {
        segment
            .split(|c: char| !c.is_ascii_digit() && c != ',')
            .filter_map(|token| {
                let trimmed = token.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    trimmed.parse::<usize>().ok()
                }
            })
            .collect()
    }

    /// Fallback decomposition if LLM parsing fails
    fn fallback_decomposition(&self, response: &str) -> Result<Vec<SubTask>> {
        // Simple fallback: split response into sentences and treat as subtasks
        let sentences: Vec<&str> = response
            .split('.')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && s.len() > 10)
            .take(self.limits.max_subtasks_per_step)
            .collect();

        Ok(sentences
            .into_iter()
            .enumerate()
            .map(|(i, desc)| SubTask {
                description: desc.to_string(),
                dependencies: if i > 0 { vec![i - 1] } else { vec![] },
                complexity: 0.5,
                priority: 1.0,
                suggested_categories: vec![],
                analysis: None,
            })
            .collect())
    }

    /// Allocate portion of resource budget to child task
    fn allocate_budget_for_child(&self, parent_budget: &ResourceBudget) -> ResourceBudget {
        // Simple equal allocation among estimated children
        // Could be enhanced with priority-based allocation
        let allocation_factor = 0.3; // Give each child 30% of parent's remaining budget

        let remaining_calls = parent_budget.remaining_llm_calls();
        let remaining_time = parent_budget.remaining_time_ms();

        ResourceBudget {
            max_llm_calls: ((remaining_calls as f32) * allocation_factor) as u32,
            max_time_ms: ((remaining_time as f32) * allocation_factor) as u64,
            max_tokens: parent_budget.max_tokens / 3, // Conservative allocation
            consumed_llm_calls: 0,
            consumed_time_ms: 0,
            consumed_tokens: 0,
            start_time: std::time::Instant::now(),
        }
    }
}

// =============================================================================
// UTILITY FUNCTIONS
// =============================================================================

/// Simple text similarity calculation (Levenshtein distance approximation)
fn calculate_text_similarity(text1: &str, text2: &str) -> f32 {
    // Simple word-based similarity for cycle detection
    let words1: HashSet<String> = text1
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let words2: HashSet<String> = text2
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let intersection = words1.intersection(&words2).count();
    let union = words1.union(&words2).count();

    if union == 0 {
        0.0
    } else {
        intersection as f32 / union as f32
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::magician_v2::{
        prompts::{
            json_storage::JsonStorageConfig, storage::PromptStore, JsonPromptStorage, PromptManager,
        },
        query_analysis::{
            operation_llm_router::MockQueryAnalysisLLM, CategoryAnalysis, ComplexityAnalysis,
            DependencyAnalysis, ResourceEstimate, UnifiedQueryAnalysis,
        },
    };

    async fn create_test_decomposer() -> TaskDecomposer {
        let llm_service = Arc::new(MockQueryAnalysisLLM);

        // Create mock tool catalog for tests (no need for actual registry)
        struct MockToolCatalog;

        #[async_trait::async_trait]
        impl ToolCatalog for MockToolCatalog {
            async fn list_tool_names(&self, _context: &ExecutionContext) -> Vec<String> {
                vec!["test_tool".to_string()]
            }

            async fn available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
                vec![
                    "test".to_string(),
                    "network".to_string(),
                    "diagnostics".to_string(),
                ]
            }

            async fn get_tool_metadata(
                &self,
                _tool_name: &str,
                _context: &ExecutionContext,
            ) -> Option<std::collections::HashMap<String, serde_json::Value>> {
                None
            }

            async fn filtered_tools_by_categories(
                &self,
                _categories: &[String],
                _context: &ExecutionContext,
            ) -> Result<Vec<runtime_core::ToolInfo>> {
                Ok(vec![])
            }

            async fn all_tools(
                &self,
                _context: &ExecutionContext,
            ) -> Result<Vec<runtime_core::ToolInfo>> {
                Ok(vec![])
            }

            async fn category_tool_counts(
                &self,
                _context: &ExecutionContext,
            ) -> Result<std::collections::HashMap<String, usize>> {
                Ok(std::collections::HashMap::new())
            }
        }

        let tool_catalog: Arc<dyn ToolCatalog> = Arc::new(MockToolCatalog);

        // Create test prompt manager with JsonPromptStorage
        let storage_config = JsonStorageConfig {
            storage_dir: crate::magician_v2::prompts::json_storage::default_prompt_dir(),
            enable_cache: false,
            max_cache_entries: 100,
        };
        let storage = Arc::new(JsonPromptStorage::new(storage_config).unwrap());
        storage.initialize().await.unwrap();
        let prompt_manager = Arc::new(PromptManager::new(storage));
        prompt_manager.initialize().await.unwrap();
        let query_analyzer = Arc::new(UnifiedQueryAnalyzer::new(
            llm_service.clone(),
            tool_catalog.clone(),
            prompt_manager.clone(),
        ));

        TaskDecomposer::new(
            llm_service,
            query_analyzer,
            prompt_manager,
            tool_catalog,
            None,
        )
    }

    fn create_test_resource_budget() -> ResourceBudget {
        // Create a simple test analysis for ResourceBudget
        let analysis = UnifiedQueryAnalysis {
            original_query: "test query".to_string(),
            complexity: ComplexityAnalysis {
                score: 0.5,
                factors: vec!["test factor".to_string()],
                reasoning: "test reasoning".to_string(),
            },
            categories: CategoryAnalysis {
                categories: vec!["test".to_string()],
                reasoning: "test category reasoning".to_string(),
            },
            dependencies: DependencyAnalysis {
                is_multi_step: false,
                dependencies: vec![],
                workflow_steps: vec![],
                reasoning: "single step task".to_string(),
                required_capabilities: vec![],
            },
            resource_estimate: ResourceEstimate {
                expected_tokens: 100,
                expected_duration_ms: 1000,
                expected_iterations: 1,
            },
            extracted_entities: crate::magician_v2::query_analysis::ExtractedEntities::default(),
            intent: crate::magician_v2::query_analysis::QueryIntent::NewTask,
            slot_match: None,
            llm_calls_used: 0, // Test data
            task_clarity: crate::magician_v2::query_analysis::TaskClarity::default(),
        };
        ResourceBudget::from_analysis(&analysis)
    }

    #[tokio::test]
    async fn test_text_similarity() {
        assert!(calculate_text_similarity("hello world", "hello world") > 0.9);
        assert!(calculate_text_similarity("hello world", "world hello") > 0.9);
        assert!(calculate_text_similarity("hello world", "goodbye world") > 0.3);
        assert!(calculate_text_similarity("hello world", "completely different") < 0.1);
    }

    #[tokio::test]
    async fn test_decomposition_limits() {
        let decomposer = create_test_decomposer().await;
        let context = DecompositionContext::new_root(
            "test task".to_string(),
            create_test_resource_budget(),
            ExecutionContext::default(),
        );

        let analysis = decomposer
            .analyze_decomposition_need("test task", &context)
            .await
            .unwrap();
        assert!(analysis.confidence >= 0.0);
    }

    #[tokio::test]
    async fn test_cycle_detection() {
        let context = DecompositionContext::new_root(
            "analyze customer data pipeline".to_string(),
            create_test_resource_budget(),
            ExecutionContext::default(),
        );
        let child_context =
            context.create_child("collect logs".to_string(), create_test_resource_budget());

        assert!(!child_context.has_similar_ancestor("process information", 0.8));
        assert!(child_context.has_similar_ancestor("analyze customer data", 0.5));
    }
}
