//! Guided search exploration strategy (LLM-guided best-first/beam search)
//!
//! This strategy replaces the previous MCTS/PUCT implementation with a
//! deterministic best-first search that is guided by LLM-derived confidence
//! scores. The search maintains a bounded priority queue (beam) of partial
//! plans, always expanding the most promising candidate first while trimming
//! low-scoring branches early. This reduces token usage and makes exploration
//! behaviour easier to reason about compared to stochastic rollouts.

use std::{
    cmp::Ordering,
    collections::{hash_map::DefaultHasher, BinaryHeap, HashMap},
    hash::{Hash, Hasher},
    sync::{Arc, RwLock},
};

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    decomposer::*,
    early_termination::{DiminishingReturnsDetector, EarlyTerminationConfig},
    plan::PlanStep,
    plan_builder,
    traits::*,
    types::*,
};

/// Parameters for the guided search strategy, derived from query complexity.
#[derive(Debug, Clone)]
pub struct GuidedSearchParams {
    pub max_iterations: u32,
    pub max_depth: u32,
    pub beam_width: usize,
}

impl GuidedSearchParams {
    /// Create guided search parameters from query analysis complexity.
    ///
    /// The heuristic keeps the search narrow on simple queries and widens the
    /// beam for complex multi-step workflows.
    pub fn from_complexity(
        analysis: &crate::magician_v2::query_analysis::UnifiedQueryAnalysis,
    ) -> Self {
        if !analysis.dependencies.is_multi_step {
            // Direct mode: evaluate the root task only.
            info!(
                "[MAGICIAN-V2-STRATEGY] GuidedSearch direct mode: single tool match (not multi-step)"
            );
            Self {
                max_iterations: 1,
                max_depth: 1,
                beam_width: 1,
            }
        } else if analysis.complexity.score < 0.5 {
            // Greedy mode: linear workflow, narrow beam.
            let depth = analysis.dependencies.workflow_steps.len().max(2) as u32;
            info!(
                "[MAGICIAN-V2-STRATEGY] GuidedSearch greedy mode (complexity={:.2}, steps={})",
                analysis.complexity.score, depth
            );
            Self {
                max_iterations: 8,
                max_depth: depth,
                beam_width: 2,
            }
        } else if analysis.complexity.score < 0.7 {
            // Moderate exploration.
            info!(
                "[MAGICIAN-V2-STRATEGY] GuidedSearch medium mode (complexity={:.2})",
                analysis.complexity.score
            );
            Self {
                max_iterations: 16,
                max_depth: 4,
                beam_width: 3,
            }
        } else {
            // Complex queries: wider beam and more iterations.
            info!(
                "[MAGICIAN-V2-STRATEGY] GuidedSearch full mode (complexity={:.2})",
                analysis.complexity.score
            );
            Self {
                max_iterations: 32,
                max_depth: 5,
                beam_width: 5,
            }
        }
    }
}

/// Priority queue entry for the guided search beam.
#[derive(Debug, Clone)]
struct QueueEntry {
    score: f32,
    node_id: String,
    depth: u32,
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.node_id == other.node_id
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher score first; tie-breaker prefers shallower depth.
        match other.score.total_cmp(&self.score) {
            Ordering::Equal => self.depth.cmp(&other.depth),
            ordering => ordering,
        }
    }
}

/// Guided best-first search strategy with optional beam width limitation.
pub struct GuidedSearchStrategy {
    /// Maximum depth to explore.
    max_depth: u32,
    /// Maximum number of node expansions.
    max_iterations: u32,
    /// Maximum number of candidates kept in the priority queue.
    beam_width: usize,
    /// Performance metrics.
    metrics: StrategyMetrics,
    /// Optional intelligent task decomposer.
    decomposer: Option<Arc<TaskDecomposer>>,
    /// Early termination configuration.
    early_termination_config: EarlyTerminationConfig,
    /// Cache for decomposition results keyed by task/depth context.
    decomposition_cache: RwLock<HashMap<String, Vec<SubTask>>>,
}

impl GuidedSearchStrategy {
    /// Create a new guided search strategy with default parameters.
    pub fn new() -> Self {
        Self {
            max_depth: 10,
            max_iterations: 50,
            beam_width: 5,
            metrics: StrategyMetrics::default(),
            decomposer: None,
            early_termination_config: EarlyTerminationConfig::default(),
            decomposition_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a strategy with explicit parameters.
    pub fn with_params(max_depth: u32, max_iterations: u32, beam_width: usize) -> Self {
        Self {
            max_depth,
            max_iterations,
            beam_width,
            metrics: StrategyMetrics::default(),
            decomposer: None,
            early_termination_config: EarlyTerminationConfig::default(),
            decomposition_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a strategy configured with an intelligent decomposer.
    pub fn with_decomposer(
        max_depth: u32,
        max_iterations: u32,
        beam_width: usize,
        decomposer: Arc<TaskDecomposer>,
    ) -> Self {
        Self {
            max_depth,
            max_iterations,
            beam_width,
            metrics: StrategyMetrics::default(),
            decomposer: Some(decomposer),
            early_termination_config: EarlyTerminationConfig::default(),
            decomposition_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a strategy from adaptive parameters (recommended path).
    pub fn with_adaptive_params(
        params: GuidedSearchParams,
        decomposer: Arc<TaskDecomposer>,
    ) -> Self {
        Self {
            max_depth: params.max_depth,
            max_iterations: params.max_iterations,
            beam_width: params.beam_width,
            metrics: StrategyMetrics::default(),
            decomposer: Some(decomposer),
            early_termination_config: EarlyTerminationConfig::default(),
            decomposition_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Evaluate whether continued exploration is worthwhile based on the
    /// amount of improvement achieved so far versus budget remaining.
    fn should_continue_exploration(
        &self,
        iteration: u32,
        best_confidence: f32,
        total_llm_calls: u32,
        context: &StrategyContext,
    ) -> bool {
        if iteration < 10 {
            return true;
        }

        let confidence_gained = best_confidence.max(0.01);
        let cost_per_confidence = total_llm_calls as f32 / confidence_gained;
        let remaining_potential = (1.0 - best_confidence).max(0.0) * 0.5;
        let estimated_cost = cost_per_confidence * remaining_potential;
        let remaining_budget = context.resource_budget.remaining_llm_calls() as f32;

        if estimated_cost > remaining_budget {
            info!(
                "[MAGICIAN-V2-STRATEGY] GuidedSearch: stopping due to poor cost-benefit \
                 (estimated {:.1} calls needed, {} remaining)",
                estimated_cost, remaining_budget
            );
            return false;
        }

        true
    }

    /// Trim the candidate queue to the configured beam width.
    fn trim_queue(&self, queue: &mut BinaryHeap<QueueEntry>) {
        if queue.len() <= self.beam_width {
            return;
        }

        let mut entries: Vec<_> = queue.drain().collect();
        entries.sort(); // Uses Ord implementation (highest score first after reverse)
        entries.truncate(self.beam_width);
        for entry in entries {
            queue.push(entry);
        }
    }

    /// Expand a node by decomposing it into subtasks or finding fallback steps.
    async fn expand_node(
        &self,
        node: &ExplorationNode,
        nodes: &mut HashMap<String, ExplorationNode>,
        context: &StrategyContext,
    ) -> Result<Vec<String>> {
        debug!(
            "[MAGICIAN-V2-STRATEGY] Expanding node '{}' at depth {}",
            node.task, node.depth
        );

        if node.depth >= self.max_depth {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Max depth {} reached, skipping expansion",
                self.max_depth
            );
            return Ok(vec![]);
        }

        // Use cached decomposition when available.
        let mut hasher = DefaultHasher::new();
        node.task_context.parent_context.hash(&mut hasher);
        for category in &node.task_context.relevant_categories {
            category.hash(&mut hasher);
        }
        for (key, value) in &node.task_context.parent_metadata {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        for (key, value) in &node.available_parameters {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        let context_hash = hasher.finish();
        let cache_key = format!("{}::{}::{}", node.task, node.depth, context_hash);

        let subtasks: Vec<SubTask> = if node.depth == 0
            && !context
                .query_analysis
                .dependencies
                .workflow_steps
                .is_empty()
        {
            info!(
                "[MAGICIAN-V2-STRATEGY] Using workflow_steps from analysis ({} steps)",
                context.query_analysis.dependencies.workflow_steps.len()
            );
            context
                .query_analysis
                .dependencies
                .workflow_steps
                .iter()
                .map(|step| SubTask {
                    description: step.clone(),
                    dependencies: vec![],
                    complexity: context.query_analysis.complexity.score,
                    priority: 1.0,
                    suggested_categories: context.query_analysis.categories.categories.clone(),
                    analysis: None,
                })
                .collect::<Vec<_>>()
        } else if let Some(ref decomposer) = self.decomposer {
            if let Some(cached) = self
                .decomposition_cache
                .read()
                .ok()
                .and_then(|cache| cache.get(&cache_key).cloned())
            {
                debug!(
                    "[MAGICIAN-V2-STRATEGY] Reusing cached decomposition for key '{}'",
                    cache_key
                );
                cached
            } else {
                let decomp_context = DecompositionContext::new_root_with_ids(
                    node.task.clone(),
                    context.resource_budget.clone(),
                    context.execution_context.clone(),
                    context.execution_id.clone(),
                    context.correlation_id.clone(),
                );

                match decomposer.decompose_task(&node.task, &decomp_context).await {
                    Ok(subtasks) => {
                        if let Ok(mut cache) = self.decomposition_cache.write() {
                            cache.insert(cache_key.clone(), subtasks.clone());
                        }
                        subtasks
                    },
                    Err(e) => {
                        warn!(
                            "[MAGICIAN-V2-STRATEGY] Decomposer failed for '{}': {}. Using fallback.",
                            node.task, e
                        );
                        self.fallback_decomposition(node, context)?
                    },
                }
            }
        } else {
            self.fallback_decomposition(node, context)?
        };

        if subtasks.is_empty() {
            debug!(
                "[MAGICIAN-V2-STRATEGY] No subtasks produced when expanding '{}'",
                node.task
            );
            return Ok(vec![]);
        }

        let mut new_child_ids: Vec<String> = Vec::new();
        for subtask in subtasks {
            let child_id = Uuid::new_v4().to_string();
            let parent_parameters = node.available_parameters.clone();
            let parent_context = node.task_context.clone();
            let child_depth = node.depth + 1;

            let mut child_node = ExplorationNode::new_with_parameters(
                child_id.clone(),
                subtask.description.clone(),
                Some(node.id.clone()),
                child_depth,
                parent_parameters,
                subtask.analysis.as_ref(),
                parent_context,
            );

            child_node.priority = subtask.priority;
            child_node.categories = subtask.suggested_categories.clone();
            child_node.dependencies = subtask
                .dependencies
                .iter()
                .map(|&idx| {
                    if idx < new_child_ids.len() {
                        new_child_ids[idx].clone()
                    } else {
                        format!("dep_{}", idx)
                    }
                })
                .collect();

            nodes.insert(child_id.clone(), child_node);
            new_child_ids.push(child_id);
        }

        if let Some(parent) = nodes.get_mut(&node.id) {
            for child_id in &new_child_ids {
                parent.add_child(child_id.clone());
            }
        }

        info!(
            "[MAGICIAN-V2-STRATEGY] Expanded '{}' into {} children",
            node.task,
            new_child_ids.len()
        );
        Ok(new_child_ids)
    }

    /// Evaluate a node by matching tools and estimating confidence.
    async fn simulate_node(
        &self,
        node: &mut ExplorationNode,
        context: &StrategyContext,
    ) -> Result<f32> {
        debug!(
            "[MAGICIAN-V2-STRATEGY] Evaluating node '{}' for tool match",
            node.task
        );

        let parent_task = node.task_context.get_parent_task().cloned();
        let parent_tool = node
            .tool_match
            .as_ref()
            .and_then(|tm| tm.primary_match.as_ref().map(|pm| pm.tool_name.clone()));

        let tool_match_with_provenance =
            crate::magician_v2::strategy::tool_match_helper::match_tool_with_v2(
                context,
                &node.task,
                parent_task,
                parent_tool,
            )
            .await?;

        let tool_match_result = tool_match_with_provenance.result;
        let match_providing_agent_id = tool_match_with_provenance.providing_agent_id;

        node.tool_match = Some(tool_match_result.clone());
        context.record_llm_usage(None);

        if !node.extracted_parameters.is_empty() {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Mapping {} entities to tool parameters",
                node.extracted_parameters.len()
            );

            let mut entity_mapper = crate::magician_v2::strategy::entity_mapper::EntityMapper::new(
                context.llm_service.clone(),
                context.prompt_manager.clone(),
            );
            if let Some((telemetry, attribution)) =
                context.operation_llm_telemetry_context("entity_mapping")
            {
                entity_mapper = entity_mapper.with_llm_telemetry(telemetry, attribution);
            }

            let task_text = node.task.clone();
            if let Err(e) = node
                .map_entities_to_tool_parameters(&entity_mapper, &task_text)
                .await
            {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Entity mapping failed: {}, using extracted parameters",
                    e
                );
            } else {
                debug!(
                    "[MAGICIAN-V2-STRATEGY] Entity mapping completed, {} parameters available",
                    node.available_parameters.len()
                );
            }
        }

        let (parameter_compatibility, missing_required) = if let Some(ref primary_match) =
            tool_match_result.primary_match
        {
            let tool_schema = &primary_match.tool_metadata.input_schema;
            let compatibility =
                crate::magician_v2::strategy::parameter_utils::calculate_parameter_compatibility(
                    tool_schema,
                    &node.available_parameters,
                );
            let required_params =
                crate::magician_v2::strategy::parameter_utils::extract_required_params(tool_schema);
            let missing = required_params
                .into_iter()
                .filter(|param| !node.available_parameters.contains_key(param))
                .collect::<Vec<_>>();
            (compatibility, missing)
        } else {
            (0.0, Vec::new())
        };

        let composite_score =
            (tool_match_result.match_confidence * 0.5) + (parameter_compatibility * 0.5);

        node.confidence = composite_score;

        let has_good_match = tool_match_result.match_confidence >= CONFIDENCE_THRESHOLD
            && parameter_compatibility >= CONFIDENCE_THRESHOLD
            && tool_match_result.primary_match.is_some();

        let is_multi_step_root = node.depth == 0
            && node
                .analysis
                .as_ref()
                .map(|a| a.dependencies.is_multi_step && !a.dependencies.workflow_steps.is_empty())
                .unwrap_or(false);

        if has_good_match && !is_multi_step_root {
            node.is_terminal = true;
        }

        if is_multi_step_root {
            info!("[MAGICIAN-V2-STRATEGY] Multi-step root detected; keeping node expandable");
        }

        let mut metadata = HashMap::new();
        metadata.insert(
            "match_confidence".to_string(),
            format!("{:.3}", tool_match_result.match_confidence),
        );
        metadata.insert(
            "parameter_compatibility".to_string(),
            format!("{:.3}", parameter_compatibility),
        );
        metadata.insert("depth".to_string(), node.depth.to_string());
        metadata.insert(
            "available_parameters".to_string(),
            node.available_parameters.len().to_string(),
        );
        if !missing_required.is_empty() {
            metadata.insert("missing_required".to_string(), missing_required.join(", "));
        }

        // Use providing_agent_id from the tool match provenance (populated by
        // V2 Tool Matcher from ToolInfo::providing_agent_id). When the V2 matcher
        // does not supply provenance, fall back to the delegate_tool_catalog on the
        // strategy context so delegation routing still works for V1 tool matching.
        let providing_agent_id = match_providing_agent_id.or_else(|| {
            let tool_name = tool_match_result
                .primary_match
                .as_ref()
                .map(|pm| pm.tool_name.as_str())?;
            context.providing_agent_id_for_tool(tool_name)
        });
        let step_hash = providing_agent_id.as_ref().and_then(|aid| {
            tool_match_result
                .primary_match
                .as_ref()
                .map(|pm| PlanStep::compute_hash(aid, &pm.tool_name))
        });

        node.partial_plan = Some(PlanStep {
            id: node.id.clone(),
            task: node.task.clone(),
            tool: tool_match_result
                .primary_match
                .as_ref()
                .map(|pm| pm.tool_name.clone()),
            parameters: node
                .available_parameters
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
            expected_outputs: Vec::new(),
            confidence: composite_score,
            metadata,
            timeout_override_secs: None,
            providing_agent_id,
            step_hash,
            ..Default::default()
        });

        node.visits = node.visits.saturating_add(1);
        node.value += composite_score;

        Ok(composite_score)
    }

    /// Fallback decomposition when intelligent decomposer fails.
    fn fallback_decomposition(
        &self,
        node: &ExplorationNode,
        context: &StrategyContext,
    ) -> Result<Vec<SubTask>> {
        if node.depth == 0 {
            warn!(
                "[MAGICIAN-V2-STRATEGY] Fallback decomposition for root '{}'",
                node.task
            );
            Ok(context
                .query_analysis
                .dependencies
                .workflow_steps
                .iter()
                .map(|step| SubTask {
                    description: step.clone(),
                    dependencies: vec![],
                    complexity: context.query_analysis.complexity.score,
                    priority: 1.0,
                    suggested_categories: context.query_analysis.categories.categories.clone(),
                    analysis: None,
                })
                .collect())
        } else {
            warn!(
                "[MAGICIAN-V2-STRATEGY] Using heuristic fallback decomposition for '{}'",
                node.task
            );
            self.create_fallback_subtasks(&node.task, &context.query_analysis)
        }
    }

    /// Simple heuristic to split a sentence into subtasks.
    fn create_fallback_subtasks(
        &self,
        task: &str,
        analysis: &crate::magician_v2::query_analysis::UnifiedQueryAnalysis,
    ) -> Result<Vec<SubTask>> {
        let delimiters = [",", " and ", " then ", " followed by ", " after "];
        for delimiter in &delimiters {
            if task.contains(delimiter) {
                let parts: Vec<_> = task
                    .split(delimiter)
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();

                if parts.len() > 1 {
                    return Ok(parts
                        .into_iter()
                        .map(|part| SubTask {
                            description: part.to_string(),
                            dependencies: vec![],
                            complexity: analysis.complexity.score,
                            priority: 1.0,
                            suggested_categories: analysis.categories.categories.clone(),
                            analysis: None,
                        })
                        .collect());
                }
            }
        }

        Ok(vec![SubTask {
            description: task.to_string(),
            dependencies: vec![],
            complexity: analysis.complexity.score,
            priority: 1.0,
            suggested_categories: analysis.categories.categories.clone(),
            analysis: None,
        }])
    }

    /// Find the best path (root → leaf) in the explored graph.
    fn find_best_path(
        &self,
        nodes: &HashMap<String, ExplorationNode>,
        root_id: &str,
    ) -> (Vec<String>, f32) {
        let mut best_path = vec![root_id.to_string()];
        let mut best_confidence = 0.0;
        self.find_best_path_recursive(
            nodes,
            root_id,
            &mut vec![root_id.to_string()],
            &mut best_path,
            &mut best_confidence,
        );
        (best_path, best_confidence)
    }

    fn find_best_path_recursive(
        &self,
        nodes: &HashMap<String, ExplorationNode>,
        current_id: &str,
        current_path: &mut Vec<String>,
        best_path: &mut Vec<String>,
        best_confidence: &mut f32,
    ) {
        if let Some(node) = nodes.get(current_id) {
            if node.confidence > *best_confidence {
                *best_confidence = node.confidence;
                *best_path = current_path.clone();
            }

            if node.is_terminal && node.confidence >= CONFIDENCE_THRESHOLD {
                return;
            }

            for child_id in &node.children {
                current_path.push(child_id.clone());
                self.find_best_path_recursive(
                    nodes,
                    child_id,
                    current_path,
                    best_path,
                    best_confidence,
                );
                current_path.pop();
            }
        }
    }

    /// Save incremental exploration result for observability.
    async fn save_incremental_result(
        &self,
        context: &StrategyContext,
        _all_nodes: &HashMap<String, ExplorationNode>,
        _root_id: &str,
        _best_path: &[String],
        _best_confidence: f32,
        _start_time: std::time::Instant,
        _llm_calls_made: u32,
        iterations: u32,
        _max_depth_reached: u32,
    ) {
        if let (Some(_store), Some(_execution_id), Some(_turn_id)) = (
            &context.conversation_store,
            &context.execution_id,
            &context.turn_id,
        ) {
            debug!(
                "[MAGICIAN-V2-STRATEGY] GuidedSearch: incremental result available at iteration {} (persistence disabled)",
                iterations
            );
        }
    }
}

#[async_trait]
impl ExplorationStrategy for GuidedSearchStrategy {
    async fn explore(
        &mut self,
        context: &StrategyContext,
        query: &str,
    ) -> Result<ExplorationResult> {
        let start_time = std::time::Instant::now();
        info!(
            "[MAGICIAN-V2-STRATEGY] GuidedSearch: exploring plan space for '{}'",
            query
        );
        debug!(
            "[MAGICIAN-V2-STRATEGY] GuidedSearch parameters: max_depth={}, max_iterations={}, \
             beam_width={}",
            self.max_depth, self.max_iterations, self.beam_width
        );

        let root_id = Uuid::new_v4().to_string();
        let root_context = TaskContext::new_root(query.to_string());
        let mut root_node = ExplorationNode::new_with_parameters(
            root_id.clone(),
            query.to_string(),
            None,
            0,
            HashMap::new(),
            Some(&context.query_analysis),
            root_context,
        );
        root_node.categories = context.suggested_categories.clone();

        let mut all_nodes = HashMap::new();
        all_nodes.insert(root_id.clone(), root_node.clone());

        // Simulate root node to seed the queue.
        if let Some(root) = all_nodes.get_mut(&root_id) {
            let score = self.simulate_node(root, context).await?;
            root.visits = 1;
            root.value = score;
        }

        let mut queue = BinaryHeap::new();
        if let Some(root) = all_nodes.get(&root_id) {
            queue.push(QueueEntry {
                score: root.confidence,
                node_id: root.id.clone(),
                depth: root.depth,
            });
        }

        let mut iterations = 0;
        let mut total_llm_calls = 1; // root simulation
        let mut nodes_explored = 1;
        let mut max_depth_reached = 0;

        let mut best_confidence = all_nodes.get(&root_id).map(|n| n.confidence).unwrap_or(0.0);
        let mut iterations_since_improvement = 0;
        let mut diminishing_detector = DiminishingReturnsDetector::new(10);
        let mut termination_reason: Option<String> = None;

        info!(
            "[MAGICIAN-V2-STRATEGY] GuidedSearch starting with budget: {} calls, {}ms remaining",
            context.resource_budget.remaining_llm_calls(),
            context.resource_budget.remaining_time_ms()
        );

        let mut best_path = vec![root_id.clone()];

        while iterations < self.max_iterations
            && context.resource_budget.can_continue()
            && !queue.is_empty()
        {
            iterations += 1;

            let QueueEntry { node_id, .. } = queue.pop().expect("queue not empty");

            let should_expand = if let Some(node) = all_nodes.get(&node_id) {
                !node.is_terminal && node.children.is_empty() && node.depth < self.max_depth
            } else {
                false
            };

            if should_expand {
                let node_clone = all_nodes.get(&node_id).unwrap().clone();
                let child_ids = self
                    .expand_node(&node_clone, &mut all_nodes, context)
                    .await?;

                if !child_ids.is_empty() {
                    nodes_explored += child_ids.len();
                    max_depth_reached = max_depth_reached.max(node_clone.depth + 1);
                }

                for child_id in child_ids {
                    if let Some(child) = all_nodes.get_mut(&child_id) {
                        let value = self.simulate_node(child, context).await?;
                        total_llm_calls += 1;

                        queue.push(QueueEntry {
                            score: value,
                            node_id: child.id.clone(),
                            depth: child.depth,
                        });

                        if value > best_confidence {
                            best_confidence = value;
                            iterations_since_improvement = 0;
                            best_path = self.find_best_path(&all_nodes, &root_id).0;
                        }
                    }
                }

                self.trim_queue(&mut queue);
            } else if let Some(node) = all_nodes.get(&node_id) {
                if node.is_terminal && node.confidence > best_confidence {
                    best_confidence = node.confidence;
                    iterations_since_improvement = 0;
                    best_path = self.find_best_path(&all_nodes, &root_id).0;
                }
            }

            iterations_since_improvement += 1;
            diminishing_detector.record(best_confidence);

            if iterations_since_improvement
                >= self.early_termination_config.acceptable_plateau_iterations
            {
                info!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch: terminating due to plateau (confidence {:.3})",
                    best_confidence
                );
                termination_reason = Some(format!(
                    "Early termination: plateau after {} iterations",
                    iterations_since_improvement
                ));
                break;
            }

            if best_confidence >= self.early_termination_config.good_confidence
                && iterations_since_improvement
                    >= self.early_termination_config.good_plateau_iterations
            {
                info!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch: terminating after hitting good confidence {:.3}",
                    best_confidence
                );
                termination_reason = Some(format!(
                    "Early termination: good confidence {:.3}",
                    best_confidence
                ));
                break;
            }

            if iterations >= 20
                && best_confidence
                    >= self
                        .early_termination_config
                        .diminishing_returns_min_confidence
                && diminishing_detector.has_diminishing_returns(
                    self.early_termination_config.diminishing_returns_threshold,
                )
            {
                let improvement_rate = diminishing_detector.improvement_rate();
                info!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch: stopping due to diminishing returns ({:.4}/iter, conf {:.3})",
                    improvement_rate, best_confidence
                );
                termination_reason = Some(format!(
                    "Early termination: diminishing returns ({:.4}/iter)",
                    improvement_rate
                ));
                break;
            }

            if iterations >= 10
                && !self.should_continue_exploration(
                    iterations,
                    best_confidence,
                    total_llm_calls,
                    context,
                )
            {
                termination_reason = Some("Early termination: cost-benefit threshold".to_string());
                break;
            }

            if iterations % 5 == 0 {
                debug!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch progress: iter={}, nodes={}, depth={}, \
                     llm_calls={}, best_conf={:.3}, queue_size={}",
                    iterations,
                    nodes_explored,
                    max_depth_reached,
                    total_llm_calls,
                    best_confidence,
                    queue.len()
                );
            }

            self.save_incremental_result(
                context,
                &all_nodes,
                &root_id,
                &best_path,
                best_confidence,
                start_time,
                total_llm_calls,
                iterations,
                max_depth_reached,
            )
            .await;
        }

        let execution_time = start_time.elapsed();
        let (final_best_path, final_confidence) = self.find_best_path(&all_nodes, &root_id);
        best_path = final_best_path;

        self.metrics.nodes_explored = nodes_explored as u32;
        self.metrics.llm_calls_made = total_llm_calls;
        self.metrics.average_confidence = final_confidence;
        self.metrics.exploration_depth = max_depth_reached;
        self.metrics.time_per_iteration_ms = if iterations > 0 {
            execution_time.as_millis() as f32 / iterations as f32
        } else {
            0.0
        };

        let completed = iterations < self.max_iterations && context.resource_budget.can_continue();
        let final_termination_reason = termination_reason.or_else(|| {
            if !completed {
                if iterations >= self.max_iterations {
                    Some("Maximum iterations reached".to_string())
                } else if !context.resource_budget.can_continue() {
                    Some("Resource budget exhausted".to_string())
                } else {
                    Some("Exploration complete".to_string())
                }
            } else {
                Some("Exploration complete".to_string())
            }
        });

        info!(
            "[MAGICIAN-V2-STRATEGY] GuidedSearch complete: iterations={}, nodes={}, conf={:.3}, \
             time={}ms",
            iterations,
            nodes_explored,
            final_confidence,
            execution_time.as_millis()
        );

        let root_node = all_nodes.get(&root_id).unwrap().clone();
        let parameter_statistics =
            ParameterStatistics::from_exploration(&root_node, &all_nodes, &best_path);

        let mut plan_graph = plan_builder::build_plan_graph(&all_nodes, &root_id, final_confidence);
        if let Some(mut plan) = plan_graph {
            // Use merged_agent_tools() to tag delegate tools with providing_agent_id.
            let tool_catalog = match context.merged_agent_tools().await {
                Ok(catalog) => catalog,
                Err(e) => {
                    warn!(
                        "[MAGICIAN-V2-STRATEGY] Failed to load tool catalog for validation: {}",
                        e
                    );
                    Vec::new()
                },
            };

            if !tool_catalog.is_empty() {
                match super::plan_validator::validate_plan(&plan, &tool_catalog, false) {
                    Ok(mut holes) => {
                        plan.unresolved_inputs.append(&mut holes);
                        plan_graph = Some(plan);
                    },
                    Err(e) => {
                        warn!(
                            "[MAGICIAN-V2-STRATEGY] Plan validation failed, discarding plan: {}",
                            e
                        );
                        plan_graph = None;
                    },
                }
            } else {
                plan_graph = Some(plan);
            }
        }

        Ok(ExplorationResult {
            root_node,
            all_nodes,
            best_path,
            confidence: final_confidence,
            plan: plan_graph,
            resources_consumed: ResourceUsage {
                llm_calls: total_llm_calls,
                time_ms: execution_time.as_millis() as u64,
                tokens: total_llm_calls * 500,
            },
            strategy_metadata: StrategyMetadata {
                strategy_type: StrategyType::GuidedSearch,
                iterations,
                nodes_explored: nodes_explored as u32,
                max_depth: max_depth_reached,
                completed,
                termination_reason: final_termination_reason,
            },
            parameter_statistics,
        })
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::GuidedSearch
    }

    fn can_continue(&self, budget: &ResourceBudget) -> bool {
        budget.can_continue() && budget.allows_guided_search()
    }
}

#[async_trait]
impl FailureRecovery for GuidedSearchStrategy {
    fn handle_failure(&self, failure: StrategyFailure, _context: &StrategyContext) -> NextAction {
        match failure {
            StrategyFailure::NoToolsFound => {
                warn!("[MAGICIAN-V2-STRATEGY] GuidedSearch: no tools found, escalating");
                NextAction::Escalate(StrategyType::AtomicComposition)
            },
            StrategyFailure::LowConfidence(conf) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch low confidence ({:.2}), escalating",
                    conf
                );
                NextAction::Escalate(StrategyType::AtomicComposition)
            },
            StrategyFailure::TimeoutExceeded => {
                warn!("[MAGICIAN-V2-STRATEGY] GuidedSearch timeout, escalating");
                NextAction::Escalate(StrategyType::AtomicComposition)
            },
            StrategyFailure::LLMBudgetExceeded => {
                warn!("[MAGICIAN-V2-STRATEGY] GuidedSearch LLM budget exceeded, escalating");
                NextAction::Escalate(StrategyType::AtomicComposition)
            },
            StrategyFailure::DecompositionFailed => {
                info!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch decomposition failed, escalating to atomic"
                );
                NextAction::Escalate(StrategyType::AtomicComposition)
            },
            StrategyFailure::InternalError(msg) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] GuidedSearch internal error: {}",
                    msg
                );
                NextAction::Abort(format!("GuidedSearch internal error: {}", msg))
            },
        }
    }

    fn should_abort(&self, _context: &StrategyContext, current_result: &ExplorationResult) -> bool {
        if current_result.confidence >= 0.95 && current_result.root_node.is_terminal {
            return true;
        }

        if current_result.resources_consumed.llm_calls > 20 && current_result.confidence < 0.3 {
            return true;
        }

        if current_result.strategy_metadata.max_depth >= self.max_depth
            && current_result.confidence < CONFIDENCE_THRESHOLD
        {
            return true;
        }

        false
    }
}

impl StrategyIntrospection for GuidedSearchStrategy {
    fn get_debug_info(&self) -> serde_json::Value {
        serde_json::json!({
            "strategy": "GuidedSearch",
            "description": "LLM-guided best-first search with optional beam width",
            "parameters": {
                "max_depth": self.max_depth,
                "max_iterations": self.max_iterations,
                "beam_width": self.beam_width
            },
            "features": [
                "Deterministic best-first expansion",
                "LLM-based tool matching for scoring",
                "Early termination heuristics",
                "Decomposition caching",
                "Incremental result persistence"
            ],
            "metrics": {
                "nodes_explored": self.metrics.nodes_explored,
                "llm_calls_made": self.metrics.llm_calls_made,
                "average_confidence": self.metrics.average_confidence,
                "exploration_depth": self.metrics.exploration_depth,
                "time_per_iteration_ms": self.metrics.time_per_iteration_ms
            },
            "status": "fully_implemented"
        })
    }

    fn get_metrics(&self) -> StrategyMetrics {
        self.metrics.clone()
    }
}

impl Default for GuidedSearchStrategy {
    fn default() -> Self {
        Self::new()
    }
}
