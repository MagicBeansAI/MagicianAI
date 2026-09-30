// Integration tests for Adaptive GuidedSearch Strategy and Failure Recovery
//
// These tests verify the unified GuidedSearch architecture:
// - GuidedSearch adapts parameters based on complexity (direct/greedy/medium/full
//   modes)
// - GuidedSearch → AtomicComposition escalation on failure
// - Budget tracking and resource management
// - Workflow steps integration from query analysis

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use magician::magician_v2::{
    prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
    query_analysis::{
        operation_llm_router::MockQueryAnalysisLLM, CategoryAnalysis, ComplexityAnalysis,
        DependencyAnalysis, ExtractedEntities, QueryIntent, ResourceEstimate, UnifiedQueryAnalysis,
    },
    slot_graph::{ClarifiedTask, SlotRecord},
    state_tracker::{StageContext, StageResumePolicy},
    strategy::{
        AdaptiveStrategySelector, NextAction, ResourceBudget, StrategyContext, StrategyFailure,
        StrategyType,
    },
};
use runtime_core::{
    ExecutionContext, MultipleToolMatchResult, SuccessMetrics, ToolDiscovery, ToolMatch,
    ToolMatchResult, ToolMetadata,
};

// Mock ToolDiscovery that returns configurable confidence
struct MockToolDiscovery {
    confidence: f32,
}

impl MockToolDiscovery {
    fn new(confidence: f32) -> Self {
        Self { confidence }
    }
}

#[async_trait]
impl ToolDiscovery for MockToolDiscovery {
    async fn find_best_match_with_context(
        &self,
        _task: &str,
        _context: &ExecutionContext,
    ) -> ToolMatchResult {
        if self.confidence > 0.0 {
            ToolMatchResult {
                primary_match: Some(ToolMatch {
                    tool_name: format!("test_tool_confidence_{:.2}", self.confidence),
                    capability_match: self.confidence,
                    parameter_mapping: HashMap::new(),
                    execution_confidence: self.confidence,
                    tool_metadata: ToolMetadata {
                        name: "test_tool".to_string(),
                        description: "test description".to_string(),
                        category: "test".to_string(),
                        typical_use_cases: vec!["testing".to_string()],
                        input_schema: serde_json::json!({}),
                        output_schema: serde_json::json!({}),
                        success_rate: 0.9,
                        avg_execution_time: 100.0,
                        enhanced_description: Some("Enhanced test description".to_string()),
                        keywords: vec!["test".to_string()],
                        use_cases: vec!["testing".to_string()],
                        confidence_score: self.confidence,
                        success_metrics: SuccessMetrics {
                            success_rate: 0.9,
                            avg_execution_time: 100.0,
                            reliability_score: 0.95,
                            last_updated: chrono::Utc::now().timestamp(),
                        },
                        last_updated: chrono::Utc::now().timestamp(),
                    },
                }),
                match_confidence: self.confidence,
                missing_capabilities: vec![],
                parameter_coverage: 1.0,
                executable: true,
            }
        } else {
            ToolMatchResult {
                primary_match: None,
                match_confidence: 0.0,
                missing_capabilities: vec![],
                parameter_coverage: 0.0,
                executable: false,
            }
        }
    }

    async fn find_multiple_matches_with_context(
        &self,
        _task: &str,
        _context: &ExecutionContext,
    ) -> MultipleToolMatchResult {
        MultipleToolMatchResult {
            matches: vec![],
            match_strategies: vec![],
            confidence_spread: 0.0,
            recommended_approach: None,
            any_executable: false,
            aggregate_missing_capabilities: vec![],
        }
    }

    async fn is_tool_available(&self, _tool_name: &str, _context: &ExecutionContext) -> bool {
        true
    }

    async fn get_tool_metadata(
        &self,
        _tool_name: &str,
        _context: &ExecutionContext,
    ) -> Option<HashMap<String, serde_json::Value>> {
        None
    }

    async fn get_available_tools(&self, _context: &ExecutionContext) -> Vec<String> {
        vec!["test_tool".to_string()]
    }

    async fn get_available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
        vec!["test".to_string()]
    }
}

// Helper to create test query analysis with configurable complexity
fn create_test_analysis(query: &str, complexity: f32, multi_step: bool) -> UnifiedQueryAnalysis {
    UnifiedQueryAnalysis {
        original_query: query.to_string(),
        complexity: ComplexityAnalysis {
            score: complexity,
            factors: vec!["test_factor".to_string()],
            reasoning: "test reasoning".to_string(),
        },
        categories: CategoryAnalysis {
            categories: vec!["test".to_string()],
            reasoning: "test category reasoning".to_string(),
        },
        dependencies: DependencyAnalysis {
            is_multi_step: multi_step,
            dependencies: vec![],
            workflow_steps: if multi_step {
                vec!["step1".to_string(), "step2".to_string()]
            } else {
                vec![]
            },
            reasoning: "test dependency reasoning".to_string(),
            required_capabilities: vec![],
        },
        resource_estimate: ResourceEstimate {
            expected_tokens: 100,
            expected_duration_ms: 1000,
            expected_iterations: 1,
        },
        extracted_entities: ExtractedEntities::default(),
        intent: QueryIntent::NewTask,
        slot_match: None,
        llm_calls_used: 0, // Test data
        task_clarity: magician::magician_v2::query_analysis::TaskClarity::default(),
    }
}

// Helper to create test strategy context
async fn create_test_context(
    analysis: UnifiedQueryAnalysis,
    tool_discovery: Arc<dyn ToolDiscovery>,
) -> StrategyContext {
    let execution_context = ExecutionContext {
        principal: "test_user".to_string(),
        workspace: "test_workspace".to_string(),
        metadata: HashMap::new(),
    };

    let llm_service = Arc::new(MockQueryAnalysisLLM);

    // Create prompt manager
    let temp_dir = tempfile::TempDir::new().unwrap();
    let storage_config = JsonStorageConfig {
        storage_dir: temp_dir.path().to_path_buf(),
        enable_cache: true,
        max_cache_entries: 100,
    };
    let storage = Arc::new(JsonPromptStorage::new(storage_config).unwrap());
    let prompt_manager = Arc::new(PromptManager::new(storage));

    // Wrap ToolDiscovery into ToolCatalog and ToolMatching using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery.clone(),
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();
    let tool_matching: Arc<dyn runtime_core::ToolMatching> = tool_adapter.clone();

    AdaptiveStrategySelector::create_strategy_context(
        analysis.clone(),
        tool_catalog,
        tool_matching,
        Some(execution_context),
        llm_service,
        prompt_manager,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        StageContext::PlanningBootstrap,
        None,
        None::<ClarifiedTask>,
        Vec::<SlotRecord>::new(),
        None,
        StageResumePolicy::default(),
        0,              // initial_llm_calls for test
        false,          // allow_consent_slots: default for tests
        Vec::new(),     // tools: empty = all tools
        Vec::new(),     // excluded_tools: empty = no exclusions
        Vec::new(),     // denied_tools: empty = no structural denies
        HashMap::new(), // delegate_tool_catalog: empty = no delegates
        Vec::new(),     // planner_agent_catalog: empty = no agent grouping
        Vec::new(),     // available_procedure_skills: empty = no activatable playbooks
    )
}

#[tokio::test]
async fn test_adaptive_guided_search_direct_mode() {
    // Test GuidedSearch fallback params in direct mode (single tool, not multi-step)
    let analysis = create_test_analysis("ping google.com", 0.2, false);
    let (strategy_type, params) =
        AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);

    assert_eq!(strategy_type, StrategyType::AtomicComposition);
    assert_eq!(
        params.max_iterations, 1,
        "Direct mode should use 1 iteration"
    );
    assert_eq!(params.max_depth, 1, "Direct mode should use depth 1");
}

#[tokio::test]
async fn test_adaptive_guided_search_greedy_mode() {
    // Test GuidedSearch fallback params in greedy mode (multi-step, low complexity)
    let analysis = create_test_analysis("is google up", 0.4, true);
    let (strategy_type, params) =
        AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);

    assert_eq!(strategy_type, StrategyType::AtomicComposition);
    assert_eq!(
        params.max_iterations, 8,
        "Greedy mode should use 8 iterations"
    );
    assert_eq!(
        params.max_depth, 2,
        "Greedy mode should use depth = workflow_steps.len()"
    );
}

#[tokio::test]
async fn test_adaptive_guided_search_medium_mode() {
    // Test GuidedSearch fallback params in medium exploration mode
    let analysis = create_test_analysis("deploy app and monitor", 0.6, true);
    let (strategy_type, params) =
        AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);

    assert_eq!(strategy_type, StrategyType::AtomicComposition);
    assert_eq!(
        params.max_iterations, 16,
        "Medium mode should use 16 iterations"
    );
    assert_eq!(params.max_depth, 4, "Medium mode should use depth 4");
}

#[tokio::test]
async fn test_adaptive_guided_search_full_mode() {
    // With complexity >= 0.7 and is_multi_step=true, selector picks GuidedSearch directly.
    let analysis = create_test_analysis(
        "deploy microservice with monitoring and alerting",
        0.8,
        true,
    );
    let (strategy_type, params) =
        AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);

    assert_eq!(strategy_type, StrategyType::GuidedSearch);
    assert_eq!(
        params.max_iterations, 32,
        "Full mode should use 32 iterations"
    );
    assert_eq!(params.max_depth, 5, "Full mode should use depth 5");
}

#[tokio::test]
async fn test_atomic_composition_fallback_to_guided_search() {
    // Test fallback from AtomicComposition to GuidedSearch on failure.
    // Use complexity < 0.7 so the selector picks AtomicComposition (not GuidedSearch).
    let analysis = create_test_analysis("complex task", 0.6, true);
    let tool_discovery = Arc::new(MockToolDiscovery::new(0.3)); // Low confidence
    let context = create_test_context(analysis.clone(), tool_discovery).await;

    let (strategy_type, _params) =
        AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);

    assert_eq!(
        strategy_type,
        StrategyType::AtomicComposition,
        "With complexity < 0.7, AtomicComposition should be selected"
    );

    // Simulate AtomicComposition failure with low confidence
    let failure = StrategyFailure::LowConfidence(0.3);
    let next_action = AdaptiveStrategySelector::handle_strategy_failure(
        strategy_type,
        failure,
        &context.resource_budget,
    );

    // Should simplify to GuidedSearch
    match next_action {
        NextAction::Simplify(StrategyType::GuidedSearch) => {
            // Expected behavior
        },
        _ => panic!("Expected simplify to GuidedSearch, got: {:?}", next_action),
    }
}

#[tokio::test]
async fn test_budget_tracking() {
    // Test resource budget creation and tracking
    let analysis = create_test_analysis("test query", 0.7, true);
    let budget = ResourceBudget::from_analysis(&analysis);

    // Verify budget values (with complexity factor)
    assert!(
        budget.max_llm_calls >= 100,
        "Should have massive LLM call budget"
    );
    assert!(
        budget.max_time_ms >= 600000,
        "Should have 10-minute time budget"
    );
    assert_eq!(
        budget.consumed_llm_calls, 0,
        "Should start with 0 consumed calls"
    );
    assert!(
        budget.can_continue(),
        "New budget should allow continuation"
    );
}

#[tokio::test]
async fn test_abort_when_guided_search_fails() {
    // Test that system aborts when fallback GuidedSearch fails
    let analysis = create_test_analysis("impossible task", 0.9, true);
    let tool_discovery = Arc::new(MockToolDiscovery::new(0.0)); // No tools found
    let context = create_test_context(analysis, tool_discovery).await;

    // Simulate GuidedSearch failure
    let failure = StrategyFailure::NoToolsFound;
    let next_action = AdaptiveStrategySelector::handle_strategy_failure(
        StrategyType::GuidedSearch,
        failure,
        &context.resource_budget,
    );

    // Should abort (no further escalation from GuidedSearch)
    match next_action {
        NextAction::Abort(_reason) => {
            // Expected behavior
        },
        _ => panic!("Expected abort, got: {:?}", next_action),
    }
}
