//! Pipeline agent wrapper for the planner stage.
//!
//! The [`PlannerAgent`] is the CONTRACT PRODUCER for `PlanGraph` v1.2. It
//! reads upstream artifacts (`QueryAnalysis`, `ElicitationResult`), produces
//! a minimal `PlanGraph`, and enriches it with v1.1/v1.2 extension fields:
//!
//! - `depends_on` per step (derived from the edge list)
//! - `success_criteria` per step (from metadata or task fallback)
//! - `planning_metadata` on the graph (elicitation context)
//! - `response_contract` on the graph (v1.2: terminal step + response kind)
//! - `role` on the terminal step (v1.2: `StepRole::Terminal`)
//!
//! The heavy-weight strategy execution (e.g. `AdaptiveStrategySelector`) is
//! NOT performed here. This agent focuses on the enrichment contract; a
//! later ticket will wire in real strategy invocation.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::Utc;
use serde_json;
use tracing::{debug, info, warn};

use crate::magician_v2::{
    pipeline::{
        agent::{
            PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext,
            AGENT_ID_PLANNER,
        },
        artifact::{AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION},
    },
    query_analysis::unified_analyzer::{TaskClarity, UnifiedQueryAnalysis},
    slot_graph::{types::SlotRecord, ClarifiedTask},
    strategy::{
        plan::{
            InputSource, ParameterProvenance, PlanGraph, PlanStep, PlanningMetadata,
            ResponseContract, ResponseKind, StepRole,
        },
        StrategyType,
    },
};

// ---------------------------------------------------------------------------
// PlannerBackend trait
// ---------------------------------------------------------------------------

/// Abstraction over the heavy-weight strategy-selection backend.
///
/// [`MagicianPlannerBackend`] in `v2_orchestrator.rs` implements this by
/// calling `plan_from_analysis()` → `execute_strategy_with_retry()`.
#[async_trait]
pub trait PlannerBackend: Send + Sync {
    /// Produce a [`PlanGraph`] and the selected [`StrategyType`] for the
    /// given query and upstream artifacts.
    async fn plan(
        &self,
        query: &str,
        analysis: serde_json::Value,
        clarified_task: Option<ClarifiedTask>,
        slot_records: Vec<SlotRecord>,
        execution_id: Option<String>,
        correlation_id: Option<String>,
    ) -> Result<(PlanGraph, StrategyType), String>;
}

// ---------------------------------------------------------------------------
// Enrichment functions (pure, tested independently)
// ---------------------------------------------------------------------------

/// Derive `depends_on` for each step from the edge list.
///
/// For every edge `from -> to`, the `to` step gains `from` in its
/// `depends_on` vector. Steps with no inbound edges keep an empty vector.
pub fn enrich_depends_on(graph: &mut PlanGraph) {
    let mut deps_map: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &graph.edges {
        deps_map
            .entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }
    for step in &mut graph.steps {
        if let Some(mut deps) = deps_map.remove(&step.id) {
            // M-17: sort for deterministic order across different backend edge orderings.
            deps.sort();
            step.depends_on = deps;
        }
    }
}

/// Extract `success_criteria` from `metadata["observation"]` or fall back to
/// the step's `task` field.
pub fn enrich_success_criteria(graph: &mut PlanGraph) {
    for step in &mut graph.steps {
        if step.success_criteria.is_none() {
            step.success_criteria = Some(
                step.metadata
                    .get("observation")
                    .cloned()
                    .unwrap_or_else(|| step.task.clone()),
            );
        }
    }
}

/// Populate `planning_metadata` from available context values.
pub fn enrich_planning_metadata(
    graph: &mut PlanGraph,
    clarified_task: Option<&str>,
    elicitation_rounds: u32,
    slots_resolved: u32,
    slot_confidence: f32,
    upstream_llm_calls: u32,
) {
    graph.planning_metadata = PlanningMetadata {
        clarified_task: clarified_task.map(String::from),
        constraints: vec![],
        objectives: vec![],
        elicitation_rounds,
        slots_resolved,
        slot_confidence,
        upstream_llm_calls,
    };
}

// ---------------------------------------------------------------------------
// v1.2 enrichment: response contract & terminal step
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// C-05: cycle detection
// ---------------------------------------------------------------------------

/// Check that the `depends_on` graph is acyclic using Kahn's topological sort.
///
/// Returns `Err(PipelineAgentError::ExecutionFailed(...))` naming the cyclic
/// step IDs when a cycle is detected.  With the C-04 fix in the router, cyclic
/// dependencies would cause neither step to ever become eligible, silently
/// exhausting `max_iterations` with no informative error — this catches it early.
pub fn check_no_cycles(graph: &PlanGraph) -> Result<(), PipelineAgentError> {
    use std::collections::VecDeque;

    let n = graph.steps.len();
    if n == 0 {
        return Ok(());
    }

    let id_to_idx: HashMap<&str, usize> = graph
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

    for (i, step) in graph.steps.iter().enumerate() {
        for dep_id in &step.depends_on {
            if let Some(&j) = id_to_idx.get(dep_id.as_str()) {
                // Edge: j → i (j must complete before i can run)
                adj[j].push(i);
                in_degree[i] += 1;
            }
            // Unknown dep IDs are silently ignored; they'll cause execution stalls
            // but are not themselves cycles.
        }
    }

    // Kahn's: queue steps with no prerequisites.
    let mut queue: VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();

    let mut sorted = 0;
    while let Some(u) = queue.pop_front() {
        sorted += 1;
        for &v in &adj[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    if sorted < n {
        let cyclic: Vec<&str> = graph
            .steps
            .iter()
            .enumerate()
            .filter(|&(i, _)| in_degree[i] > 0)
            .map(|(_, s)| s.id.as_str())
            .collect();
        return Err(PipelineAgentError::ExecutionFailed(format!(
            "cycle detected in PlanGraph involving steps: {cyclic:?}"
        )));
    }

    Ok(())
}

/// Identify the terminal step in the plan and set the `response_contract`.
///
/// Detection priority (I-10):
/// 1. **Primary**: the step whose `role == Some(StepRole::Terminal)` (set by backend).
/// 2. **Fallback heuristic**: the LAST step whose `expected_outputs` contain one of the
///    hardcoded terminal keywords (`"confirmation"`, `"report"`, etc.).
///    A `debug!` is logged when falling back so mismatches are detectable.
///
/// The identified step's `role` is set (or confirmed) as `StepRole::Terminal`; all other
/// steps have their role cleared to guarantee at-most-one terminal marker.
pub fn enrich_response_contract(graph: &mut PlanGraph, context: &PipelineContext) {
    let terminal_keywords = ["confirmation", "booking_confirmation", "report", "answer"];

    // I-10: primary signal — backend-declared StepRole::Terminal.
    let terminal_step_idx: Option<usize> = graph
        .steps
        .iter()
        .enumerate()
        .find(|(_, step)| step.role == Some(StepRole::Terminal))
        .map(|(idx, _)| idx)
        .or_else(|| {
            // Fallback: keyword heuristic on expected_outputs.
            let idx = graph
                .steps
                .iter()
                .enumerate()
                .rev()
                .find(|(_, step)| {
                    step.expected_outputs.iter().any(|output| {
                        let lower = output.to_lowercase();
                        terminal_keywords.iter().any(|kw| lower.contains(kw))
                    })
                })
                .map(|(idx, _)| idx);
            if idx.is_some() {
                tracing::debug!(
                    "[PIPELINE:planner] enrich_response_contract: no StepRole::Terminal set by backend; \
                     falling back to keyword heuristic"
                );
            }
            idx
        });

    let (kind, terminal_step_id) = if let Some(idx) = terminal_step_idx {
        let step = &graph.steps[idx];
        let kind = classify_response_kind(step, context);
        let step_id = step.id.clone();
        (kind, Some(step_id))
    } else if context.agent_id.is_some() {
        (ResponseKind::Silent, None)
    } else {
        // No terminal step found and no agent — default to Action with last step as terminal.
        // Action kind requires terminal_step; fall back to the last step in the graph.
        let last_step_id = graph.steps.last().map(|s| s.id.clone());
        (ResponseKind::Action, last_step_id)
    };

    graph.response_contract = Some(ResponseContract {
        kind,
        terminal_step: terminal_step_id,
    });

    // Mark only the terminal step with role
    if let Some(idx) = terminal_step_idx {
        // Clear any previously set terminal roles (ensure at-most-one)
        for step in &mut graph.steps {
            step.role = None;
        }
        graph.steps[idx].role = Some(StepRole::Terminal);
    }
}

/// Classify the [`ResponseKind`] for a terminal step.
///
/// Priority order (highest to lowest):
/// 1. `SpawnTask` — tool name contains "spawn" or "sub_goal".
/// 2. `Action`   — expected outputs contain "confirmation" or "booking_confirmation".
/// 3. `Text`     — task description contains text-indicating keywords.
/// 4. `Silent`   — agent context with no other indicator.
/// 5. `Action`   — default fallback.
fn classify_response_kind(step: &PlanStep, context: &PipelineContext) -> ResponseKind {
    // P2-6: Check tool for spawn task FIRST — a spawning step must not be
    // mis-classified as Action even when expected_outputs mention "confirmation".
    if let Some(ref tool) = step.tool {
        let tool_lower = tool.to_lowercase();
        if tool_lower.contains("spawn") || tool_lower.contains("sub_goal") {
            return ResponseKind::SpawnTask;
        }
    }

    // Check expected_outputs for action-indicating keywords.
    let has_confirmation = step.expected_outputs.iter().any(|o| {
        let lower = o.to_lowercase();
        lower.contains("confirmation") || lower.contains("booking_confirmation")
    });

    if has_confirmation {
        return ResponseKind::Action;
    }

    // Check task for text-indicating keywords.
    let task_lower = step.task.to_lowercase();
    let text_keywords = ["summarize", "analyze", "report", "compare", "answer"];
    if text_keywords.iter().any(|kw| task_lower.contains(kw)) {
        return ResponseKind::Text;
    }

    // If agent context with no other indicator.
    if context.agent_id.is_some() {
        return ResponseKind::Silent;
    }

    ResponseKind::Action
}

// ---------------------------------------------------------------------------
// D-05: parameter provenance enrichment
// ---------------------------------------------------------------------------

/// Populate `parameter_provenance` for each step's parameters.
///
/// For each parameter in a step, looks for a matching SlotRecord (by string
/// value). If found, sets `source = InputSource::Discovery` and records the
/// slot_id. Otherwise falls back to `InputSource::Planner`. Existing entries
/// are never overwritten.
pub fn enrich_parameter_provenance(graph: &mut PlanGraph, slot_records: &[SlotRecord]) {
    if graph.steps.is_empty() {
        return;
    }

    // Build value → slot_id lookup (first match wins).
    let mut value_to_slot_id: HashMap<String, String> = HashMap::new();
    for record in slot_records {
        if let Some(s) = record.value.as_str() {
            value_to_slot_id
                .entry(s.to_string())
                .or_insert_with(|| record.id.clone());
        }
    }

    for step in &mut graph.steps {
        let step_conf = step.confidence;
        let params: Vec<String> = step.parameters.keys().cloned().collect();
        for param_name in params {
            if step.parameter_provenance.contains_key(&param_name) {
                continue; // preserve existing entries
            }
            let slot_id = step
                .parameters
                .get(&param_name)
                .and_then(|v| v.as_str())
                .and_then(|s| value_to_slot_id.get(s))
                .cloned();

            let (source, confidence) = if slot_id.is_some() {
                (InputSource::Discovery, step_conf.min(0.9))
            } else {
                (InputSource::Planner, step_conf)
            };

            step.parameter_provenance.insert(
                param_name,
                ParameterProvenance {
                    source,
                    confidence: Some(confidence), // M-14: wrap in Some()
                    slot_id,
                    method: Some("pipeline:enrich_parameter_provenance".to_string()),
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// PlannerAgent
// ---------------------------------------------------------------------------

/// Pipeline agent that produces enriched `PlanGraph` v1.2 artifacts.
///
pub struct PlannerAgent {
    backend: Arc<dyn PlannerBackend>,
}

impl PlannerAgent {
    /// Create a planner agent backed by the given [`PlannerBackend`].
    pub fn with_backend(backend: Arc<dyn PlannerBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl PipelineAgent for PlannerAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_PLANNER
    }

    fn required_inputs(&self) -> Vec<ArtifactType> {
        // All three inputs (QueryAnalysis, ElicitationResult, ClarifiedTask) are consumed
        // via .ok() / .unwrap_or_default(). The planner degrades gracefully when any are
        // absent (D-06), so none are truly required for dispatch.
        vec![]
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::PlanGraph]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        // ---- Read upstream artifacts ----
        let elicitation = store.latest_of_type(&ArtifactType::ElicitationResult);

        // Extract clarified task (full struct) from a ClarifiedTask artifact.
        let clarified_task: Option<ClarifiedTask> = store
            .latest_of_type(&ArtifactType::ClarifiedTask)
            .and_then(|a| a.deserialize_content::<ClarifiedTask>().ok());

        // Convenience: task text for enrichment/metadata.
        let clarified_task_text: Option<String> =
            clarified_task.as_ref().map(|ct| ct.clarified_task.clone());

        // I-08: use the accurate elicitation_rounds counter from PipelineContext.
        // The orchestrator increments this each time system:elicitor completes, so it
        // represents actual rounds, not just the question count in the last round.
        let elicitation_rounds: u32 = context.elicitation_rounds;
        let _ = elicitation; // suppress unused warning; elicitation var is still used below for slot_records

        // M-10: extract slot_records once and reuse for backend call and provenance enrichment.
        let slot_records: Vec<SlotRecord> = store
            .latest_of_type(&ArtifactType::ElicitationResult)
            .and_then(|a| {
                a.content.get("slot_graph").and_then(|s| {
                    crate::magician_v2::json_traversal::deserialize_json_bounded::<Vec<SlotRecord>>(
                        s,
                        1_000_000,
                        crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
                    )
                    .map_err(|e| {
                        warn!("[PIPELINE:planner] slot_graph deserialization: {}", e);
                        e
                    })
                    .ok()
                })
            })
            .unwrap_or_default();

        let analysis_json = store
            .latest_of_type(&ArtifactType::QueryAnalysis)
            .map(|a| crate::magician_v2::json_traversal::clone_json_iteratively(&a.content))
            .unwrap_or(serde_json::Value::Null);

        // D-06: use clarified_task query if present, else original query.
        let effective_query = clarified_task_text
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&context.query);

        debug!(
            "[PIPELINE:planner] Calling backend for query='{}' (effective='{}')",
            context.query, effective_query
        );

        let mut graph = match self
            .backend
            .plan(
                effective_query,
                analysis_json,
                clarified_task.clone(),
                slot_records.clone(), // M-10: reuse extracted slot_records
                context.execution_id.clone(),
                context.correlation_id.clone(),
            )
            .await
        {
            Ok((plan, _strategy)) => {
                info!(
                    "[PIPELINE:planner] Backend PlanGraph: {} steps",
                    plan.steps.len()
                );
                plan
            },
            Err(e) => {
                return Err(PipelineAgentError::ServiceError(format!(
                    "PlannerBackend failed: {e}"
                )));
            },
        };

        let slots_resolved = slot_records.len() as u32;
        let slot_confidence: f32 = if slot_records.is_empty() {
            0.0_f32
        } else {
            slot_records
                .iter()
                .map(|r| r.confidence as f32)
                .sum::<f32>()
                / slots_resolved as f32
        };

        // ---- Enrich with v1.1 fields ----
        enrich_depends_on(&mut graph);
        // C-05: reject cyclic dependency graphs before they reach the router.
        check_no_cycles(&graph)?;
        enrich_success_criteria(&mut graph);
        enrich_planning_metadata(
            &mut graph,
            clarified_task_text.as_deref(),
            elicitation_rounds,
            slots_resolved,
            slot_confidence,
            context.llm_routing_calls,
        );

        enrich_parameter_provenance(&mut graph, &slot_records);

        // ---- Enrich with v1.2 fields ----
        enrich_response_contract(&mut graph, context);

        // ---- Phase C: classify step readiness for post-plan refinement ----
        // The staged planning pipeline always runs in full now. We still use
        // task_clarity as a signal for whether step-level readiness
        // classification should activate the refinement / JIT path.
        if let Some(qa_artifact) = store.latest_of_type(&ArtifactType::QueryAnalysis) {
            if let Ok(qa) = qa_artifact.deserialize_content::<UnifiedQueryAnalysis>() {
                if qa.task_clarity == TaskClarity::Plannable {
                    crate::magician_v2::strategy::plan::classify_readiness(&mut graph);
                    info!(
                        "[PIPELINE:planner] classified step readiness: {:?}",
                        graph
                            .steps
                            .iter()
                            .map(|s| (&s.id, &s.readiness))
                            .collect::<Vec<_>>()
                    );
                }
            }
        }

        // I-19: validate the plan contract before writing the artifact so
        // downstream agents never see a structurally invalid PlanGraph.
        if let Err(e) = graph.validate_contract() {
            return Err(PipelineAgentError::ServiceError(format!(
                "PlanGraph contract validation failed: {e}"
            )));
        }

        info!(
            "[PIPELINE:planner] Produced PlanGraph with {} steps, session_policy={:?}",
            graph.steps.len(),
            graph.session_policy
        );

        // ---- Store the artifact ----
        let artifact_id = uuid::Uuid::new_v4().to_string();
        let artifact = AgentArtifact {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::PlanGraph,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content: serde_json::to_value(&graph)
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        };
        store.put(artifact);

        Ok(PipelineAgentResult::Completed {
            artifact_ids: vec![artifact_id],
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::strategy::plan::{PlanEdge, PlanProvenance, SessionPolicy};

    /// Helper: create a PlanGraph with the given steps and edges.
    fn make_graph(steps: Vec<PlanStep>, edges: Vec<PlanEdge>) -> PlanGraph {
        PlanGraph {
            steps,
            edges,
            unresolved_inputs: vec![],
            confidence: 0.8,
            provenance: PlanProvenance::default(),
            session_policy: SessionPolicy::default(),
            planning_metadata: PlanningMetadata::default(),
            ..Default::default()
        }
    }

    fn make_step(id: &str, task: &str) -> PlanStep {
        PlanStep {
            id: id.to_string(),
            task: task.to_string(),
            ..Default::default()
        }
    }

    fn make_step_with_observation(id: &str, task: &str, observation: &str) -> PlanStep {
        let mut metadata = HashMap::new();
        metadata.insert("observation".to_string(), observation.to_string());
        PlanStep {
            id: id.to_string(),
            task: task.to_string(),
            metadata,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // enrich_depends_on
    // -----------------------------------------------------------------------

    #[test]
    fn planner_agent_depends_on_derived_from_edges() {
        let steps = vec![
            make_step("A", "Step A"),
            make_step("B", "Step B"),
            make_step("C", "Step C"),
        ];
        let edges = vec![
            PlanEdge {
                from: "A".to_string(),
                to: "B".to_string(),
                reason: "B needs A".to_string(),
            },
            PlanEdge {
                from: "A".to_string(),
                to: "C".to_string(),
                reason: "C needs A".to_string(),
            },
        ];
        let mut graph = make_graph(steps, edges);

        enrich_depends_on(&mut graph);

        assert!(
            graph.steps[0].depends_on.is_empty(),
            "A has no inbound edges"
        );
        assert_eq!(
            graph.steps[1].depends_on,
            vec!["A".to_string()],
            "B depends on A"
        );
        assert_eq!(
            graph.steps[2].depends_on,
            vec!["A".to_string()],
            "C depends on A"
        );
    }

    #[test]
    fn planner_agent_depends_on_empty_when_no_inbound() {
        let steps = vec![make_step("X", "Step X"), make_step("Y", "Step Y")];
        // Only X -> Y, so X has no inbound
        let edges = vec![PlanEdge {
            from: "X".to_string(),
            to: "Y".to_string(),
            reason: "sequential".to_string(),
        }];
        let mut graph = make_graph(steps, edges);

        enrich_depends_on(&mut graph);

        assert!(
            graph.steps[0].depends_on.is_empty(),
            "X has no inbound edges"
        );
        assert_eq!(graph.steps[1].depends_on, vec!["X".to_string()]);
    }

    #[test]
    fn planner_agent_depends_on_multiple_inbound() {
        let steps = vec![
            make_step("A", "Step A"),
            make_step("B", "Step B"),
            make_step("C", "Step C"),
        ];
        let edges = vec![
            PlanEdge {
                from: "A".to_string(),
                to: "C".to_string(),
                reason: "C needs A".to_string(),
            },
            PlanEdge {
                from: "B".to_string(),
                to: "C".to_string(),
                reason: "C needs B".to_string(),
            },
        ];
        let mut graph = make_graph(steps, edges);

        enrich_depends_on(&mut graph);

        assert!(graph.steps[0].depends_on.is_empty());
        assert!(graph.steps[1].depends_on.is_empty());
        let mut deps = graph.steps[2].depends_on.clone();
        deps.sort();
        assert_eq!(deps, vec!["A".to_string(), "B".to_string()]);
    }

    // -----------------------------------------------------------------------
    // enrich_success_criteria
    // -----------------------------------------------------------------------

    #[test]
    fn planner_agent_success_criteria_from_observation_metadata() {
        let steps = vec![make_step_with_observation(
            "s1",
            "Navigate to page",
            "Page loaded",
        )];
        let mut graph = make_graph(steps, vec![]);

        enrich_success_criteria(&mut graph);

        assert_eq!(
            graph.steps[0].success_criteria.as_deref(),
            Some("Page loaded")
        );
    }

    #[test]
    fn planner_agent_success_criteria_fallback_to_task() {
        let steps = vec![make_step("s1", "Click the submit button")];
        let mut graph = make_graph(steps, vec![]);

        enrich_success_criteria(&mut graph);

        assert_eq!(
            graph.steps[0].success_criteria.as_deref(),
            Some("Click the submit button")
        );
    }

    #[test]
    fn planner_agent_success_criteria_preserves_existing() {
        let mut step = make_step("s1", "Do something");
        step.success_criteria = Some("Already set".to_string());
        let mut graph = make_graph(vec![step], vec![]);

        enrich_success_criteria(&mut graph);

        assert_eq!(
            graph.steps[0].success_criteria.as_deref(),
            Some("Already set"),
            "Existing success_criteria should not be overwritten"
        );
    }

    // -----------------------------------------------------------------------
    // enrich_planning_metadata
    // -----------------------------------------------------------------------

    #[test]
    fn planner_agent_planning_metadata_populated() {
        let mut graph = make_graph(vec![], vec![]);

        enrich_planning_metadata(&mut graph, Some("Build a dashboard"), 3, 5, 0.87, 12);

        assert_eq!(
            graph.planning_metadata.clarified_task.as_deref(),
            Some("Build a dashboard")
        );
        assert_eq!(graph.planning_metadata.elicitation_rounds, 3);
        assert_eq!(graph.planning_metadata.slots_resolved, 5);
        assert!((graph.planning_metadata.slot_confidence - 0.87).abs() < f32::EPSILON);
        assert_eq!(graph.planning_metadata.upstream_llm_calls, 12);
        assert!(graph.planning_metadata.constraints.is_empty());
        assert!(graph.planning_metadata.objectives.is_empty());
    }

    #[test]
    fn planner_agent_planning_metadata_defaults_when_absent() {
        let mut graph = make_graph(vec![], vec![]);

        enrich_planning_metadata(&mut graph, None, 0, 0, 0.0, 0);

        assert!(graph.planning_metadata.clarified_task.is_none());
        assert_eq!(graph.planning_metadata.elicitation_rounds, 0);
        assert_eq!(graph.planning_metadata.slots_resolved, 0);
        assert!((graph.planning_metadata.slot_confidence).abs() < f32::EPSILON);
        assert_eq!(graph.planning_metadata.upstream_llm_calls, 0);
    }

    // -----------------------------------------------------------------------
    // SessionPolicy default
    // -----------------------------------------------------------------------

    #[test]
    fn planner_agent_session_policy_defaults_shared() {
        let graph = PlanGraph::default();
        assert_eq!(graph.session_policy, SessionPolicy::Shared);
    }

    // -----------------------------------------------------------------------
    // PipelineAgent trait compliance
    // -----------------------------------------------------------------------

    #[test]
    fn planner_agent_compiles_as_pipeline_agent() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<PlannerAgent>();
    }

    #[test]
    fn planner_agent_metadata() {
        let agent = PlannerAgent::with_backend(Arc::new(CapturingBackend::new()));
        assert_eq!(agent.agent_id(), "system:planner");
        assert_eq!(agent.required_inputs(), Vec::<ArtifactType>::new());
        assert_eq!(agent.output_types(), vec![ArtifactType::PlanGraph]);
        assert!(!agent.skippable());
    }

    // -----------------------------------------------------------------------
    // v1.2 enrichment helpers
    // -----------------------------------------------------------------------

    fn make_step_with_outputs(id: &str, task: &str, outputs: Vec<&str>) -> PlanStep {
        PlanStep {
            id: id.to_string(),
            task: task.to_string(),
            expected_outputs: outputs.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    fn test_pipeline_context() -> PipelineContext {
        PipelineContext {
            chain_id: "test-chain".to_string(),
            cycle_id: "cycle-1".to_string(),
            workflow_id: "wf-1".to_string(),
            query: "test query".to_string(),
            iteration: 0,
            agent_id: None,
            correlation_id: None,
            user_answer: None,
            run_started_at: None,
            resume_mode: None,
            llm_routing_calls: 0,
            elicitation_rounds: 0,
            session_id: None,
            question_id: None,
            agent_kind: None,
            is_autonomous_cycle: false,
            trust_level: None,
            tier_definitions: Vec::new(),
            observation_mode: None,
            llm_model_override: None,
            max_delegation_depth: None,
            schedule_context: None,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // enrich_response_contract
    // -----------------------------------------------------------------------

    #[test]
    fn v1_2_enrichment_action_plan() {
        let steps = vec![
            make_step("step-1", "Search for flights"),
            make_step("step-2", "Select best option"),
            make_step_with_outputs("step-3", "Complete booking", vec!["booking_confirmation"]),
        ];
        let mut graph = make_graph(steps, vec![]);
        let ctx = test_pipeline_context();

        enrich_response_contract(&mut graph, &ctx);

        let contract = graph
            .response_contract
            .expect("response_contract should be set");
        assert_eq!(contract.kind, ResponseKind::Action);
        assert_eq!(contract.terminal_step.as_deref(), Some("step-3"));
        assert!(graph.steps[0].role.is_none());
        assert!(graph.steps[1].role.is_none());
        assert_eq!(graph.steps[2].role, Some(StepRole::Terminal));
    }

    #[test]
    fn v1_2_enrichment_text_plan() {
        let steps = vec![
            make_step("step-1", "Gather data"),
            make_step_with_outputs("step-2", "Summarize the findings", vec!["report"]),
        ];
        let mut graph = make_graph(steps, vec![]);
        let ctx = test_pipeline_context();

        enrich_response_contract(&mut graph, &ctx);

        let contract = graph
            .response_contract
            .expect("response_contract should be set");
        assert_eq!(contract.kind, ResponseKind::Text);
        assert_eq!(contract.terminal_step.as_deref(), Some("step-2"));
        assert!(graph.steps[0].role.is_none());
        assert_eq!(graph.steps[1].role, Some(StepRole::Terminal));
    }

    #[test]
    fn v1_2_enrichment_fallback() {
        let steps = vec![
            make_step("step-1", "Do something generic"),
            make_step("step-2", "Do another thing"),
        ];
        let mut graph = make_graph(steps, vec![]);
        let ctx = test_pipeline_context();

        enrich_response_contract(&mut graph, &ctx);

        let contract = graph
            .response_contract
            .expect("response_contract should be set");
        assert_eq!(contract.kind, ResponseKind::Action);
        // Fallback: last step becomes terminal to satisfy Action contract validation
        assert_eq!(contract.terminal_step.as_deref(), Some("step-2"));
    }

    #[test]
    fn v1_2_enrichment_silent_for_agent() {
        let steps = vec![make_step("step-1", "Background task")];
        let mut graph = make_graph(steps, vec![]);
        let mut ctx = test_pipeline_context();
        ctx.agent_id = Some("sub-agent-1".to_string());

        enrich_response_contract(&mut graph, &ctx);

        let contract = graph
            .response_contract
            .expect("response_contract should be set");
        assert_eq!(contract.kind, ResponseKind::Silent);
        assert!(contract.terminal_step.is_none());
    }

    // -----------------------------------------------------------------------
    // D-06: planner uses raw query when ClarifiedTask is absent
    // -----------------------------------------------------------------------

    /// Mock backend that captures the query string it receives.
    struct CapturingBackend {
        captured_query: std::sync::Mutex<Option<String>>,
    }

    impl CapturingBackend {
        fn new() -> Self {
            Self {
                captured_query: std::sync::Mutex::new(None),
            }
        }
        fn captured(&self) -> String {
            self.captured_query.lock().unwrap().clone().unwrap()
        }
    }

    #[async_trait]
    impl PlannerBackend for CapturingBackend {
        async fn plan(
            &self,
            query: &str,
            _analysis: serde_json::Value,
            _clarified_task: Option<ClarifiedTask>,
            _slot_records: Vec<SlotRecord>,
            _execution_id: Option<String>,
            _correlation_id: Option<String>,
        ) -> Result<(PlanGraph, StrategyType), String> {
            *self.captured_query.lock().unwrap() = Some(query.to_string());
            // Return a 1-step plan to pass contract validation.
            let plan = make_graph(
                vec![make_step_with_outputs("s1", query, vec!["confirmation"])],
                vec![],
            );
            Ok((plan, StrategyType::GuidedSearch))
        }
    }

    #[tokio::test]
    async fn planner_uses_raw_query_when_clarified_task_absent() {
        let backend = Arc::new(CapturingBackend::new());
        let agent = PlannerAgent::with_backend(backend.clone());

        let mut store = ArtifactStore::new("test-chain");
        // Seed a QueryAnalysis but NO ClarifiedTask.
        store.put(AgentArtifact {
            artifact_id: "qa-1".to_string(),
            artifact_type: ArtifactType::QueryAnalysis,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: chrono::Utc::now(),
            render_hints: None,
        });

        let ctx = PipelineContext {
            query: "ping google.com".to_string(),
            ..test_pipeline_context()
        };

        let result = agent.execute(&mut store, &ctx).await;
        assert!(result.is_ok(), "planner should succeed: {:?}", result.err());

        // D-06: with no ClarifiedTask, the backend must receive the raw context.query.
        assert_eq!(
            backend.captured(),
            "ping google.com",
            "planner should fall back to context.query when ClarifiedTask is absent"
        );
    }

    #[test]
    fn v1_2_enrichment_at_most_one_terminal() {
        let steps = vec![
            make_step_with_outputs("step-1", "Initial confirmation", vec!["confirmation"]),
            make_step("step-2", "Middle step"),
            make_step_with_outputs("step-3", "Final confirmation", vec!["confirmation"]),
        ];
        let mut graph = make_graph(steps, vec![]);
        let ctx = test_pipeline_context();

        enrich_response_contract(&mut graph, &ctx);

        let contract = graph
            .response_contract
            .expect("response_contract should be set");
        assert_eq!(contract.terminal_step.as_deref(), Some("step-3"));
        // Only the LAST matching step gets Terminal role
        assert!(
            graph.steps[0].role.is_none(),
            "step-1 should NOT have Terminal role"
        );
        assert!(
            graph.steps[1].role.is_none(),
            "step-2 should NOT have Terminal role"
        );
        assert_eq!(
            graph.steps[2].role,
            Some(StepRole::Terminal),
            "step-3 should be the sole Terminal"
        );
    }
}
