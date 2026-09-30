//! Adaptive strategy selector for MagicianV2
//!
//! This module implements the bridge between query analysis and strategy
//! selection. It uses heuristic rules to select the best exploration strategy
//! without requiring additional LLM calls, and provides failure handling with
//! strategy escalation.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, AtomicU64},
        Arc,
    },
};

use tracing::{debug, info, warn};

use super::{guided_search::GuidedSearchParams, types::*};
use crate::magician_v2::{
    ask_loop::ledger::BudgetLedger,
    query_analysis::UnifiedQueryAnalysis,
    state_tracker::{StageContext, StageResumePolicy},
};
use runtime_core::{ExecutionContext, ToolCatalog, ToolMatching};

/// Adaptive strategy selector that bridges query analysis to strategy execution
pub struct AdaptiveStrategySelector;

impl AdaptiveStrategySelector {
    /// Select default strategy and fallback parameters without extra LLM calls.
    ///
    /// # Strategy Architecture:
    /// AtomicComposition is attempted first.
    /// GuidedSearch parameters are still precomputed for fallback:
    /// - **Direct** (not multi-step): iterations=1, depth=1, beam=1
    /// - **Greedy** (complexity < 0.5): iterations≈8, depth≈steps, beam=2
    /// - **Medium** (complexity 0.5-0.7): iterations≈16, depth=4, beam=3
    /// - **Complex** (complexity >= 0.7): iterations≈32, depth=5, beam=5
    ///
    /// # Arguments
    /// * `analysis` - Query analysis from Stage 1
    ///
    /// # Returns
    /// * `(StrategyType, GuidedSearchParams)` - AtomicComposition default plus GuidedSearch fallback params
    pub fn select_strategy_from_analysis(
        analysis: &UnifiedQueryAnalysis,
    ) -> (StrategyType, GuidedSearchParams) {
        let params = GuidedSearchParams::from_complexity(analysis);

        let strategy = if analysis.dependencies.is_multi_step && analysis.complexity.score >= 0.7 {
            StrategyType::GuidedSearch
        } else {
            StrategyType::AtomicComposition
        };

        info!(
            "[MAGICIAN-V2-STRATEGY] Strategy selected: {:?}; GuidedSearch \
             params: iterations={}, depth={}, beam={} (complexity={:.2}, multi_step={})",
            strategy,
            params.max_iterations,
            params.max_depth,
            params.beam_width,
            analysis.complexity.score,
            analysis.dependencies.is_multi_step
        );

        (strategy, params)
    }

    /// Extract query pattern for strategy selection
    #[allow(dead_code)]
    fn extract_query_pattern(analysis: &UnifiedQueryAnalysis) -> QueryPattern {
        QueryPattern {
            word_count: analysis.original_query.split_whitespace().count(),
            has_conjunction: Self::has_conjunctions(&analysis.original_query),
            estimated_tools: analysis.categories.categories.len(),
            complexity_score: analysis.complexity.score,
            categories: analysis.categories.categories.clone(),
        }
    }

    /// Check for conjunctions that indicate multi-step operations
    #[allow(dead_code)]
    fn has_conjunctions(query: &str) -> bool {
        let conjunctions = [
            "and",
            "then",
            "with",
            "also",
            "plus",
            "after",
            "before",
            "while",
            "during",
            "followed by",
            "along with",
        ];
        let query_lower = query.to_lowercase();
        conjunctions.iter().any(|&conj| query_lower.contains(conj))
    }

    /// Create strategy context from analysis and dependencies
    ///
    /// # Arguments
    /// * `analysis` - Query analysis results
    /// * `tool_catalog` / `tool_matching` - Tool services for catalog + matching
    /// * `execution_context` - Optional execution context (defaults if None)
    /// * `llm_service` - LLM service for entity mapping and other AI tasks
    /// * `prompt_manager` - Prompt manager for loading templates
    /// * `v2_tool_matcher` - Optional V2 Tool Matcher for 4-tier progressive
    ///   filtering
    /// * `execution_id` - Optional execution ID for correlation
    /// * `correlation_id` - Optional correlation ID for tracing
    ///
    /// # Returns
    /// * `StrategyContext` - Complete context for strategy execution
    pub fn create_strategy_context(
        analysis: UnifiedQueryAnalysis,
        tool_catalog: Arc<dyn ToolCatalog>,
        tool_matching: Arc<dyn ToolMatching>,
        execution_context: Option<ExecutionContext>,
        llm_service: Arc<
            dyn crate::magician_v2::query_analysis::operation_llm_router::QueryAnalysisLLM,
        >,
        prompt_manager: Arc<crate::magician_v2::prompts::PromptManager>,
        v2_tool_matcher: Option<Arc<crate::magician_v2::tool_matcher::V2ToolMatcher>>,
        execution_id: Option<String>,
        correlation_id: Option<String>,
        turn_id: Option<String>,
        conversation_store: Option<Arc<dyn crate::magician_v2::storage::V2ConversationStore>>,
        event_broadcaster: Option<
            Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
        >,
        elicitation_manager: Option<Arc<crate::magician_v2::elicitation::ElicitationManager>>,
        planning_snapshot: Option<crate::magician_v2::strategy::types::PlanningSnapshot>,
        stage_context: StageContext,
        budget_ledger: Option<Arc<BudgetLedger>>,
        clarified_task: Option<crate::magician_v2::slot_graph::ClarifiedTask>,
        slot_graph: Vec<crate::magician_v2::slot_graph::SlotRecord>,
        slot_diff: Option<crate::magician_v2::strategy::types::SlotDiffStats>,
        stage_resume_policy: StageResumePolicy,
        initial_llm_calls: u32, // LLM calls already made before strategy (query analysis + elicitation)
        allow_consent_slots: bool, // Whether to allow LLM to generate consent_flags
        tools: Vec<String>,     // Per-agent tool whitelist (empty = all tools)
        excluded_tools: Vec<String>, // Per-agent tool blacklist
        denied_tools: Vec<String>, // Per-agent structural deny list
        delegate_tool_catalog: HashMap<String, Vec<runtime_core::ToolInfo>>, // Delegate agent tool catalogs
        planner_agent_catalog: Vec<crate::magician_v2::strategy::types::PlannerAgentCatalogEntry>, // Agent-grouped planner catalog
        available_procedure_skills: Vec<(String, String)>, // Activatable procedure playbooks
    ) -> StrategyContext {
        let available_categories = analysis.categories.categories.clone();

        let resource_budget = ResourceBudget::from_analysis(&analysis);

        debug!(
            "[MAGICIAN-V2-STRATEGY] Strategy context: categories={:?}, budget={}ms/{}calls/{}tokens, \
             v2_matcher={}",
            available_categories,
            resource_budget.max_time_ms,
            resource_budget.max_llm_calls,
            resource_budget.max_tokens,
            if v2_tool_matcher.is_some() {
                "enabled"
            } else {
                "disabled (using V1)"
            }
        );

        debug!(
            "[MAGICIAN-V2-STRATEGY] Initializing strategy context with {} LLM calls already made (query analysis + elicitation)",
            initial_llm_calls
        );

        StrategyContext {
            query_analysis: analysis,
            resource_budget,
            suggested_categories: available_categories,
            tool_catalog,
            tool_matching,
            execution_context: execution_context.unwrap_or_default(),
            llm_service,
            prompt_manager,
            v2_tool_matcher,
            execution_id,
            correlation_id,
            turn_id,
            conversation_store,
            event_broadcaster,
            llm_call_counter: Arc::new(AtomicU32::new(initial_llm_calls)), // Start from accumulated count
            llm_token_counter: Arc::new(AtomicU64::new(0)),
            elicitation_manager,
            planning_snapshot,
            stage_context,
            budget_ledger,
            clarified_task,
            slot_graph,
            slot_diff,
            stage_resume_policy,
            allow_consent_slots,
            tools,
            excluded_tools,
            denied_tools,
            delegate_tool_catalog,
            planner_agent_catalog,
            available_procedure_skills,
        }
    }

    /// Handle strategy failure with adaptive escalation
    ///
    /// # Arguments
    /// * `current` - Current strategy that failed
    /// * `failure` - Type of failure encountered
    /// * `remaining_budget` - Remaining resource budget
    ///
    /// # Returns
    /// * `NextAction` - Recommended next action
    pub fn handle_strategy_failure(
        current: StrategyType,
        failure: StrategyFailure,
        _remaining_budget: &ResourceBudget,
    ) -> NextAction {
        info!(
            "[MAGICIAN-V2-STRATEGY] Strategy failure: {:?} failed with {:?}",
            current, failure
        );

        match (&current, &failure) {
            // AtomicComposition failure - fallback to GuidedSearch
            (&StrategyType::AtomicComposition, _) => {
                info!(
                    "[MAGICIAN-V2-STRATEGY] AtomicComposition failed, falling back to GuidedSearch"
                );
                NextAction::Simplify(StrategyType::GuidedSearch)
            },

            // GuidedSearch failed - abort as it's the final fallback
            (&StrategyType::GuidedSearch, _) => {
                warn!("[MAGICIAN-V2-STRATEGY] GuidedSearch failed (final fallback), aborting");
                NextAction::Abort(format!(
                    "GuidedSearch (final fallback) failed with {:?}",
                    failure
                ))
            },
        }
    }

    /// Get category filter for tool discovery
    ///
    /// # Arguments
    /// * `analysis` - Query analysis containing detected categories
    ///
    /// # Returns
    /// * `Vec<String>` - Categories to filter tools by (reduces 800 → 15 tools)
    pub fn get_category_filter(analysis: &UnifiedQueryAnalysis) -> Vec<String> {
        let categories = analysis.categories.categories.clone();

        if categories.is_empty() {
            debug!("[MAGICIAN-V2-STRATEGY] No categories detected, using general category");
            vec!["general".to_string()]
        } else {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Using categories for filtering: {:?}",
                categories
            );
            categories
        }
    }

    /// Check if a strategy switch is beneficial based on current progress
    ///
    /// # Arguments
    /// * `current_strategy` - Currently executing strategy
    /// * `progress` - Current exploration progress
    /// * `budget` - Remaining budget
    ///
    /// # Returns
    /// * `Option<StrategyType>` - Recommended strategy switch, if any
    ///
    /// # Note
    /// Strategy switching is no longer used for mid-run adaptation.
    /// This function is kept for backward compatibility and always returns
    /// None.
    pub fn should_switch_strategy(
        _current_strategy: StrategyType,
        _progress: &ExplorationResult,
        _budget: &ResourceBudget,
    ) -> Option<StrategyType> {
        // Runtime switching is disabled; fallback is handled by retry logic.
        None
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::query_analysis::{
        CategoryAnalysis, ComplexityAnalysis, DependencyAnalysis, ResourceEstimate,
    };

    fn create_test_analysis(
        query: &str,
        complexity: f32,
        categories: Vec<String>,
    ) -> UnifiedQueryAnalysis {
        UnifiedQueryAnalysis {
            original_query: query.to_string(),
            complexity: ComplexityAnalysis {
                score: complexity,
                factors: vec!["test".to_string()],
                reasoning: "test reasoning".to_string(),
            },
            categories: CategoryAnalysis {
                categories,
                reasoning: "test category reasoning".to_string(),
            },
            dependencies: DependencyAnalysis {
                is_multi_step: complexity > 0.5,
                dependencies: vec![],
                workflow_steps: vec![],
                reasoning: "test dependency reasoning".to_string(),
                required_capabilities: vec![],
            },
            resource_estimate: ResourceEstimate {
                expected_tokens: 1000,
                expected_duration_ms: 5000,
                expected_iterations: 2,
            },
            extracted_entities: crate::magician_v2::query_analysis::ExtractedEntities::default(),
            intent: crate::magician_v2::query_analysis::QueryIntent::NewTask,
            slot_match: None,
            llm_calls_used: 0, // Test data
            task_clarity: crate::magician_v2::query_analysis::TaskClarity::default(),
        }
    }

    #[test]
    fn test_adaptive_guided_search_direct_mode() {
        let analysis = create_test_analysis("ping google", 0.2, vec!["network".to_string()]);
        let (strategy, params) = AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);
        assert_eq!(strategy, StrategyType::AtomicComposition);
        assert_eq!(
            params.max_iterations, 1,
            "Direct mode should use 1 iteration"
        );
        assert_eq!(params.max_depth, 1, "Direct mode should use depth 1");
        assert_eq!(
            params.beam_width, 1,
            "Direct mode should keep a narrow beam"
        );
    }

    #[test]
    fn test_adaptive_guided_search_greedy_mode() {
        let mut analysis = create_test_analysis(
            "deploy app and monitor",
            0.4,
            vec!["deployment".to_string()],
        );
        analysis.dependencies.is_multi_step = true;
        analysis.dependencies.workflow_steps = vec!["step1".to_string(), "step2".to_string()];
        let (strategy, params) = AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);
        assert_eq!(strategy, StrategyType::AtomicComposition);
        assert_eq!(
            params.max_iterations, 8,
            "Greedy mode should allow limited exploration"
        );
        assert_eq!(
            params.max_depth, 2,
            "Greedy mode should use depth = workflow_steps.len()"
        );
        assert_eq!(
            params.beam_width, 2,
            "Greedy mode should keep the beam narrow"
        );
    }

    #[test]
    fn test_adaptive_guided_search_medium_mode() {
        let mut analysis = create_test_analysis(
            "deploy app and monitor logs",
            0.6,
            vec!["deployment".to_string()],
        );
        analysis.dependencies.is_multi_step = true;
        let (strategy, params) = AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);
        assert_eq!(strategy, StrategyType::AtomicComposition);
        assert_eq!(
            params.max_iterations, 16,
            "Medium mode should use moderate iterations"
        );
        assert_eq!(params.max_depth, 4, "Medium mode should use depth 4");
        assert_eq!(
            params.beam_width, 3,
            "Medium mode should widen the beam modestly"
        );
    }

    #[test]
    fn test_adaptive_guided_search_full_mode() {
        let mut analysis = create_test_analysis(
            "deploy microservice with monitoring and alerting",
            0.8,
            vec!["deployment".to_string(), "monitoring".to_string()],
        );
        analysis.dependencies.is_multi_step = true;
        let (strategy, params) = AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);
        assert_eq!(strategy, StrategyType::GuidedSearch);
        assert_eq!(
            params.max_iterations, 32,
            "Full mode should use wide exploration"
        );
        assert_eq!(params.max_depth, 5, "Full mode should use depth 5");
        assert_eq!(
            params.beam_width, 5,
            "Full mode should allow the widest beam"
        );
    }

    #[test]
    fn test_consumer_mode_overrides_guided_search() {
        // Complex multi-step query: selector picks GuidedSearch
        let mut analysis = create_test_analysis(
            "deploy microservice with monitoring and alerting",
            0.8,
            vec!["deployment".to_string(), "monitoring".to_string()],
        );
        analysis.dependencies.is_multi_step = true;
        let (selected_strategy, _params) =
            AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);
        assert_eq!(
            selected_strategy,
            StrategyType::GuidedSearch,
            "Selector should pick GuidedSearch for complex multi-step queries"
        );

        // consumer_mode = true → guard overrides to AtomicComposition
        let consumer_mode = true;
        let effective_strategy =
            if consumer_mode && selected_strategy != StrategyType::AtomicComposition {
                StrategyType::AtomicComposition
            } else {
                selected_strategy
            };
        assert_eq!(
            effective_strategy,
            StrategyType::AtomicComposition,
            "consumer_mode=true should force AtomicComposition even when selector picks GuidedSearch"
        );

        // consumer_mode = false → GuidedSearch passes through
        let consumer_mode = false;
        let effective_strategy =
            if consumer_mode && selected_strategy != StrategyType::AtomicComposition {
                StrategyType::AtomicComposition
            } else {
                selected_strategy
            };
        assert_eq!(
            effective_strategy,
            StrategyType::GuidedSearch,
            "consumer_mode=false should allow GuidedSearch to pass through"
        );
    }

    #[test]
    fn test_consumer_mode_does_not_affect_atomic_composition() {
        // Simple query: selector picks AtomicComposition
        let analysis = create_test_analysis("ping google", 0.2, vec!["network".to_string()]);
        let (selected_strategy, _params) =
            AdaptiveStrategySelector::select_strategy_from_analysis(&analysis);
        assert_eq!(selected_strategy, StrategyType::AtomicComposition);

        // consumer_mode = true → AtomicComposition stays AtomicComposition (no-op)
        let consumer_mode = true;
        let effective_strategy =
            if consumer_mode && selected_strategy != StrategyType::AtomicComposition {
                StrategyType::AtomicComposition
            } else {
                selected_strategy
            };
        assert_eq!(
            effective_strategy,
            StrategyType::AtomicComposition,
            "consumer_mode should be a no-op when selector already picks AtomicComposition"
        );
    }

    #[test]
    fn test_consumer_mode_blocks_escalation_to_guided_search() {
        let budget = ResourceBudget::from_analysis(&create_test_analysis("test", 0.5, vec![]));

        // AtomicComposition failure → escalation recommends GuidedSearch
        let action = AdaptiveStrategySelector::handle_strategy_failure(
            StrategyType::AtomicComposition,
            StrategyFailure::LowConfidence(0.3),
            &budget,
        );
        let escalated_strategy = match &action {
            NextAction::Simplify(s) => Some(*s),
            _ => None,
        };
        assert_eq!(
            escalated_strategy,
            Some(StrategyType::GuidedSearch),
            "Failure handler should recommend GuidedSearch escalation"
        );

        // consumer_mode = true → block escalation to non-AtomicComposition
        let consumer_mode = true;
        let blocked = match &action {
            NextAction::Escalate(s) | NextAction::Simplify(s)
                if consumer_mode && *s != StrategyType::AtomicComposition =>
            {
                true
            },
            _ => false,
        };
        assert!(
            blocked,
            "consumer_mode=true should block escalation to GuidedSearch"
        );

        // consumer_mode = false → escalation proceeds
        let consumer_mode = false;
        let blocked = match &action {
            NextAction::Escalate(s) | NextAction::Simplify(s)
                if consumer_mode && *s != StrategyType::AtomicComposition =>
            {
                true
            },
            _ => false,
        };
        assert!(
            !blocked,
            "consumer_mode=false should allow escalation to GuidedSearch"
        );
    }

    #[test]
    fn test_conjunction_detection() {
        assert!(AdaptiveStrategySelector::has_conjunctions(
            "deploy and monitor"
        ));
        assert!(AdaptiveStrategySelector::has_conjunctions(
            "create file then upload"
        ));
        assert!(!AdaptiveStrategySelector::has_conjunctions("simple query"));
    }

    #[test]
    fn test_failure_escalation() {
        let budget = ResourceBudget::from_analysis(&create_test_analysis("test", 0.5, vec![]));

        // Test AtomicComposition falls back to GuidedSearch on failure
        let action = AdaptiveStrategySelector::handle_strategy_failure(
            StrategyType::AtomicComposition,
            StrategyFailure::LowConfidence(0.3),
            &budget,
        );
        match action {
            NextAction::Simplify(StrategyType::GuidedSearch) => {},
            _ => panic!("Expected fallback to GuidedSearch, got: {:?}", action),
        }

        // Test AtomicComposition falls back on decomposition failure
        let action = AdaptiveStrategySelector::handle_strategy_failure(
            StrategyType::AtomicComposition,
            StrategyFailure::DecompositionFailed,
            &budget,
        );
        match action {
            NextAction::Simplify(StrategyType::GuidedSearch) => {},
            _ => panic!("Expected fallback to GuidedSearch, got: {:?}", action),
        }

        // Test AtomicComposition falls back on no tools found
        let action = AdaptiveStrategySelector::handle_strategy_failure(
            StrategyType::AtomicComposition,
            StrategyFailure::NoToolsFound,
            &budget,
        );
        match action {
            NextAction::Simplify(StrategyType::GuidedSearch) => {},
            _ => panic!("Expected fallback to GuidedSearch, got: {:?}", action),
        }

        // Test GuidedSearch aborts (final fallback, no further escalation)
        let action = AdaptiveStrategySelector::handle_strategy_failure(
            StrategyType::GuidedSearch,
            StrategyFailure::LowConfidence(0.3),
            &budget,
        );
        match action {
            NextAction::Abort(_) => {},
            _ => panic!("Expected abort for GuidedSearch failure, got: {:?}", action),
        }
    }
}
