//! Atomic Tool Composition Strategy
//!
//! This strategy serves as the ultimate fallback when all other strategies fail
//! to find matching tools. It uses llm-reasoning (e.g., o1-preview) to
//! decompose tasks into sequences of atomic primitive tools (bash, browser,
//! SQL) and directly generates an execution plan without requiring tool
//! matching.
//!
//! ## Key Features:
//! - Uses llm-reasoning model for deep problem-solving
//! - Works with the atomic/compositional tool catalog
//! - Generates direct execution sequences (no matching phase)
//! - Terminal strategy (no further escalation)
//! - Recursive decomposition for complex atomic workflows

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as FmtWrite,
    sync::Arc,
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{
    atomic_filter::{group_atomic_tools, AtomicToolSet},
    error::StrategyError,
    plan::{PlanEdge, PlanGraph, PlanProvenance, PlanStep, QuestionPriority, UnresolvedInput},
    traits::*,
    types::*,
};
use crate::magician_v2::ask_loop::TaskComplexity;
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::query_analysis::operation_llm_router::QueryAnalysisLLM;
use crate::magician_v2::{
    elicitation::{
        confidence_thresholds, ParameterContext, PlanningContext, ResolvedParameter, WorkflowStage,
    },
    prompt_identity::render_prompt_identity_from_metadata,
    prompts::{
        constants::{names, versions},
        PromptManager,
    },
    query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter},
    state_tracker::StageContext,
};

#[allow(dead_code)]
fn map_complexity(score: f32) -> TaskComplexity {
    if score < 0.33 {
        TaskComplexity::Simple
    } else if score < 0.66 {
        TaskComplexity::Moderate
    } else {
        TaskComplexity::Complex
    }
}

/// Atomic composition strategy - fallback using llm-reasoning with progressive elicitation
pub struct AtomicCompositionStrategy {
    /// Performance metrics
    metrics: StrategyMetrics,
    /// Operation-aware LLM router for llm-reasoning operations
    llm_service: Option<Arc<OperationLlmRouter>>,
    /// Prompt manager for loading templates
    prompt_manager: Option<Arc<PromptManager>>,
    /// Parameter inference service for progressive resolution (Phase 2.1)
    inference_service: Option<Arc<dyn crate::magician_v2::elicitation::ParameterInferenceService>>,
    /// Autonomous discovery service for progressive resolution (Phase 2.2)
    discovery_service: Option<Arc<dyn crate::magician_v2::elicitation::AutonomousDiscoveryService>>,
    /// Enable progressive parameter resolution (default: true)
    enable_progressive_resolution: bool,
}

impl AtomicCompositionStrategy {
    const DEFAULT_STEP_TIMEOUT_SECS: u64 = 300;
    /// Native function contract for the final atomic plan. Prompt-only JSON
    /// examples are advisory; a provider-native schema prevents a model from
    /// returning booleans or objects in fields the executor treats as text.
    const ATOMIC_PLAN_TOOL_SCHEMA: &'static str = r#"{
  "name": "emit_atomic_composition_plan",
  "description": "Emit one executable atomic composition plan.",
  "parameters": {
    "type": "object",
    "additionalProperties": false,
    "required": ["plan", "execution_strategy", "estimated_duration_ms", "confidence"],
    "properties": {
      "plan": {
        "type": "array",
        "items": {
          "type": "object",
          "additionalProperties": false,
          "required": ["step", "tool", "description", "parameters", "expected_output"],
          "properties": {
            "step": { "type": "integer", "minimum": 1 },
            "tool": { "type": "string" },
            "description": { "type": "string" },
            "parameters": { "type": "object", "additionalProperties": true },
            "expected_output": { "type": "string" },
            "rationale": { "type": ["string", "null"] },
            "dependencies": { "type": "array", "items": { "type": "integer", "minimum": 1 } },
            "session": {
              "type": ["object", "null"],
              "additionalProperties": false,
              "properties": {
                "session_id": { "type": ["string", "null"] },
                "new_session": { "type": "boolean" },
                "reuse_session": { "type": "boolean" }
              }
            },
            "prerequisites": {
              "type": ["object", "null"],
              "additionalProperties": false,
              "properties": {
                "required_slots": { "type": "array", "items": { "type": "string" } },
                "consent_flags": { "type": "array", "items": { "type": "string" } },
                "retry_budget": { "type": ["integer", "null"], "minimum": 0 }
              }
            },
            "confidence": { "type": ["number", "null"], "minimum": 0, "maximum": 1 },
            "observation": {
              "type": ["object", "null"],
              "additionalProperties": false,
              "properties": {
                "expected_state": { "type": ["string", "null"] },
                "screenshot": { "type": ["string", "null"] },
                "notes": { "type": ["string", "null"] }
              }
            }
          }
        }
      },
      "execution_strategy": { "type": "string", "enum": ["sequential", "parallel", "mixed"] },
      "estimated_duration_ms": { "type": "integer", "minimum": 0 },
      "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
    }
  }
}"#;

    /// Create new atomic composition strategy
    pub fn new() -> Self {
        Self {
            metrics: StrategyMetrics::default(),
            llm_service: None,
            prompt_manager: None,
            inference_service: None,
            discovery_service: None,
            enable_progressive_resolution: true, // Progressive mode enabled by default
        }
    }

    /// Create with LLM service and prompt manager
    pub fn with_services(
        llm_service: Arc<OperationLlmRouter>,
        prompt_manager: Arc<PromptManager>,
    ) -> Self {
        Self {
            metrics: StrategyMetrics::default(),
            llm_service: Some(llm_service),
            prompt_manager: Some(prompt_manager),
            inference_service: None,
            discovery_service: None,
            enable_progressive_resolution: true, // Progressive mode enabled by default
        }
    }

    /// Add elicitation services for progressive parameter resolution
    pub fn with_elicitation_services(
        mut self,
        inference: Option<Arc<dyn crate::magician_v2::elicitation::ParameterInferenceService>>,
        discovery: Option<Arc<dyn crate::magician_v2::elicitation::AutonomousDiscoveryService>>,
    ) -> Self {
        self.inference_service = inference;
        self.discovery_service = discovery;
        self
    }

    /// Disable progressive resolution (for A/B testing or fallback)
    pub fn disable_progressive_resolution(mut self) -> Self {
        self.enable_progressive_resolution = false;
        self
    }

    fn stage_prompt_label(stage: StageContext) -> &'static str {
        match stage {
            StageContext::PlanningBootstrap | StageContext::Unknown => "Planning Bootstrap",
            StageContext::PlanningIteration => "Planning Iteration",
            StageContext::ExecutionCycle => "Execution Cycle",
            StageContext::FollowUp => "Follow-up",
        }
    }

    fn stage_prompt_guidance(stage: StageContext) -> &'static str {
        match stage {
            StageContext::PlanningBootstrap | StageContext::Unknown => {
                "This is the first comprehensive plan for the mission. Establish full coverage, surface prerequisites, and enumerate verification checkpoints before execution begins."
            },
            StageContext::PlanningIteration => {
                "The prior plan required adjustments while still in planning. Focus on resolving previously identified gaps or clarifications without discarding validated structure."
            },
            StageContext::ExecutionCycle => {
                "Execution has already begun and new information arrived. Produce the minimal set of steps needed to resume progress, reuse validated steps, and highlight any blockers that require human input."
            },
            StageContext::FollowUp => {
                "The mission is in wrap-up mode. Concentrate on outstanding follow-ups, confirmations, and tidy hand-off steps rather than rebuilding the entire workflow."
            },
        }
    }

    fn with_stage_guidance(stage: StageContext, phase: &str, prompt: String) -> String {
        let label = Self::stage_prompt_label(stage);
        let guidance = Self::stage_prompt_guidance(stage);
        let stage_name = stage.as_str();
        format!("Stage Guidance: {label} ({stage_name}) — {phase}.\n{guidance}\n\n{prompt}",)
    }

    /// Render the tool catalog for one planning phase.
    ///
    /// `include_params` follows the two-phase split this strategy already has:
    /// the **outline** picks which tools to use and needs names and
    /// descriptions only, while the **expansion** has to fill a `parameters`
    /// object per step and cannot do that without the signatures.
    ///
    /// The split is worth the branch. Measured against the live
    /// `anonymous/default` catalog, the flat listing is ~2.2k tokens without
    /// parameters and ~12.5k with them; the expansion retries up to three
    /// times, so sending signatures to the outline as well would put roughly
    /// 10k redundant tokens into every planning run before a single step is
    /// written.
    fn render_atomic_tool_prompt(
        context: &StrategyContext,
        atomic_tools: &AtomicToolSet,
        include_params: bool,
    ) -> String {
        let flat_catalog = atomic_tools.format_for_prompt_compact_with_options(include_params);
        let grouped_catalog = context.render_planner_tool_catalog(&atomic_tools.tools);
        let mut prompt = if grouped_catalog.trim().is_empty() {
            flat_catalog
        } else {
            format!("{flat_catalog}\n\nAGENT-GROUPED TOOL CATALOG:\n{grouped_catalog}",)
        };

        // Procedure playbooks carry no `runtime_contract:`, so they produce no
        // capability pack and cannot appear in either catalog above. Their only
        // entry point is `activate_skill`, whose `name` is a free-form string —
        // without this list the planner has nothing to name.
        let playbooks = context.render_available_procedure_skills();
        if !playbooks.trim().is_empty() {
            prompt.push('\n');
            prompt.push_str(&playbooks);
        }

        prompt
    }

    /// Generate atomic composition using llm-reasoning with optional feedback
    /// context and progressive parameter resolution
    async fn request_atomic_plan_with_context(
        &self,
        context: &StrategyContext,
        query: &str,
        atomic_tools: &AtomicToolSet,
        outline: &AtomicOutlinePlan,
        outline_tool_selections: &[crate::magician_v2::slot_graph::SlotRecord],
        planning_context: &PlanningContext,
        feedback: Option<&str>,
    ) -> Result<AtomicCompositionPlan> {
        let llm_service = self
            .llm_service
            .as_ref()
            .ok_or_else(|| anyhow!("LLM service not configured for atomic composition"))?;

        let tools_description = Self::render_atomic_tool_prompt(context, atomic_tools, true);

        let feedback_processed = feedback.and_then(|f| {
            let trimmed = f.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        // Build prompt with available atomic tools
        let prompt_manager = self
            .prompt_manager
            .as_ref()
            .ok_or_else(|| anyhow!("Prompt manager not configured for atomic composition"))?;

        let mut variables = HashMap::new();
        variables.insert("query".to_string(), query.to_string());
        variables.insert("tools_description".to_string(), tools_description.clone());
        variables.insert("outline_summary".to_string(), outline.format_for_prompt());
        variables.insert(
            "outline_json".to_string(),
            serde_json::to_string(outline).unwrap_or_else(|_| "{}".to_string()),
        );
        variables.insert(
            "planning_stage".to_string(),
            context.stage_context.as_str().to_string(),
        );

        if let Some(ref fb) = feedback_processed {
            variables.insert("has_feedback".to_string(), "true".to_string());
            variables.insert("feedback_text".to_string(), fb.clone());
        } else {
            variables.insert("has_feedback".to_string(), "false".to_string());
            variables.insert("feedback_text".to_string(), "".to_string());
        }

        // Add allow_consent_slots flag from context
        variables.insert(
            "allow_consent_slots".to_string(),
            context.allow_consent_slots.to_string(),
        );

        // Extract tool_selection slots from both context slot graph and outline prerequisites
        let mut tool_selections = Vec::new();

        // First check context.slot_graph (filled slots from previous elicitation)
        for slot_record in &context.slot_graph {
            if slot_record.id.starts_with("tool_selection:") {
                // Extract the selection value and format for prompt
                let selection_name = slot_record
                    .id
                    .strip_prefix("tool_selection:")
                    .unwrap_or(&slot_record.id);
                let value = slot_record.value.as_str().unwrap_or("unknown");
                tool_selections.push(format!("{}: {}", selection_name, value));
            }
        }

        // Then check outline_tool_selections (newly detected from this outline)
        for slot_record in outline_tool_selections {
            if slot_record.id.starts_with("tool_selection:") {
                let selection_name = slot_record
                    .id
                    .strip_prefix("tool_selection:")
                    .unwrap_or(&slot_record.id);
                let value = slot_record.value.as_str().unwrap_or("unknown");
                let formatted = format!("{}: {}", selection_name, value);
                // Avoid duplicates
                if !tool_selections.contains(&formatted) {
                    tool_selections.push(formatted);
                }
            }
        }

        if !tool_selections.is_empty() {
            variables.insert("has_tool_selections".to_string(), "true".to_string());
            variables.insert("tool_selections".to_string(), tool_selections.join("\n"));
        } else {
            variables.insert("has_tool_selections".to_string(), "false".to_string());
            variables.insert("tool_selections".to_string(), "".to_string());
        }

        // NEW: Add resolved parameters from progressive elicitation
        if !planning_context.resolved_params().is_empty() {
            let resolved_params_json = serde_json::to_string(planning_context.resolved_params())
                .unwrap_or_else(|_| "{}".to_string());

            variables.insert("has_resolved_params".to_string(), "true".to_string());
            variables.insert("resolved_params".to_string(), resolved_params_json);

            info!(
                "[ATOMIC-PROGRESSIVE] Passing {} resolved parameters to expansion LLM",
                planning_context.resolved_params().len()
            );
        } else {
            variables.insert("has_resolved_params".to_string(), "false".to_string());
            variables.insert("resolved_params".to_string(), "{}".to_string());
        }

        if let Some(snapshot) = &context.planning_snapshot {
            variables.insert(
                "planning_confidence_overall".to_string(),
                format!("{:.3}", snapshot.confidence_overall),
            );
            variables.insert(
                "planning_needs_clarification".to_string(),
                snapshot.needs_clarification.to_string(),
            );

            if let Some(slope) = snapshot.confidence_slope {
                variables.insert(
                    "planning_confidence_slope".to_string(),
                    format!("{:.6}", slope),
                );
            }

            if !snapshot.confidence_per_slot.is_empty() {
                if let Ok(serialized) = serde_json::to_string(&snapshot.confidence_per_slot) {
                    variables.insert("planning_confidence_per_slot".to_string(), serialized);
                }
            }

            if !snapshot.confidence_history.is_empty() {
                if let Ok(serialized) = serde_json::to_string(&snapshot.confidence_history) {
                    variables.insert("planning_confidence_history".to_string(), serialized);
                }
            }
        }

        let prompt = prompt_manager
            .get_rendered_prompt(
                names::ATOMIC_COMPOSITION,
                versions::ATOMIC_COMPOSITION,
                variables,
            )
            .await
            .map_err(|e| anyhow!("Failed to load atomic prompt from storage: {}", e))?;

        let mut system_vars = HashMap::new();
        system_vars.insert(
            "identity_section".to_string(),
            render_prompt_identity_from_metadata(&context.execution_context.metadata, false),
        );
        let system_prompt = prompt_manager
            .get_rendered_prompt(
                names::ATOMIC_COMPOSITION_SYSTEM,
                versions::ATOMIC_COMPOSITION_SYSTEM,
                system_vars,
            )
            .await
            .map_err(|e| anyhow!("Failed to load atomic composition system prompt: {}", e))?;

        let prompt =
            Self::with_stage_guidance(context.stage_context, "plan expansion phase", prompt);

        info!(
            "[MAGICIAN-V2-STRATEGY] Atomic composition: using llm-reasoning for {} atomic tools",
            atomic_tools.total_count
        );

        if let Some(decision) = context
            .evaluate_budget_guard()
            .await
            .map_err(|e| anyhow!("Budget evaluation failed before plan expansion: {}", e))?
        {
            return Err(StrategyError::budget_hold(
                context.stage_context.as_str(),
                decision.recommended_channel.as_str(),
                &decision.reason,
            )
            .into());
        }
        let llm_started = std::time::Instant::now();
        let llm_response = llm_service
            .generate_for_operation_with_system_and_tool_schema(
                &LLMOperation::AtomicComposition,
                Some(system_prompt.as_str()),
                &prompt,
                Some(Self::ATOMIC_PLAN_TOOL_SCHEMA),
                None,
            )
            .await?;
        context.emit_operation_llm_telemetry(
            LLMOperation::AtomicComposition.as_str(),
            &llm_response,
            llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        );

        // Record actual token usage from API response
        if let Some(ref usage) = llm_response.usage {
            context.record_llm_usage(Some(usage.total_tokens));
            debug!(
                "[ATOMIC-COMPOSITION] LLM call used {} tokens (prompt: {}, completion: {})",
                usage.total_tokens, usage.prompt_tokens, usage.completion_tokens
            );
        } else {
            // Fallback to estimation if no usage data available
            let estimated_tokens = ((prompt.len() / 4).max(1)) as u32;
            context.record_llm_usage(Some(estimated_tokens));
            debug!(
                "[ATOMIC-COMPOSITION] No usage data, estimated {} tokens from prompt length",
                estimated_tokens
            );
        }

        // Parse response into atomic plan
        self.parse_atomic_plan(&llm_response.content, query)
    }

    /// Parse LLM response into atomic plan
    fn parse_atomic_plan(&self, response: &str, query: &str) -> Result<AtomicCompositionPlan> {
        debug!("[MAGICIAN-V2-STRATEGY] Parsing atomic composition response");

        // Try to parse as JSON
        match serde_json::from_str::<AtomicCompositionPlan>(response) {
            Ok(plan) => {
                info!(
                    "[MAGICIAN-V2-STRATEGY] Successfully parsed atomic plan with {} steps",
                    plan.plan.len()
                );
                Ok(plan)
            },
            Err(e) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Failed to parse atomic plan as JSON: {}",
                    e
                );

                // Fallback: try to extract JSON from response
                if let Some(start) = response.find('{') {
                    if let Some(end) = response.rfind('}') {
                        let json_str = &response[start..=end];
                        return serde_json::from_str::<AtomicCompositionPlan>(json_str)
                            .map_err(|e2| anyhow!("Failed to parse extracted JSON: {}", e2));
                    }
                }

                Err(anyhow!(
                    "Failed to parse atomic composition plan for query '{}': {}",
                    query,
                    e
                ))
            },
        }
    }

    async fn request_atomic_outline(
        &self,
        context: &StrategyContext,
        query: &str,
        atomic_tools: &AtomicToolSet,
    ) -> Result<AtomicOutlinePlan> {
        let llm_service = self
            .llm_service
            .as_ref()
            .ok_or_else(|| anyhow!("LLM service not configured for atomic composition outline"))?;

        let prompt_manager = self.prompt_manager.as_ref().ok_or_else(|| {
            anyhow!("Prompt manager not configured for atomic composition outline")
        })?;

        let mut variables = HashMap::new();
        variables.insert("query".to_string(), query.to_string());
        variables.insert(
            "complexity_score".to_string(),
            format!("{:.2}", context.query_analysis.complexity.score),
        );
        variables.insert(
            "tools_description".to_string(),
            Self::render_atomic_tool_prompt(context, atomic_tools, false),
        );
        variables.insert(
            "planning_stage".to_string(),
            context.stage_context.as_str().to_string(),
        );

        let prompt = prompt_manager
            .get_rendered_prompt(
                names::ATOMIC_COMPOSITION_OUTLINE,
                versions::ATOMIC_COMPOSITION_OUTLINE,
                variables,
            )
            .await
            .map_err(|e| anyhow!("Failed to load atomic outline prompt: {}", e))?;

        let mut system_vars = HashMap::new();
        system_vars.insert(
            "identity_section".to_string(),
            render_prompt_identity_from_metadata(&context.execution_context.metadata, false),
        );
        let system_prompt = prompt_manager
            .get_rendered_prompt(
                names::ATOMIC_COMPOSITION_OUTLINE_SYSTEM,
                versions::ATOMIC_COMPOSITION_OUTLINE_SYSTEM,
                system_vars,
            )
            .await
            .map_err(|e| anyhow!("Failed to load atomic outline system prompt: {}", e))?;

        let prompt = Self::with_stage_guidance(context.stage_context, "outline phase", prompt);

        info!(
            "[MAGICIAN-V2-STRATEGY] Atomic composition outline phase: {} atomic tools available",
            atomic_tools.total_count
        );

        if let Some(decision) = context
            .evaluate_budget_guard()
            .await
            .map_err(|e| anyhow!("Budget evaluation failed before outline: {}", e))?
        {
            return Err(StrategyError::budget_hold(
                context.stage_context.as_str(),
                decision.recommended_channel.as_str(),
                &decision.reason,
            )
            .into());
        }
        let llm_started = std::time::Instant::now();
        let llm_response = llm_service
            .generate_for_operation_with_system_and_tool_schema(
                &LLMOperation::AtomicCompositionOutline,
                Some(system_prompt.as_str()),
                &prompt,
                None,
                None,
            )
            .await?;
        context.emit_operation_llm_telemetry(
            LLMOperation::AtomicCompositionOutline.as_str(),
            &llm_response,
            llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        );

        // Record actual token usage from API response
        if let Some(ref usage) = llm_response.usage {
            context.record_llm_usage(Some(usage.total_tokens));
            debug!(
                "[ATOMIC-COMPOSITION] Outline LLM call used {} tokens (prompt: {}, completion: {})",
                usage.total_tokens, usage.prompt_tokens, usage.completion_tokens
            );
        } else {
            // Fallback to estimation if no usage data available
            let estimated_tokens = ((prompt.len() / 4).max(1)) as u32;
            context.record_llm_usage(Some(estimated_tokens));
            debug!(
                "[ATOMIC-COMPOSITION] No usage data for outline, estimated {} tokens",
                estimated_tokens
            );
        }

        self.parse_atomic_outline(&llm_response.content, query)
    }

    fn parse_atomic_outline(&self, response: &str, query: &str) -> Result<AtomicOutlinePlan> {
        debug!("[MAGICIAN-V2-STRATEGY] Parsing atomic composition outline response");

        match serde_json::from_str::<AtomicOutlinePlan>(response) {
            Ok(plan) => {
                info!(
                    "[MAGICIAN-V2-STRATEGY] Parsed outline with {} top-level goals",
                    plan.goals.len()
                );
                Ok(plan)
            },
            Err(e) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Failed to parse outline as JSON: {}",
                    e
                );

                if let Some(start) = response.find('{') {
                    if let Some(end) = response.rfind('}') {
                        let json_str = &response[start..=end];
                        return serde_json::from_str::<AtomicOutlinePlan>(json_str).map_err(|e2| {
                            anyhow!("Failed to parse extracted outline JSON: {}", e2)
                        });
                    }
                }

                Err(anyhow!(
                    "Failed to parse atomic composition outline for query '{}': {}",
                    query,
                    e
                ))
            },
        }
    }

    async fn compose_with_retry(
        &self,
        context: &StrategyContext,
        query: &str,
        atomic_tools: &AtomicToolSet,
        max_retries: u32,
    ) -> Result<(AtomicCompositionPlan, PlanGraph)> {
        let mut feedback: Option<String> = None;
        // Initialize PlanningContext for progressive parameter resolution
        let mut planning_context = PlanningContext::new();
        let prompt_identity =
            crate::magician_v2::prompt_identity::parse_prompt_identity_from_execution_context(
                &context.execution_context,
            );

        for attempt in 0..max_retries {
            // Broadcast outline phase started (Phase 1)
            if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
                &context.event_broadcaster,
                &context.execution_id,
                &context.correlation_id,
            ) {
                broadcaster.atomic_plan_outline_started(
                    execution_id,
                    correlation_id,
                    atomic_tools.total_count,
                );
            }

            let outline = self
                .request_atomic_outline(context, query, atomic_tools)
                .await?;

            // Broadcast outline phase completed
            if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
                &context.event_broadcaster,
                &context.execution_id,
                &context.correlation_id,
            ) {
                broadcaster.atomic_plan_outline_completed(
                    execution_id,
                    correlation_id,
                    outline.goals.len(),
                    outline.overall_confidence.unwrap_or(0.0) as f64,
                );
            }

            // Extract tool_selection prerequisites from outline
            // These will be checked when building prompt variables
            let workflow_id = context.execution_id.as_deref().unwrap_or("unknown");
            let outline_tool_selections = Self::extract_tool_selection_slots(&outline, workflow_id);
            if !outline_tool_selections.is_empty() {
                debug!(
                    "[MAGICIAN-V2-STRATEGY] Extracted {} tool_selection slots from outline prerequisites",
                    outline_tool_selections.len()
                );
            }

            // ========== PROGRESSIVE PARAMETER RESOLUTION ==========
            if self.enable_progressive_resolution {
                info!(
                    "[ATOMIC-PROGRESSIVE] Starting progressive parameter resolution for {} outline goals",
                    outline.goals.len()
                );

                // Extract parameters from outline goals (lightweight pre-scan)
                let potential_params = self.extract_parameters_from_outline(&outline, query).await;

                if !potential_params.is_empty() {
                    let total_params = potential_params.len();
                    info!(
                        "[ATOMIC-PROGRESSIVE] Extracted {} potential parameters from outline",
                        total_params
                    );

                    // Phase 1.1: Classify parameter priorities with LLM
                    let classified_params = match self
                        .classify_parameter_priorities(potential_params, query, &outline, context)
                        .await
                    {
                        Ok(params) => params,
                        Err(e) => {
                            warn!(
                                "[ATOMIC-PROGRESSIVE] Priority classification failed: {}. Using default priorities.",
                                e
                            );
                            // Fallback to extracted params with default priorities
                            self.extract_parameters_from_outline(&outline, query).await
                        },
                    };

                    for param in classified_params {
                        // DEDUPLICATION: Skip if already resolved
                        if planning_context.has_value(&param.parameter) {
                            info!(
                                "[ATOMIC-PROGRESSIVE] ✅ Reusing parameter '{}'",
                                param.parameter
                            );
                            continue;
                        }

                        let threshold = Self::get_confidence_threshold(&param.priority);
                        let execution_id = context.execution_id.as_deref().unwrap_or("");

                        // Phase 2.1: Try inference
                        if let Some(resolved) = self
                            .try_progressive_inference(
                                &param,
                                &planning_context,
                                query,
                                execution_id,
                                threshold,
                                prompt_identity.as_ref(),
                            )
                            .await?
                        {
                            planning_context.set_resolved(param.parameter.clone(), resolved);
                            continue;
                        }

                        // Phase 2.2: Try discovery
                        // Emit ParameterDiscoveryAttempted event
                        if let Some(broadcaster) = &context.event_broadcaster {
                            broadcaster.parameter_discovery_attempted(
                                execution_id,
                                param.parameter.clone(),
                                "AutonomousDiscovery".to_string(), // Discovery method
                            );
                        }

                        match self
                            .try_progressive_discovery(
                                &param,
                                &planning_context,
                                query,
                                execution_id,
                                threshold,
                                prompt_identity.as_ref(),
                            )
                            .await
                        {
                            Ok(Some(resolved)) => {
                                // Emit ParameterDiscovered event
                                if let Some(broadcaster) = &context.event_broadcaster {
                                    broadcaster.parameter_discovered(
                                        execution_id,
                                        param.parameter.clone(),
                                        resolved.value.clone(),
                                        resolved.confidence,
                                        format!("{:?}", resolved.source),
                                        false, // external_actions_performed - discovery is manual for now
                                    );
                                }
                                planning_context.set_resolved(param.parameter.clone(), resolved);
                                continue;
                            },
                            Ok(None) => {
                                // Emit ParameterDiscoveryFailed event (below threshold or not discovered)
                                if let Some(broadcaster) = &context.event_broadcaster {
                                    broadcaster.parameter_discovery_failed(
                                        execution_id,
                                        param.parameter.clone(),
                                        "Discovery returned no value or below confidence threshold"
                                            .to_string(),
                                    );
                                }
                            },
                            Err(e) => {
                                // Emit ParameterDiscoveryFailed event
                                if let Some(broadcaster) = &context.event_broadcaster {
                                    broadcaster.parameter_discovery_failed(
                                        execution_id,
                                        param.parameter.clone(),
                                        e.to_string(),
                                    );
                                }
                                // Don't propagate error, just continue to next phase
                            },
                        }

                        // Phase 3: Defer or ask upfront
                        if Self::should_defer_to_execution(&param) {
                            info!(
                                "[ATOMIC-PROGRESSIVE] ⏳ Deferring '{}' to JIT",
                                param.parameter
                            );
                            let deferred = ResolvedParameter::deferred(param.parameter.clone());
                            planning_context.set_resolved(param.parameter.clone(), deferred);
                        } else {
                            info!(
                                "[ATOMIC-PROGRESSIVE] ❓ Marking '{}' for upfront question",
                                param.parameter
                            );
                            // Don't add to planning_context - will be in unresolved_inputs
                        }
                    }

                    let auto_resolved_count = planning_context
                        .resolved_params()
                        .values()
                        .filter(|p| !p.deferred)
                        .count();
                    let deferred_count = planning_context
                        .resolved_params()
                        .values()
                        .filter(|p| p.deferred)
                        .count();
                    let to_ask_count = total_params - planning_context.resolved_params().len();

                    info!(
                        "[ATOMIC-PROGRESSIVE] Progressive resolution complete: {} auto-resolved, {} deferred, {} to ask",
                        auto_resolved_count, deferred_count, to_ask_count
                    );
                } else {
                    debug!("[ATOMIC-PROGRESSIVE] No parameters extracted from outline");
                }
            }
            // ========== END PROGRESSIVE RESOLUTION ==========

            // Broadcast expansion phase started (Phase 2)
            if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
                &context.event_broadcaster,
                &context.execution_id,
                &context.correlation_id,
            ) {
                broadcaster.atomic_plan_expansion_started(
                    execution_id,
                    correlation_id,
                    outline.goals.len(),
                );
            }

            let plan = match self
                .request_atomic_plan_with_context(
                    context,
                    query,
                    atomic_tools,
                    &outline,
                    &outline_tool_selections,
                    &planning_context,
                    feedback.as_deref(),
                )
                .await
            {
                Ok(plan) => plan,
                Err(error) if attempt + 1 < max_retries => {
                    warn!(
                        attempt = attempt + 1,
                        max_retries,
                        error = %error,
                        "Atomic plan response failed its native/serde contract; retrying with corrective feedback"
                    );
                    feedback = Some(format!(
                        "The previous expansion did not satisfy the atomic plan JSON contract: {error}. Return only the emit_atomic_composition_plan arguments and preserve every declared field type exactly."
                    ));
                    continue;
                },
                Err(error) => return Err(error),
            };

            let mut graph = self
                .convert_atomic_plan_to_graph(&plan, query, context, context.allow_consent_slots)
                .await;

            match super::plan_validator::validate_plan(&graph, &atomic_tools.tools, true) {
                Ok(mut holes) => {
                    // ENHANCED: Filter out already-resolved parameters
                    if self.enable_progressive_resolution {
                        let original_count = holes.len();
                        holes.retain(|hole| !planning_context.has_value(&hole.parameter));
                        let filtered_count = original_count - holes.len();
                        if filtered_count > 0 {
                            info!(
                                "[ATOMIC-PROGRESSIVE] Filtered out {} already-resolved parameters from unresolved inputs",
                                filtered_count
                            );
                        }
                    }

                    graph.unresolved_inputs.append(&mut holes);

                    // Broadcast final plan generated with "valid" status
                    if let (
                        Some(broadcaster),
                        Some(execution_id),
                        Some(correlation_id),
                        Some(turn_id),
                    ) = (
                        &context.event_broadcaster,
                        &context.execution_id,
                        &context.correlation_id,
                        &context.turn_id,
                    ) {
                        broadcaster.atomic_plan_generated(
                            execution_id,
                            correlation_id,
                            turn_id,
                            graph.clone(),
                            "valid".to_string(),
                            attempt + 1,
                        );
                    }

                    return Ok((plan, graph));
                },
                Err(err) => {
                    warn!(
                        "[MAGICIAN-V2-STRATEGY] Atomic plan validation failed (attempt {}): {}",
                        attempt + 1,
                        err
                    );

                    // Broadcast plan with "retrying" status
                    if let (
                        Some(broadcaster),
                        Some(execution_id),
                        Some(correlation_id),
                        Some(turn_id),
                    ) = (
                        &context.event_broadcaster,
                        &context.execution_id,
                        &context.correlation_id,
                        &context.turn_id,
                    ) {
                        broadcaster.atomic_plan_generated(
                            execution_id,
                            correlation_id,
                            turn_id,
                            graph.clone(),
                            "retrying".to_string(),
                            attempt + 1,
                        );
                    }

                    if attempt + 1 == max_retries {
                        return Err(anyhow!(
                            "Atomic composition validation failed after {} attempts: {}",
                            max_retries,
                            err
                        ));
                    }
                    feedback = Some(err.to_string());
                },
            }
        }

        Err(anyhow!(
            "Atomic composition retry loop exhausted without producing a valid plan"
        ))
    }

    /// Convert raw atomic plan into shared PlanGraph representation.
    async fn convert_atomic_plan_to_graph(
        &self,
        atomic_plan: &AtomicCompositionPlan,
        root_query: &str,
        context: &StrategyContext,
        allow_consent_slots: bool,
    ) -> PlanGraph {
        let mut steps = Vec::new();
        let mut step_id_lookup: HashMap<usize, String> = HashMap::new();

        // Always process steps in ascending order to keep the DAG deterministic.
        let mut ordered_steps: Vec<&AtomicStep> = atomic_plan.plan.iter().collect();
        ordered_steps.sort_by_key(|step| step.step);

        for atomic_step in ordered_steps.iter() {
            // Build parameters map from the parsed JSON object.
            let parameters: HashMap<String, serde_json::Value> =
                if let serde_json::Value::Object(obj) = &atomic_step.parameters {
                    obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                } else {
                    HashMap::new()
                };

            let mut metadata = HashMap::new();
            metadata.insert("description".to_string(), atomic_step.description.clone());
            if let Some(rationale) = &atomic_step.rationale {
                metadata.insert("rationale".to_string(), rationale.clone());
            }
            if let Some(session) = &atomic_step.session {
                if let Ok(serialized) = serde_json::to_string(session) {
                    metadata.insert("session".to_string(), serialized);
                }
            }
            if let Some(prereqs) = &atomic_step.prerequisites {
                if let Ok(serialized) = serde_json::to_string(prereqs) {
                    metadata.insert("prerequisites".to_string(), serialized);
                }
            }
            if let Some(observation) = &atomic_step.observation {
                if let Ok(serialized) = serde_json::to_string(observation) {
                    metadata.insert("observation_checkpoint".to_string(), serialized);
                }
            }

            if !atomic_step.dependencies.is_empty() {
                metadata.insert(
                    "dependencies".to_string(),
                    atomic_step
                        .dependencies
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }

            let step_confidence = atomic_step.confidence.unwrap_or(atomic_plan.confidence);
            let step_id = format!("step-{}", atomic_step.step);

            // Use a generous per-step timeout (5 minutes) until tool metadata exposes precise hints.
            let timeout_override_secs = Some(Self::DEFAULT_STEP_TIMEOUT_SECS);

            // Look up providing_agent_id from the delegate tool catalog.
            // If the tool name matches an entry in any delegate's catalog,
            // tag the step so the executor routes it to that delegate agent.
            let providing_agent_id = context.providing_agent_id_for_tool(&atomic_step.tool);
            let step_hash = providing_agent_id
                .as_ref()
                .map(|aid| PlanStep::compute_hash(aid, &atomic_step.tool));

            steps.push(PlanStep {
                id: step_id.clone(),
                task: atomic_step.description.clone(),
                tool: Some(atomic_step.tool.clone()),
                parameters,
                expected_outputs: if atomic_step.expected_output.is_empty() {
                    Vec::new()
                } else {
                    vec![atomic_step.expected_output.clone()]
                },
                confidence: step_confidence,
                metadata,
                timeout_override_secs,
                providing_agent_id,
                step_hash,
                ..Default::default()
            });

            step_id_lookup.insert(atomic_step.step, step_id);
        }

        // Determine whether we should link every step sequentially, or rely solely
        // on declared dependencies to unlock parallel execution.
        let lowered_strategy = atomic_plan.execution_strategy.to_lowercase();
        let allow_sequential_fallback =
            lowered_strategy.contains("sequential") && !lowered_strategy.contains("parallel");

        let mut edges = Vec::new();
        let mut seen_edges: HashSet<(String, String)> = HashSet::new();

        for (idx, atomic_step) in ordered_steps.iter().enumerate() {
            let current_id = match step_id_lookup.get(&atomic_step.step) {
                Some(id) => id.clone(),
                None => continue,
            };

            if !atomic_step.dependencies.is_empty() {
                for dep in &atomic_step.dependencies {
                    if let Some(dep_id) = step_id_lookup.get(dep) {
                        let edge = (dep_id.clone(), current_id.clone());
                        if seen_edges.insert(edge.clone()) {
                            edges.push(PlanEdge {
                                from: edge.0,
                                to: edge.1,
                                reason: "declared_dependency".to_string(),
                            });
                        }
                    } else {
                        warn!(
                            "[MAGICIAN-V2-STRATEGY] Atomic step {} references missing dependency {}",
                            atomic_step.step, dep
                        );
                    }
                }
            } else if allow_sequential_fallback {
                if let Some(prev_step) = idx
                    .checked_sub(1)
                    .and_then(|i| step_id_lookup.get(&ordered_steps[i].step))
                {
                    let edge = (prev_step.clone(), current_id.clone());
                    if seen_edges.insert(edge.clone()) {
                        edges.push(PlanEdge {
                            from: edge.0,
                            to: edge.1,
                            reason: "sequential_dependency".to_string(),
                        });
                    }
                }
            }
        }

        let mut unresolved_inputs = self
            .build_prerequisite_input_holes(atomic_plan, root_query, context)
            .await;

        // Filter out consent_flags if allow_consent_slots is false
        if !allow_consent_slots {
            unresolved_inputs
                .retain(|hole| !matches!(hole.expected_type.as_deref(), Some("consent_flag")));
        }

        PlanGraph {
            steps,
            edges,
            unresolved_inputs: {
                unresolved_inputs.sort_by(|a, b| {
                    a.step_id
                        .cmp(&b.step_id)
                        .then_with(|| a.parameter.cmp(&b.parameter))
                });
                unresolved_inputs
            },
            confidence: atomic_plan.confidence,
            provenance: PlanProvenance {
                strategy: "AtomicComposition".to_string(),
                generator: Some("llm_reasoner".to_string()),
                notes: Some(format!("query: {}", root_query)),
            },
            ..Default::default()
        }
    }

    async fn build_prerequisite_input_holes(
        &self,
        atomic_plan: &AtomicCompositionPlan,
        root_query: &str,
        context: &StrategyContext,
    ) -> Vec<UnresolvedInput> {
        let mut inputs = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();

        for step in &atomic_plan.plan {
            let step_identifier = format!("step-{}", step.step);
            if let Some(prereqs) = &step.prerequisites {
                // Process required slots
                for slot in &prereqs.required_slots {
                    if slot.trim().is_empty() {
                        continue;
                    }
                    let key = format!("slot::{}", slot);
                    if seen.insert((step_identifier.clone(), key.clone())) {
                        let id = format!("slot_{}", slot);
                        let param = slot.clone();

                        // LLM-based priority classification
                        let (priority, ask_timing, discovery_timing, inference_hints) = self
                            .classify_slot_priority_with_llm(&param, step, root_query, context)
                            .await
                            .unwrap_or_else(|_| {
                                // Fallback to safe defaults if LLM fails
                                (
                                    QuestionPriority::PreExecution,
                                    super::plan::AskTiming::PreExecution,
                                    super::plan::DiscoveryTiming::Auto,
                                    vec![
                                        format!("step_tool:{}", step.tool),
                                        format!("step_description:{}", step.description),
                                    ],
                                )
                            });

                        inputs.push(UnresolvedInput {
                            id: id.clone(),
                            parameter: param.clone(),
                            display_name: super::plan::humanize_parameter(&param),
                            step_id: Some(step_identifier.clone()),
                            linked_steps: vec![step_identifier.clone()],
                            expected_type: Some("slot".to_string()),
                            json_schema: None,
                            prompt: format!(
                                "Await required slot '{}' before executing step {}",
                                slot, step.step
                            ),
                            required: true,
                            notes: Some("slot_prerequisite".to_string()),
                            priority,
                            ask_timing,
                            discovery_timing,
                            default_value: None,
                            inference_hints,
                            inference_threshold: 0.7,
                            auto_fill: None,
                            auto_fill_confidence: None,
                            source: super::plan::InputSource::Planner,
                            created_at: None,
                            updated_at: None,
                            status: None,
                        });
                    }
                }

                // Process consent flags
                for consent in &prereqs.consent_flags {
                    if consent.trim().is_empty() {
                        continue;
                    }
                    let key = format!("consent::{}", consent);
                    if seen.insert((step_identifier.clone(), key.clone())) {
                        let id = format!("consent_{}", consent);
                        let param = consent.clone();

                        inputs.push(UnresolvedInput {
                            id: id.clone(),
                            parameter: param.clone(),
                            display_name: super::plan::humanize_parameter(&param),
                            step_id: Some(step_identifier.clone()),
                            linked_steps: vec![step_identifier.clone()],
                            expected_type: Some("consent_flag".to_string()),
                            json_schema: Some(json!({"type": "boolean"})),
                            prompt: format!(
                                "Consent flag '{}' must be acknowledged before executing step {}",
                                consent, step.step
                            ),
                            required: true,
                            notes: Some("consent_prerequisite".to_string()),
                            priority: QuestionPriority::Critical, // Consent is always critical
                            ask_timing: super::plan::AskTiming::PreExecution,
                            discovery_timing: super::plan::DiscoveryTiming::Auto,
                            default_value: Some(json!(false)), // Default to false for safety
                            inference_hints: vec![],
                            inference_threshold: 0.9, // High threshold for consent
                            auto_fill: None,
                            auto_fill_confidence: None,
                            source: super::plan::InputSource::Planner,
                            created_at: None,
                            updated_at: None,
                            status: None,
                        });
                    }
                }
            }
        }

        inputs
    }

    /// Classify slot priority using LLM (mini/fast model for efficiency)
    ///
    /// Returns: (priority, ask_timing, discovery_timing, inference_hints)
    async fn classify_slot_priority_with_llm(
        &self,
        slot_name: &str,
        step: &AtomicStep,
        user_query: &str,
        context: &StrategyContext,
    ) -> Result<(
        super::plan::QuestionPriority,
        super::plan::AskTiming,
        super::plan::DiscoveryTiming,
        Vec<String>,
    )> {
        // Build inference hints from context
        let mut inference_hints = Vec::new();
        inference_hints.push(format!("step_tool:{}", step.tool));
        inference_hints.push(format!("step_description:{}", step.description));

        // Build variables for prompt rendering
        let mut variables = HashMap::new();
        variables.insert("parameter_name".to_string(), slot_name.to_string());
        variables.insert("step_tool".to_string(), step.tool.clone());
        variables.insert("step_description".to_string(), step.description.clone());
        variables.insert("user_request".to_string(), user_query.to_string());

        // Get prompt manager
        let prompt_manager = self
            .prompt_manager
            .as_ref()
            .ok_or_else(|| anyhow!("PromptManager not available for parameter classification"))?;

        // Get rendered prompt from PromptManager
        let prompt = prompt_manager
            .get_rendered_prompt(
                names::PARAM_PRIORITY_CLASSIFICATION,
                versions::PARAM_PRIORITY_CLASSIFICATION,
                variables,
            )
            .await
            .map_err(|e| anyhow!("Failed to load param_priority_classification prompt: {}", e))?;

        // Get LLM service reference
        let llm_service = self.llm_service.as_ref().ok_or_else(|| {
            anyhow::anyhow!("LLM service not available for parameter classification")
        })?;

        // Call LLM with ParameterExtraction operation (nano) for simple classification
        let llm_started = std::time::Instant::now();
        let response = llm_service
            .generate_for_operation(&LLMOperation::ParameterExtraction, &prompt)
            .await?;
        context.emit_operation_llm_telemetry(
            LLMOperation::ParameterExtraction.as_str(),
            &response,
            llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        );

        // Parse JSON response
        let json: serde_json::Value = serde_json::from_str(&response.content)
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {}", e))?;

        // Extract priority
        let priority = match json["priority"].as_str() {
            Some("Critical") => QuestionPriority::Critical,
            Some("PreExecution") => QuestionPriority::PreExecution,
            Some("JustInTime") => QuestionPriority::JustInTime,
            Some("Optional") => QuestionPriority::Optional,
            Some("Inferrable") => QuestionPriority::Inferrable,
            _ => QuestionPriority::PreExecution, // Safe default
        };

        // Extract ask_timing
        let ask_timing = match json["ask_timing"].as_str() {
            Some("PreExecution") => super::plan::AskTiming::PreExecution,
            Some("JustInTime") => super::plan::AskTiming::JustInTime,
            Some("Never") => super::plan::AskTiming::Never,
            _ => super::plan::AskTiming::PreExecution, // Safe default
        };

        // Extract discovery_timing
        let discovery_timing = match json["discovery_timing"].as_str() {
            Some("PreExecution") => super::plan::DiscoveryTiming::PreExecution,
            Some("JustInTime") => super::plan::DiscoveryTiming::JustInTime,
            Some("Auto") => super::plan::DiscoveryTiming::Auto,
            _ => super::plan::DiscoveryTiming::Auto, // Safe default
        };

        // Extract inference hints from LLM + add our context hints
        if let Some(llm_hints) = json["inference_hints"].as_array() {
            for hint in llm_hints {
                if let Some(hint_str) = hint.as_str() {
                    inference_hints.push(hint_str.to_string());
                }
            }
        }

        Ok((priority, ask_timing, discovery_timing, inference_hints))
    }

    // =========================================================================
    // PROGRESSIVE PARAMETER RESOLUTION METHODS
    // =========================================================================

    /// Get confidence threshold based on parameter priority
    fn get_confidence_threshold(priority: &QuestionPriority) -> f64 {
        match priority {
            QuestionPriority::Critical => confidence_thresholds::CRITICAL,
            QuestionPriority::PreExecution => confidence_thresholds::PRE_EXECUTION,
            QuestionPriority::JustInTime => confidence_thresholds::JUST_IN_TIME,
            QuestionPriority::Optional | QuestionPriority::Inferrable => {
                confidence_thresholds::OPTIONAL
            },
        }
    }

    /// Phase 2.1: Try to infer parameter value from context
    async fn try_progressive_inference(
        &self,
        param: &UnresolvedInput,
        planning_context: &PlanningContext,
        user_message: &str,
        execution_id: &str,
        confidence_threshold: f64,
        prompt_identity: Option<&crate::magician_v2::execution::PromptIdentityContext>,
    ) -> Result<Option<ResolvedParameter>> {
        let inference_service = match &self.inference_service {
            Some(svc) => svc,
            None => return Ok(None),
        };

        // Build parameter context from current state
        let param_context = ParameterContext {
            execution_id: execution_id.to_string(),
            user_message: user_message.to_string(),
            tool_context: None, // No tool metadata available during planning phase
            slot_context: planning_context
                .resolved_params()
                .iter()
                .map(|(k, v)| (k.clone(), v.value.clone()))
                .collect(),
            observations: vec![],
            stage: WorkflowStage::Planning,
            prompt_identity: prompt_identity.cloned(),
        };

        match inference_service.infer(param, &param_context).await {
            Ok(result) if result.value.is_some() && result.confidence >= confidence_threshold => {
                info!(
                    "[ATOMIC-PROGRESSIVE] ✅ Inferred '{}' with confidence {:.2} (threshold: {:.2})",
                    param.parameter, result.confidence, confidence_threshold
                );
                Ok(Some(ResolvedParameter::from_inference(
                    param.parameter.clone(),
                    result,
                )))
            },
            Ok(result) => {
                debug!(
                    "[ATOMIC-PROGRESSIVE] ⚠️ Inference for '{}' below threshold: {:.2} < {:.2}",
                    param.parameter, result.confidence, confidence_threshold
                );
                Ok(None)
            },
            Err(e) => {
                debug!(
                    "[ATOMIC-PROGRESSIVE] Inference failed for '{}': {}",
                    param.parameter, e
                );
                Ok(None)
            },
        }
    }

    /// Phase 2.2: Try to discover parameter value autonomously
    async fn try_progressive_discovery(
        &self,
        param: &UnresolvedInput,
        planning_context: &PlanningContext,
        user_message: &str,
        execution_id: &str,
        confidence_threshold: f64,
        prompt_identity: Option<&crate::magician_v2::execution::PromptIdentityContext>,
    ) -> Result<Option<ResolvedParameter>> {
        let discovery_service = match &self.discovery_service {
            Some(svc) => svc,
            None => return Ok(None),
        };

        // Build parameter context
        let param_context = ParameterContext {
            execution_id: execution_id.to_string(),
            user_message: user_message.to_string(),
            tool_context: None, // No tool metadata available during planning phase
            slot_context: planning_context
                .resolved_params()
                .iter()
                .map(|(k, v)| (k.clone(), v.value.clone()))
                .collect(),
            observations: vec![],
            stage: WorkflowStage::Planning,
            prompt_identity: prompt_identity.cloned(),
        };

        match discovery_service.discover(param, &param_context).await {
            Ok(result) if result.value.is_some() && result.confidence >= confidence_threshold => {
                info!(
                    "[ATOMIC-PROGRESSIVE] ✅ Discovered '{}' with confidence {:.2} (threshold: {:.2})",
                    param.parameter, result.confidence, confidence_threshold
                );
                Ok(Some(ResolvedParameter::from_discovery(
                    param.parameter.clone(),
                    result,
                )))
            },
            Ok(result) => {
                debug!(
                    "[ATOMIC-PROGRESSIVE] ⚠️ Discovery for '{}' below threshold: {:.2} < {:.2}",
                    param.parameter, result.confidence, confidence_threshold
                );
                Ok(None)
            },
            Err(e) => {
                debug!(
                    "[ATOMIC-PROGRESSIVE] Discovery failed for '{}': {}",
                    param.parameter, e
                );
                Ok(None)
            },
        }
    }

    /// Phase 3: Determine if parameter should be asked upfront or deferred to JIT
    fn should_defer_to_execution(param: &UnresolvedInput) -> bool {
        matches!(param.priority, QuestionPriority::JustInTime)
            && matches!(param.ask_timing, super::plan::AskTiming::JustInTime)
    }

    /// Extract potential parameters from outline goals (lightweight pre-scan)
    async fn extract_parameters_from_outline(
        &self,
        outline: &AtomicOutlinePlan,
        _root_query: &str,
    ) -> Vec<UnresolvedInput> {
        let mut params = Vec::new();
        let mut seen = HashSet::new();

        for goal in &outline.goals {
            // Extract from prerequisites
            for prereq in &goal.prerequisites {
                if seen.insert(prereq.clone()) {
                    // Create lightweight UnresolvedInput for progressive resolution
                    params.push(UnresolvedInput {
                        id: format!("outline_param_{}", prereq),
                        parameter: prereq.clone(),
                        display_name: super::plan::humanize_parameter(prereq),
                        step_id: Some(goal.id.clone()),
                        linked_steps: vec![goal.id.clone()],
                        expected_type: Some("outline_prerequisite".to_string()),
                        json_schema: None,
                        prompt: format!("Parameter '{}' required for goal: {}", prereq, goal.title),
                        required: true,
                        notes: Some("from_outline".to_string()),
                        priority: QuestionPriority::PreExecution, // Default, will be refined
                        ask_timing: super::plan::AskTiming::PreExecution,
                        discovery_timing: super::plan::DiscoveryTiming::Auto,
                        default_value: None,
                        inference_hints: vec![
                            format!("goal:{}", goal.title),
                            format!("description:{}", goal.description),
                        ],
                        inference_threshold: 0.7,
                        auto_fill: None,
                        auto_fill_confidence: None,
                        source: super::plan::InputSource::Planner,
                        created_at: None,
                        updated_at: None,
                        status: None,
                    });
                }
            }

            // Recursively process child goals
            Self::extract_from_goal_children(&goal.children, &mut params, &mut seen);
        }

        params
    }

    /// Classify parameter priorities using LLM (Phase 1.1 - Priority Classification)
    ///
    /// Takes extracted parameters with default priorities and assigns context-aware
    /// priorities using LLM-based classification.
    async fn classify_parameter_priorities(
        &self,
        params: Vec<UnresolvedInput>,
        user_message: &str,
        outline: &AtomicOutlinePlan,
        context: &StrategyContext,
    ) -> Result<Vec<UnresolvedInput>> {
        if params.is_empty() {
            return Ok(params);
        }

        info!(
            "[PRIORITY-CLASSIFICATION] Classifying priorities for {} parameters",
            params.len()
        );

        let llm_service = self
            .llm_service
            .as_ref()
            .ok_or_else(|| anyhow!("LLM service not available for priority classification"))?;

        let prompt_manager = self
            .prompt_manager
            .as_ref()
            .ok_or_else(|| anyhow!("Prompt manager not available for priority classification"))?;

        let mut classified_params = Vec::new();

        // Classify each parameter individually (allows for per-parameter context)
        for param in params {
            // Find the goal this parameter belongs to for context
            let goal_context = Self::find_goal_for_parameter(&param, outline);

            // Build variables for prompt rendering
            let mut variables = HashMap::new();
            variables.insert("parameter_name".to_string(), param.parameter.clone());
            variables.insert(
                "step_tool".to_string(),
                goal_context
                    .map(|g| g.title.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
            );
            variables.insert(
                "step_description".to_string(),
                goal_context
                    .map(|g| g.description.clone())
                    .unwrap_or_else(|| param.prompt.clone()),
            );
            variables.insert("user_request".to_string(), user_message.to_string());

            // Get rendered prompt
            match prompt_manager
                .get_rendered_prompt(
                    names::PARAM_PRIORITY_CLASSIFICATION,
                    versions::PARAM_PRIORITY_CLASSIFICATION,
                    variables,
                )
                .await
            {
                Ok(prompt) => {
                    // Call LLM for classification (use ParameterExtraction operation - nano tier)
                    let llm_started = std::time::Instant::now();
                    match llm_service
                        .generate_for_operation(&LLMOperation::ParameterExtraction, &prompt)
                        .await
                    {
                        Ok(response) => {
                            context.emit_operation_llm_telemetry(
                                LLMOperation::ParameterExtraction.as_str(),
                                &response,
                                llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                            );
                            // Parse LLM response
                            match serde_json::from_str::<Value>(&response.content) {
                                Ok(json) => {
                                    let mut updated_param = param.clone();

                                    // Extract priority
                                    if let Some(priority_str) = json["priority"].as_str() {
                                        updated_param.priority = match priority_str {
                                            "Critical" => QuestionPriority::Critical,
                                            "PreExecution" => QuestionPriority::PreExecution,
                                            "JustInTime" => QuestionPriority::JustInTime,
                                            "Optional" => QuestionPriority::Optional,
                                            "Inferrable" => QuestionPriority::Inferrable,
                                            _ => {
                                                warn!(
                                                    "[PRIORITY-CLASSIFICATION] Invalid priority '{}' for '{}', defaulting to PreExecution",
                                                    priority_str, param.parameter
                                                );
                                                QuestionPriority::PreExecution
                                            },
                                        };
                                    }

                                    // Extract ask_timing
                                    if let Some(ask_timing_str) = json["ask_timing"].as_str() {
                                        updated_param.ask_timing = match ask_timing_str {
                                            "PreExecution" => super::plan::AskTiming::PreExecution,
                                            "JustInTime" => super::plan::AskTiming::JustInTime,
                                            "Never" => super::plan::AskTiming::Never,
                                            _ => super::plan::AskTiming::PreExecution,
                                        };
                                    }

                                    // Extract discovery_timing
                                    if let Some(discovery_timing_str) =
                                        json["discovery_timing"].as_str()
                                    {
                                        updated_param.discovery_timing = match discovery_timing_str
                                        {
                                            "PreExecution" => {
                                                super::plan::DiscoveryTiming::PreExecution
                                            },
                                            "JustInTime" => {
                                                super::plan::DiscoveryTiming::JustInTime
                                            },
                                            "Auto" => super::plan::DiscoveryTiming::Auto,
                                            _ => super::plan::DiscoveryTiming::Auto,
                                        };
                                    }

                                    // Extract inference_hints
                                    if let Some(hints) = json["inference_hints"].as_array() {
                                        updated_param.inference_hints = hints
                                            .iter()
                                            .filter_map(|v| v.as_str().map(String::from))
                                            .collect();
                                    }

                                    // Apply security overrides (destructive actions always Critical)
                                    if Self::is_destructive_action(&updated_param.parameter) {
                                        debug!(
                                            "[PRIORITY-CLASSIFICATION] Security override: '{}' → Critical (destructive action)",
                                            updated_param.parameter
                                        );
                                        updated_param.priority = QuestionPriority::Critical;
                                        updated_param.ask_timing =
                                            super::plan::AskTiming::PreExecution;
                                    }

                                    info!(
                                        "[PRIORITY-CLASSIFICATION] '{}' → {:?} (ask: {:?}, discovery: {:?})",
                                        updated_param.parameter,
                                        updated_param.priority,
                                        updated_param.ask_timing,
                                        updated_param.discovery_timing
                                    );

                                    classified_params.push(updated_param);
                                },
                                Err(e) => {
                                    warn!(
                                        "[PRIORITY-CLASSIFICATION] Failed to parse LLM response for '{}': {}. Using defaults.",
                                        param.parameter, e
                                    );
                                    classified_params.push(param);
                                },
                            }
                        },
                        Err(e) => {
                            warn!(
                                "[PRIORITY-CLASSIFICATION] LLM call failed for '{}': {}. Using defaults.",
                                param.parameter, e
                            );
                            classified_params.push(param);
                        },
                    }
                },
                Err(e) => {
                    warn!(
                        "[PRIORITY-CLASSIFICATION] Failed to load prompt for '{}': {}. Using defaults.",
                        param.parameter, e
                    );
                    classified_params.push(param);
                },
            }
        }

        info!(
            "[PRIORITY-CLASSIFICATION] Classification complete: {} parameters processed",
            classified_params.len()
        );

        Ok(classified_params)
    }

    /// Helper: Find the goal that contains this parameter
    fn find_goal_for_parameter<'a>(
        param: &UnresolvedInput,
        outline: &'a AtomicOutlinePlan,
    ) -> Option<&'a OutlineGoal> {
        if let Some(step_id) = &param.step_id {
            // Search top-level goals
            for goal in &outline.goals {
                if &goal.id == step_id {
                    return Some(goal);
                }
                // Search nested children
                if let Some(child) = Self::find_goal_in_children(step_id, &goal.children) {
                    return Some(child);
                }
            }
        }
        None
    }

    /// Helper: Recursively search for goal in children
    fn find_goal_in_children<'a>(
        step_id: &str,
        children: &'a [OutlineGoal],
    ) -> Option<&'a OutlineGoal> {
        for child in children {
            if child.id == step_id {
                return Some(child);
            }
            if let Some(nested) = Self::find_goal_in_children(step_id, &child.children) {
                return Some(nested);
            }
        }
        None
    }

    /// Helper: Check if a parameter name indicates a destructive action
    ///
    /// Security override: Parameters matching destructive patterns are always Critical
    /// to prevent accidental data loss even if LLM classifies them differently.
    fn is_destructive_action(param_name: &str) -> bool {
        let destructive_keywords = [
            "delete", "destroy", "remove", "drop", "truncate", "purge", "erase", "wipe", "clear",
            "reset", "nuke",
        ];
        let lower = param_name.to_lowercase();
        destructive_keywords.iter().any(|kw| lower.contains(kw))
    }

    /// Helper to recursively extract from nested goals
    fn extract_from_goal_children(
        children: &[OutlineGoal],
        params: &mut Vec<UnresolvedInput>,
        seen: &mut HashSet<String>,
    ) {
        for child in children {
            for prereq in &child.prerequisites {
                if seen.insert(prereq.clone()) {
                    params.push(UnresolvedInput {
                        id: format!("outline_param_{}", prereq),
                        parameter: prereq.clone(),
                        display_name: super::plan::humanize_parameter(prereq),
                        step_id: Some(child.id.clone()),
                        linked_steps: vec![child.id.clone()],
                        expected_type: Some("outline_prerequisite".to_string()),
                        json_schema: None,
                        prompt: format!(
                            "Parameter '{}' required for goal: {}",
                            prereq, child.title
                        ),
                        required: true,
                        notes: Some("from_outline".to_string()),
                        priority: QuestionPriority::PreExecution,
                        ask_timing: super::plan::AskTiming::PreExecution,
                        discovery_timing: super::plan::DiscoveryTiming::Auto,
                        default_value: None,
                        inference_hints: vec![
                            format!("goal:{}", child.title),
                            format!("description:{}", child.description),
                        ],
                        inference_threshold: 0.7,
                        auto_fill: None,
                        auto_fill_confidence: None,
                        source: super::plan::InputSource::Planner,
                        created_at: None,
                        updated_at: None,
                        status: None,
                    });
                }
            }
            Self::extract_from_goal_children(&child.children, params, seen);
        }
    }
}

impl Default for AtomicCompositionStrategy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExplorationStrategy for AtomicCompositionStrategy {
    async fn explore(
        &mut self,
        context: &StrategyContext,
        query: &str,
    ) -> Result<ExplorationResult> {
        let start_time = std::time::Instant::now();
        info!(
            "[MAGICIAN-V2-STRATEGY] Atomic Composition strategy: composing atomic sequence for \
             '{}'",
            query
        );

        // Broadcast initial progress
        if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
            &context.event_broadcaster,
            &context.execution_id,
            &context.correlation_id,
        ) {
            broadcaster.exploration_progress(
                execution_id,
                correlation_id,
                0,
                0,
                0.0,
                "Analyzing available atomic tools".to_string(),
                10.0,
            );
        }

        // Get available tools using the discovery service (includes security
        // filtering).  When the agent has capability/excluded packs configured,
        // narrow the catalog so the planner only sees permitted tools.
        // Uses merged_agent_tools() to tag delegate tools with providing_agent_id.
        let tool_infos = context
            .merged_agent_tools()
            .await
            .map_err(|e| anyhow!("Failed to get tools: {}", e))?;

        debug!(
            "[MAGICIAN-V2-STRATEGY] Retrieved {} tools from discovery service",
            tool_infos.len()
        );

        // Index the catalog by composition category (no narrowing happens here)
        let atomic_tools = group_atomic_tools(&tool_infos);

        if atomic_tools.total_count == 0 {
            return Err(anyhow!("No atomic tools available for composition"));
        }

        // Broadcast tool discovery progress
        if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
            &context.event_broadcaster,
            &context.execution_id,
            &context.correlation_id,
        ) {
            broadcaster.exploration_progress(
                execution_id,
                correlation_id,
                0,
                0,
                0.0,
                format!(
                    "Found {} atomic tools, generating composition plan",
                    atomic_tools.total_count
                ),
                40.0,
            );
        }

        let (base_llm_calls, base_llm_tokens) = context.llm_usage_snapshot();

        // Generate atomic composition plan with validation + retry
        let (atomic_plan, mut plan_graph) = self
            .compose_with_retry(context, query, &atomic_tools, 3)
            .await?;

        // Broadcast plan generation complete
        if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
            &context.event_broadcaster,
            &context.execution_id,
            &context.correlation_id,
        ) {
            broadcaster.exploration_progress(
                execution_id,
                correlation_id,
                1,
                0,
                atomic_plan.confidence as f64,
                format!(
                    "Generated atomic plan with {} steps",
                    atomic_plan.plan.len()
                ),
                80.0,
            );
        }

        let execution_time = start_time.elapsed();

        // Create root node with atomic plan
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

        // Mark as terminal with high confidence (atomic plan is deterministic)
        root_node.is_terminal = true;
        root_node.confidence = atomic_plan.confidence;
        root_node.visits = 1;
        root_node.value = atomic_plan.confidence;

        let mut slots_auto_filled = 0usize;
        if let Some(elicitation_manager) = context.elicitation_manager.as_ref() {
            // Register unresolved inputs directly (they're already UnresolvedInput now)
            let unresolved_inputs = plan_graph.unresolved_inputs.clone();

            if !unresolved_inputs.is_empty() {
                let total_slots = unresolved_inputs.len();
                let auto_filled_input_ids: HashSet<String> = unresolved_inputs
                    .iter()
                    .filter(|input| input.auto_fill.is_some())
                    .map(|input| input.id.clone())
                    .collect();
                let auto_filled_count = auto_filled_input_ids.len();

                match elicitation_manager.register_slots(unresolved_inputs).await {
                    Ok(_) => {
                        slots_auto_filled = auto_filled_count;
                        if !auto_filled_input_ids.is_empty() {
                            plan_graph.unresolved_inputs.retain(|input| {
                                if input.notes.as_deref() == Some("slot_prerequisite") {
                                    !auto_filled_input_ids.contains(&input.id)
                                } else {
                                    true
                                }
                            });
                        }
                        info!(
                            "[MAGICIAN-V2-STRATEGY] Registered {} slot prerequisites ({} auto-filled from parameters)",
                            total_slots,
                            auto_filled_count
                        );
                    },
                    Err(err) => {
                        warn!(
                            "[MAGICIAN-V2-STRATEGY] Failed to register elicitation slots: {}",
                            err
                        );
                    },
                }
            }
        }

        // Store atomic plan in node's task context metadata
        let plan_json = serde_json::to_string(&atomic_plan).unwrap_or_else(|_| "{}".to_string());
        root_node
            .task_context
            .parent_metadata
            .insert("atomic_plan".to_string(), plan_json);

        // Build result
        let mut all_nodes = HashMap::new();
        all_nodes.insert(root_id.clone(), root_node.clone());

        let best_path = vec![root_id];

        // Update metrics
        let (llm_calls_total, llm_tokens_total) = context.llm_usage_snapshot();
        let llm_calls_strategy_only = llm_calls_total.saturating_sub(base_llm_calls);
        let llm_tokens = llm_tokens_total.saturating_sub(base_llm_tokens);

        self.metrics.nodes_explored = 1;
        self.metrics.llm_calls_made = llm_calls_strategy_only;
        self.metrics.average_confidence = atomic_plan.confidence;
        self.metrics.exploration_depth = 0;
        self.metrics.time_per_iteration_ms = execution_time.as_millis() as f32;

        let mut parameter_statistics =
            ParameterStatistics::from_atomic_plan(&root_node, &plan_graph);
        parameter_statistics.slots_auto_filled = slots_auto_filled;

        info!(
            "[MAGICIAN-V2-STRATEGY] Resource usage: {} total LLM calls (including {} from query analysis + elicitation, {} from strategy)",
            llm_calls_total, base_llm_calls, llm_calls_strategy_only
        );

        let result = ExplorationResult {
            root_node: root_node.clone(),
            all_nodes,
            best_path,
            confidence: atomic_plan.confidence,
            plan: Some(plan_graph.clone()),
            resources_consumed: ResourceUsage {
                llm_calls: llm_calls_total, // Use total including query analysis + elicitation
                time_ms: execution_time.as_millis() as u64,
                tokens: llm_tokens.min(u32::MAX as u64) as u32,
            },
            strategy_metadata: StrategyMetadata {
                strategy_type: StrategyType::AtomicComposition,
                iterations: 1,
                nodes_explored: 1,
                max_depth: 0,
                completed: true,
                termination_reason: None,
            },
            parameter_statistics,
        };

        info!(
            "[MAGICIAN-V2-STRATEGY] Atomic composition complete: {} steps, confidence={:.3}, \
             time={}ms",
            atomic_plan.plan.len(),
            result.confidence,
            result.resources_consumed.time_ms
        );

        // Save exploration result (if conversation store is available)
        if let (Some(store), Some(execution_id), Some(turn_id)) = (
            &context.conversation_store,
            &context.execution_id,
            &context.turn_id,
        ) {
            debug!("[MAGICIAN-V2-STRATEGY] AtomicComposition: Saving exploration result");

            // Read existing turn to preserve accumulated strategies_attempted
            let mut strategies_attempted = vec![StrategyType::AtomicComposition];
            let mut retry_attempts = 0;
            if let Ok(existing_turn) = store.get_turn(execution_id, turn_id).await {
                if let Some(existing_metadata) = existing_turn.processing_metadata {
                    // Preserve existing strategies and append current one if not already present
                    strategies_attempted = existing_metadata.strategies_attempted.clone();
                    if !strategies_attempted.contains(&StrategyType::AtomicComposition) {
                        strategies_attempted.push(StrategyType::AtomicComposition);
                    }
                    retry_attempts = existing_metadata.retry_attempts;
                }
            }

            let _metadata = crate::magician_v2::orchestrator::ProcessingMetadata {
                total_duration_ms: result.resources_consumed.time_ms,
                strategy_duration_ms: result.resources_consumed.time_ms,
                strategy_selected: StrategyType::AtomicComposition,
                processed_at: chrono::Utc::now(),
                retry_attempts,
                strategies_attempted,
                final_strategy: StrategyType::AtomicComposition,
                current_stage: None,
                current_provider: None,
            };

            // Note: incremental saves are disabled; orchestrator persists all attempts at the end
            // to allow the frontend to see complete escalation history.
            // if let Err(e) = store.store_strategy_attempts(...) {
            //     warn!("[MAGICIAN-V2-STRATEGY] AtomicComposition: Failed to save exploration result: {}", e);
            // } else {
            //     debug!("[MAGICIAN-V2-STRATEGY] AtomicComposition: Successfully saved exploration result");
            // }
        }

        // Optionally broadcast completion progress if an event broadcaster is available.
        if let (Some(broadcaster), Some(execution_id), Some(correlation_id)) = (
            &context.event_broadcaster,
            &context.execution_id,
            &context.correlation_id,
        ) {
            broadcaster.exploration_progress(
                execution_id,
                correlation_id,
                1,
                0,
                result.confidence as f64,
                format!(
                    "Atomic composition complete with {} steps",
                    atomic_plan.plan.len()
                ),
                100.0,
            );
        }

        Ok(result)
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::AtomicComposition
    }

    fn can_continue(&self, _budget: &ResourceBudget) -> bool {
        // Atomic composition completes in single iteration
        true
    }
}

impl FailureRecovery for AtomicCompositionStrategy {
    fn handle_failure(&self, failure: StrategyFailure, _context: &StrategyContext) -> NextAction {
        // Atomic composition is the terminal strategy - no further escalation
        match failure {
            StrategyFailure::NoToolsFound => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Atomic composition failed: no atomic tools available"
                );
                NextAction::Abort("No atomic tools available".to_string())
            },
            StrategyFailure::InternalError(msg) => {
                warn!("[MAGICIAN-V2-STRATEGY] Atomic composition error: {}", msg);
                NextAction::Abort(format!("LLM reasoning or internal error: {}", msg))
            },
            StrategyFailure::LowConfidence(conf) => {
                warn!(
                    "[MAGICIAN-V2-STRATEGY] Atomic composition low confidence: {:.2}",
                    conf
                );
                NextAction::Abort(format!("Low confidence atomic plan: {:.2}", conf))
            },
            StrategyFailure::DecompositionFailed => {
                warn!("[MAGICIAN-V2-STRATEGY] Atomic composition decomposition failed");
                NextAction::Abort("Failed to generate atomic composition plan".to_string())
            },
            StrategyFailure::TimeoutExceeded => {
                warn!("[MAGICIAN-V2-STRATEGY] Atomic composition timeout exceeded");
                NextAction::Abort("Timeout during atomic composition".to_string())
            },
            StrategyFailure::LLMBudgetExceeded => {
                warn!("[MAGICIAN-V2-STRATEGY] Atomic composition LLM budget exceeded");
                NextAction::Abort("LLM budget exceeded during atomic composition".to_string())
            },
        }
    }

    fn should_abort(
        &self,
        _context: &StrategyContext,
        _current_result: &ExplorationResult,
    ) -> bool {
        // Atomic composition completes in single step
        false
    }
}

impl StrategyIntrospection for AtomicCompositionStrategy {
    fn get_debug_info(&self) -> serde_json::Value {
        serde_json::json!({
            "strategy": "AtomicComposition",
            "description": "Atomic tool composition using llm-reasoning (terminal fallback)",
            "llm_calls": 1,
            "features": [
                "Uses llm-reasoning model",
                "Atomic tools only",
                "Direct execution plan",
                "No tool matching needed",
                "Terminal strategy"
            ],
            "metrics": {
                "nodes_explored": self.metrics.nodes_explored,
                "llm_calls_made": self.metrics.llm_calls_made,
                "average_confidence": self.metrics.average_confidence,
                "time_per_iteration_ms": self.metrics.time_per_iteration_ms
            }
        })
    }

    fn get_metrics(&self) -> StrategyMetrics {
        self.metrics.clone()
    }
}

// =============================================================================
// ATOMIC COMPOSITION TYPES
// =============================================================================

/// Hierarchical outline produced during the first planning phase
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AtomicOutlinePlan {
    #[serde(default)]
    pub goals: Vec<OutlineGoal>,
    #[serde(default)]
    pub overall_confidence: Option<f32>,
}

/// Outline goal representing a top-level or nested objective
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutlineGoal {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub expected_outcome: Option<String>,
    #[serde(default)]
    pub session_hint: Option<String>,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    /// Nested sub-goals.
    #[serde(default)]
    pub children: Vec<OutlineGoal>,
}

impl AtomicOutlinePlan {
    pub fn format_for_prompt(&self) -> String {
        let mut output = String::new();
        for goal in &self.goals {
            Self::format_goal(goal, 0, &mut output);
        }
        output
    }

    fn format_goal(goal: &OutlineGoal, depth: usize, buffer: &mut String) {
        let indent = "  ".repeat(depth);
        let _ = writeln!(
            buffer,
            "{}- [{}] {} :: {}",
            indent, goal.id, goal.title, goal.description
        );
        if let Some(hint) = &goal.session_hint {
            let _ = writeln!(buffer, "{}  session_hint: {}", indent, hint);
        }
        if !goal.prerequisites.is_empty() {
            let joined = goal.prerequisites.join(", ");
            let _ = writeln!(buffer, "{}  prerequisites: {}", indent, joined);
        }
        for child in &goal.children {
            Self::format_goal(child, depth + 1, buffer);
        }
    }
}

/// Atomic composition plan from llm-reasoning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicCompositionPlan {
    /// Sequence of atomic tool steps
    pub plan: Vec<AtomicStep>,
    /// Execution strategy (sequential, parallel, etc.)
    pub execution_strategy: String,
    /// Estimated duration in milliseconds
    pub estimated_duration_ms: u64,
    /// Confidence in this plan
    pub confidence: f32,
}

/// Single atomic tool step
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicStep {
    /// Step number
    pub step: usize,
    /// Atomic tool name
    pub tool: String,
    /// Human-readable description
    pub description: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
    /// Expected output description
    pub expected_output: String,
    /// Additional rationale for why this step is required
    #[serde(default)]
    pub rationale: Option<String>,
    /// Explicit dependencies (step numbers) that must complete before executing
    #[serde(default)]
    pub dependencies: Vec<usize>,
    /// Session affinity metadata for browser contexts
    #[serde(default)]
    pub session: Option<SessionMetadata>,
    /// Execution prerequisites (slots, consent, retry budget)
    #[serde(default)]
    pub prerequisites: Option<StepPrerequisites>,
    /// Confidence score provided by the planner
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Observation checkpoint hints for executor validation
    #[serde(default)]
    pub observation: Option<ObservationCheckpoint>,
}

/// Session metadata describing how a step interacts with browser context
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionMetadata {
    /// Identifier of the session/tab to use
    #[serde(default)]
    pub session_id: Option<String>,
    /// Whether a new session should be created
    #[serde(default)]
    pub new_session: bool,
    /// Whether an existing session should be reused after temporary focus loss
    #[serde(default)]
    pub reuse_session: bool,
}

/// Requirements that must be satisfied before a step may execute
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepPrerequisites {
    /// Slot identifiers that must be filled
    #[serde(default)]
    pub required_slots: Vec<String>,
    /// Consent or policy flags that must be acknowledged
    #[serde(default)]
    pub consent_flags: Vec<String>,
    /// Optional retry budget to enforce resilience
    pub retry_budget: Option<u32>,
}

/// Expected state that the executor should observe after running a step
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObservationCheckpoint {
    /// Text or DOM cues that should be present
    #[serde(default)]
    pub expected_state: Option<String>,
    /// Optional screenshot filename or descriptor for verification
    #[serde(default)]
    pub screenshot: Option<String>,
    /// Free-form notes guiding recovery if the checkpoint fails
    #[serde(default)]
    pub notes: Option<String>,
}

impl AtomicCompositionStrategy {
    /// Extract tool_selection prerequisites from outline goals and convert to SlotRecords.
    /// This enables the elicitation system to ask users which tool approach they prefer
    /// when multiple valid approaches exist (e.g., GitHub API vs Browser UI).
    fn extract_tool_selection_slots(
        outline: &AtomicOutlinePlan,
        workflow_id: &str,
    ) -> Vec<crate::magician_v2::slot_graph::SlotRecord> {
        use crate::magician_v2::slot_graph::{
            ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType,
        };
        use chrono::Utc;

        let mut slots = Vec::new();
        let mut processed = std::collections::HashSet::new();

        // Recursively extract from all goals
        fn extract_from_goal(
            goal: &OutlineGoal,
            slots: &mut Vec<SlotRecord>,
            processed: &mut std::collections::HashSet<String>,
            workflow_id: &str,
        ) {
            for prereq in &goal.prerequisites {
                if prereq.starts_with("tool_selection:") && !processed.contains(prereq) {
                    // Extract service name (e.g., "tool_selection:github_access" → "github_access")
                    let slot_id = prereq.clone();

                    let slot = SlotRecord {
                        id: slot_id.clone(),
                        slot_type: SlotType::ToolSelection,
                        value: serde_json::Value::Null, // Unfilled - needs user selection
                        confidence: 0.0,                // Zero confidence = needs elicitation
                        provenance: vec![ProvenanceRecord {
                            source: ProvenanceSource::OutlinePrerequisite,
                            timestamp: Utc::now(),
                        }],
                        evidence_links: vec![format!("outline_goal:{}", goal.id)],
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    };

                    slots.push(slot);
                    processed.insert(slot_id);
                }
            }

            // Recursively process children
            for child in &goal.children {
                extract_from_goal(child, slots, processed, workflow_id);
            }
        }

        for goal in &outline.goals {
            extract_from_goal(goal, &mut slots, &mut processed, workflow_id);
        }

        slots
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::atomic::{AtomicU32, AtomicU64};

    use super::*;
    use crate::magician_v2::{
        query_analysis::operation_llm_router::SimplifiedLLMResponse,
        strategy::plan::{AskTiming, DiscoveryTiming, InputSource},
    };
    use serde_json::json;

    #[test]
    fn test_atomic_strategy_creation() {
        let strategy = AtomicCompositionStrategy::new();
        assert_eq!(strategy.strategy_type(), StrategyType::AtomicComposition);
    }

    #[test]
    fn test_parse_atomic_plan() {
        let strategy = AtomicCompositionStrategy::new();

        let response = r#"{
            "plan": [
                {
                    "step": 1,
                    "tool": "shell",
                    "description": "Test command",
                    "parameters": {"command": "echo test"},
                    "expected_output": "test"
                }
            ],
            "execution_strategy": "sequential",
            "estimated_duration_ms": 100,
            "confidence": 0.9
        }"#;

        let plan = strategy.parse_atomic_plan(response, "test query").unwrap();
        assert_eq!(plan.plan.len(), 1);
        assert_eq!(plan.plan[0].tool, "shell");
        assert_eq!(plan.confidence, 0.9);
    }

    #[test]
    fn atomic_plan_native_schema_requires_textual_observation_and_consent_ids() {
        let schema: Value =
            serde_json::from_str(AtomicCompositionStrategy::ATOMIC_PLAN_TOOL_SCHEMA)
                .expect("atomic plan native schema");
        assert_eq!(schema["name"], "emit_atomic_composition_plan");
        assert_eq!(
            schema["parameters"]["properties"]["plan"]["items"]["properties"]["observation"]
                ["properties"]["expected_state"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            schema["parameters"]["properties"]["plan"]["items"]["properties"]["prerequisites"]
                ["properties"]["consent_flags"]["items"]["type"],
            "string"
        );
    }

    #[test]
    fn test_parse_atomic_outline() {
        let strategy = AtomicCompositionStrategy::new();
        let response = r#"{
            "goals": [
                {
                    "id": "G1",
                    "title": "Prepare dashboard",
                    "description": "Open the dashboard and gather context",
                    "session_hint": "browser_main",
                    "prerequisites": ["need_credentials"],
                    "children": [
                        {
                            "id": "G1.1",
                            "title": "Log in",
                            "description": "Authenticate into the dashboard",
                            "children": []
                        }
                    ]
                }
            ],
            "overall_confidence": 0.75
        }"#;

        let outline = strategy
            .parse_atomic_outline(response, "test query")
            .expect("outline parsed");
        assert_eq!(outline.goals.len(), 1);
        assert_eq!(outline.goals[0].id, "G1");
        assert_eq!(outline.goals[0].children.len(), 1);
    }

    #[tokio::test]
    async fn test_convert_plan_respects_parallel_strategy() {
        let strategy = AtomicCompositionStrategy::new();

        let plan = AtomicCompositionPlan {
            plan: vec![
                AtomicStep {
                    step: 1,
                    tool: "shell".to_string(),
                    description: "Prepare workspace".to_string(),
                    parameters: json!({ "command": "echo setup" }),
                    expected_output: "workspace ready".to_string(),
                    rationale: Some("Prepare environment before UI work".to_string()),
                    dependencies: Vec::new(),
                    session: None,
                    prerequisites: None,
                    confidence: Some(0.9),
                    observation: None,
                },
                AtomicStep {
                    step: 2,
                    tool: "browser_navigate".to_string(),
                    description: "Open dashboard".to_string(),
                    parameters: json!({ "url": "https://example.com" }),
                    expected_output: "Dashboard visible".to_string(),
                    rationale: Some("Navigate to dashboard".to_string()),
                    dependencies: vec![1],
                    session: Some(SessionMetadata {
                        session_id: Some("browser_main".to_string()),
                        new_session: true,
                        reuse_session: false,
                    }),
                    prerequisites: Some(StepPrerequisites {
                        required_slots: vec!["auth_token".to_string()],
                        consent_flags: vec![],
                        retry_budget: Some(2),
                    }),
                    confidence: Some(0.85),
                    observation: None,
                },
                AtomicStep {
                    step: 3,
                    tool: "shell".to_string(),
                    description: "Collect logs".to_string(),
                    parameters: json!({ "command": "cat logs" }),
                    expected_output: "Logs captured".to_string(),
                    rationale: Some("Collect diagnostics in parallel".to_string()),
                    dependencies: Vec::new(),
                    session: None,
                    prerequisites: Some(StepPrerequisites {
                        required_slots: vec!["log_path".to_string()],
                        consent_flags: vec!["shell_consent".to_string()],
                        retry_budget: None,
                    }),
                    confidence: Some(0.8),
                    observation: None,
                },
            ],
            execution_strategy: "parallel".to_string(),
            estimated_duration_ms: 2000,
            confidence: 0.82,
        };

        let graph = strategy
            .convert_atomic_plan_to_graph(&plan, "parallel query", &create_mock_context(), true)
            .await;

        // Edges should respect declared dependency only (1 -> 2) and leave step-3 independent.
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "step-1" && edge.to == "step-2"));
        assert!(!graph
            .edges
            .iter()
            .any(|edge| edge.to == "step-3" && edge.reason == "sequential_dependency"));

        let slots: Vec<&UnresolvedInput> = graph
            .unresolved_inputs
            .iter()
            .filter(|input| input.notes.as_deref() == Some("slot_prerequisite"))
            .collect();
        assert_eq!(slots.len(), 2);
        assert!(slots
            .iter()
            .any(|input| input.parameter == "auth_token"
                && input.step_id.as_deref() == Some("step-2")));
        assert!(slots.iter().any(
            |input| input.parameter == "log_path" && input.step_id.as_deref() == Some("step-3")
        ));

        let consent: Vec<&UnresolvedInput> = graph
            .unresolved_inputs
            .iter()
            .filter(|input| input.notes.as_deref() == Some("consent_prerequisite"))
            .collect();
        assert_eq!(consent.len(), 1);
        assert_eq!(consent[0].parameter, "shell_consent");
    }

    #[tokio::test]
    async fn test_convert_plan_sequential_fallback() {
        let strategy = AtomicCompositionStrategy::new();

        let plan = AtomicCompositionPlan {
            plan: vec![
                AtomicStep {
                    step: 1,
                    tool: "shell".to_string(),
                    description: "Step one".to_string(),
                    parameters: json!({ "command": "echo 1" }),
                    expected_output: "1".to_string(),
                    rationale: None,
                    dependencies: Vec::new(),
                    session: None,
                    prerequisites: None,
                    confidence: None,
                    observation: None,
                },
                AtomicStep {
                    step: 2,
                    tool: "shell".to_string(),
                    description: "Step two".to_string(),
                    parameters: json!({ "command": "echo 2" }),
                    expected_output: "2".to_string(),
                    rationale: None,
                    dependencies: Vec::new(),
                    session: None,
                    prerequisites: None,
                    confidence: None,
                    observation: None,
                },
                AtomicStep {
                    step: 3,
                    tool: "shell".to_string(),
                    description: "Step three".to_string(),
                    parameters: json!({ "command": "echo 3" }),
                    expected_output: "3".to_string(),
                    rationale: None,
                    dependencies: Vec::new(),
                    session: None,
                    prerequisites: None,
                    confidence: None,
                    observation: None,
                },
            ],
            execution_strategy: "sequential".to_string(),
            estimated_duration_ms: 1500,
            confidence: 0.9,
        };

        let graph = strategy
            .convert_atomic_plan_to_graph(&plan, "seq query", &create_mock_context(), false)
            .await;

        assert_eq!(graph.edges.len(), 2);
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "step-1" && edge.to == "step-2"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.from == "step-2" && edge.to == "step-3"));
    }

    #[test]
    fn test_failure_handling() {
        let strategy = AtomicCompositionStrategy::new();
        let context = create_mock_context();

        let action = strategy.handle_failure(StrategyFailure::NoToolsFound, &context);

        match action {
            NextAction::Abort(_) => {}, // Expected
            _ => panic!("Expected Abort action"),
        }
    }

    fn create_mock_context() -> StrategyContext {
        use std::sync::Arc;

        use crate::{
            magician_v2::{
                query_analysis::{
                    CategoryAnalysis, ComplexityAnalysis, DependencyAnalysis, ResourceEstimate,
                    UnifiedQueryAnalysis,
                },
                tooling::ToolDiscoveryAdapter,
            },
            ExecutionContext, ToolDiscovery,
        };
        use runtime_core::{ToolCatalog, ToolMatching};

        struct MockToolDiscovery;
        #[async_trait::async_trait]
        impl ToolDiscovery for MockToolDiscovery {
            async fn find_best_match_with_context(
                &self,
                _task: &str,
                _context: &crate::ExecutionContext,
            ) -> crate::ToolMatchResult {
                crate::ToolMatchResult {
                    primary_match: None,
                    match_confidence: 0.0,
                    missing_capabilities: vec![],
                    parameter_coverage: 0.0,
                    executable: false,
                }
            }
            async fn find_multiple_matches_with_context(
                &self,
                _task: &str,
                _context: &crate::ExecutionContext,
            ) -> crate::MultipleToolMatchResult {
                crate::MultipleToolMatchResult {
                    matches: vec![],
                    match_strategies: vec![],
                    confidence_spread: 0.0,
                    recommended_approach: None,
                    any_executable: false,
                    aggregate_missing_capabilities: vec![],
                }
            }
            async fn is_tool_available(
                &self,
                _tool_name: &str,
                _context: &crate::ExecutionContext,
            ) -> bool {
                false
            }
            async fn get_tool_metadata(
                &self,
                _tool_name: &str,
                _context: &crate::ExecutionContext,
            ) -> Option<HashMap<String, serde_json::Value>> {
                None
            }
            async fn get_available_tools(&self, _context: &crate::ExecutionContext) -> Vec<String> {
                vec![]
            }
            async fn get_available_categories(
                &self,
                _context: &crate::ExecutionContext,
            ) -> Vec<String> {
                vec![]
            }
        }

        struct MockLLM;
        #[async_trait::async_trait]
        impl QueryAnalysisLLM for MockLLM {
            async fn generate_analysis(&self, _prompt: &str) -> Result<SimplifiedLLMResponse> {
                Ok(SimplifiedLLMResponse::content_only("{}".to_string()))
            }
        }

        struct MockPromptStore;
        #[async_trait::async_trait]
        impl crate::magician_v2::prompts::PromptStore for MockPromptStore {
            async fn get_prompt(
                &self,
                _name: &str,
                _version: &str,
            ) -> Result<crate::magician_v2::prompts::Prompt> {
                Ok(crate::magician_v2::prompts::Prompt {
                    name: "test".to_string(),
                    version: "1.0.0".to_string(),
                    content: "test".to_string(),
                    variables: vec![],
                    metadata: crate::magician_v2::prompts::PromptMetadata {
                        category: crate::magician_v2::prompts::PromptCategory::QueryAnalysis,
                        description: "test".to_string(),
                        author: "test".to_string(),
                        created_at: chrono::Utc::now(),
                        tags: vec![],
                        changelog: String::new(),
                        estimated_tokens: Some(100),
                    },
                })
            }
            async fn list_versions(&self, _name: &str) -> Result<Vec<String>> {
                Ok(vec![])
            }
            async fn list_prompt_names(&self) -> Result<Vec<String>> {
                Ok(vec![])
            }
            async fn save_prompt(
                &self,
                _prompt: &crate::magician_v2::prompts::Prompt,
            ) -> Result<()> {
                Ok(())
            }
            async fn delete_prompt(&self, _name: &str, _version: &str) -> Result<()> {
                Ok(())
            }
            async fn prompt_exists(&self, _name: &str, _version: &str) -> Result<bool> {
                Ok(true)
            }
            async fn latest_version(&self, _name: &str) -> Result<String> {
                Ok("1.0.0".to_string())
            }
            async fn initialize(&self) -> Result<()> {
                Ok(())
            }
            async fn health_check(&self) -> Result<bool> {
                Ok(true)
            }
        }

        let tool_discovery = Arc::new(MockToolDiscovery) as Arc<dyn ToolDiscovery>;
        let tool_adapter = Arc::new(ToolDiscoveryAdapter::new(tool_discovery));
        let tool_catalog: Arc<dyn ToolCatalog> = tool_adapter.clone();
        let tool_matching: Arc<dyn ToolMatching> = tool_adapter.clone();

        let analysis = UnifiedQueryAnalysis {
            original_query: "test".to_string(),
            complexity: ComplexityAnalysis {
                score: 0.5,
                factors: vec![],
                reasoning: "test".to_string(),
            },
            categories: CategoryAnalysis {
                categories: vec![],
                reasoning: "test".to_string(),
            },
            dependencies: DependencyAnalysis {
                is_multi_step: false,
                dependencies: vec![],
                workflow_steps: vec![],
                reasoning: "test".to_string(),
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

        StrategyContext {
            query_analysis: analysis.clone(),
            resource_budget: ResourceBudget::from_analysis(&analysis),
            suggested_categories: vec![],
            tool_catalog,
            tool_matching,
            execution_context: ExecutionContext::default(),
            llm_service: Arc::new(MockLLM),
            prompt_manager: Arc::new(crate::magician_v2::prompts::PromptManager::new(Arc::new(
                MockPromptStore,
            ))),
            v2_tool_matcher: None,
            execution_id: None,
            correlation_id: None,
            turn_id: None,
            conversation_store: None,
            event_broadcaster: None,
            llm_call_counter: Arc::new(AtomicU32::new(0)),
            llm_token_counter: Arc::new(AtomicU64::new(0)),
            elicitation_manager: None,
            planning_snapshot: None,
            stage_context: crate::magician_v2::state_tracker::StageContext::PlanningBootstrap,
            budget_ledger: None,
            clarified_task: None,
            slot_graph: Vec::new(),
            slot_diff: None,
            stage_resume_policy: crate::magician_v2::state_tracker::StageResumePolicy::default(),
            allow_consent_slots: true, // Allow consent slots in tests by default
            tools: Vec::new(),
            excluded_tools: Vec::new(),
            denied_tools: Vec::new(),
            delegate_tool_catalog: HashMap::new(),
            planner_agent_catalog: Vec::new(),
            available_procedure_skills: Vec::new(),
        }
    }

    // ========== Priority Classification Tests ==========

    #[test]
    fn test_is_destructive_action() {
        // Should detect destructive keywords
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "delete_user"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "confirm_delete"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "destroy_database"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "remove_all"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "DROP_TABLE"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "truncate_log"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "purge_cache"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "erase_disk"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "wipe_data"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "clear_history"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "reset_config"
        ));
        assert!(AtomicCompositionStrategy::is_destructive_action(
            "nuke_everything"
        ));

        // Should NOT detect non-destructive parameters
        assert!(!AtomicCompositionStrategy::is_destructive_action(
            "target_host"
        ));
        assert!(!AtomicCompositionStrategy::is_destructive_action("api_key"));
        assert!(!AtomicCompositionStrategy::is_destructive_action(
            "timeout_ms"
        ));
        assert!(!AtomicCompositionStrategy::is_destructive_action(
            "description"
        ));
        assert!(!AtomicCompositionStrategy::is_destructive_action(
            "username"
        ));
    }

    #[test]
    fn test_find_goal_for_parameter() {
        let outline = AtomicOutlinePlan {
            goals: vec![OutlineGoal {
                id: "G1".to_string(),
                title: "Test Goal".to_string(),
                description: "Test description".to_string(),
                rationale: None,
                expected_outcome: None,
                session_hint: None,
                prerequisites: vec!["param1".to_string()],
                children: vec![OutlineGoal {
                    id: "G1.1".to_string(),
                    title: "Child Goal".to_string(),
                    description: "Child description".to_string(),
                    rationale: None,
                    expected_outcome: None,
                    session_hint: None,
                    prerequisites: vec!["param2".to_string()],
                    children: vec![],
                }],
            }],
            overall_confidence: Some(0.9),
        };

        // Create test parameter with step_id
        let param_g1 = UnresolvedInput {
            id: "test_param".to_string(),
            parameter: "param1".to_string(),
            display_name: "Param 1".to_string(),
            step_id: Some("G1".to_string()),
            linked_steps: vec![],
            expected_type: None,
            json_schema: None,
            prompt: "test prompt".to_string(),
            required: true,
            notes: None,
            priority: QuestionPriority::PreExecution,
            ask_timing: AskTiming::PreExecution,
            discovery_timing: DiscoveryTiming::Auto,
            default_value: None,
            inference_hints: vec![],
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        };

        let param_g11 = UnresolvedInput {
            id: "test_param2".to_string(),
            parameter: "param2".to_string(),
            display_name: "Param 2".to_string(),
            step_id: Some("G1.1".to_string()),
            linked_steps: vec![],
            expected_type: None,
            json_schema: None,
            prompt: "test prompt 2".to_string(),
            required: true,
            notes: None,
            priority: QuestionPriority::PreExecution,
            ask_timing: AskTiming::PreExecution,
            discovery_timing: DiscoveryTiming::Auto,
            default_value: None,
            inference_hints: vec![],
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        };

        // Test finding top-level goal
        let found_g1 = AtomicCompositionStrategy::find_goal_for_parameter(&param_g1, &outline);
        assert!(found_g1.is_some());
        assert_eq!(found_g1.unwrap().id, "G1");

        // Test finding nested goal
        let found_g11 = AtomicCompositionStrategy::find_goal_for_parameter(&param_g11, &outline);
        assert!(found_g11.is_some());
        assert_eq!(found_g11.unwrap().id, "G1.1");

        // Test parameter with no step_id
        let param_no_step = UnresolvedInput {
            id: "test_param3".to_string(),
            parameter: "param3".to_string(),
            display_name: "Param 3".to_string(),
            step_id: None,
            linked_steps: vec![],
            expected_type: None,
            json_schema: None,
            prompt: "test prompt 3".to_string(),
            required: true,
            notes: None,
            priority: QuestionPriority::PreExecution,
            ask_timing: AskTiming::PreExecution,
            discovery_timing: DiscoveryTiming::Auto,
            default_value: None,
            inference_hints: vec![],
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        };

        let found_none =
            AtomicCompositionStrategy::find_goal_for_parameter(&param_no_step, &outline);
        assert!(found_none.is_none());
    }

    #[test]
    fn test_find_goal_in_children() {
        let children = vec![
            OutlineGoal {
                id: "C1".to_string(),
                title: "Child 1".to_string(),
                description: "Description 1".to_string(),
                rationale: None,
                expected_outcome: None,
                session_hint: None,
                prerequisites: vec![],
                children: vec![OutlineGoal {
                    id: "C1.1".to_string(),
                    title: "Nested Child".to_string(),
                    description: "Nested Description".to_string(),
                    rationale: None,
                    expected_outcome: None,
                    session_hint: None,
                    prerequisites: vec![],
                    children: vec![],
                }],
            },
            OutlineGoal {
                id: "C2".to_string(),
                title: "Child 2".to_string(),
                description: "Description 2".to_string(),
                rationale: None,
                expected_outcome: None,
                session_hint: None,
                prerequisites: vec![],
                children: vec![],
            },
        ];

        // Test finding direct child
        let found_c1 = AtomicCompositionStrategy::find_goal_in_children("C1", &children);
        assert!(found_c1.is_some());
        assert_eq!(found_c1.unwrap().id, "C1");

        // Test finding nested child
        let found_c11 = AtomicCompositionStrategy::find_goal_in_children("C1.1", &children);
        assert!(found_c11.is_some());
        assert_eq!(found_c11.unwrap().id, "C1.1");

        // Test finding second direct child
        let found_c2 = AtomicCompositionStrategy::find_goal_in_children("C2", &children);
        assert!(found_c2.is_some());
        assert_eq!(found_c2.unwrap().id, "C2");

        // Test not finding non-existent goal
        let found_none = AtomicCompositionStrategy::find_goal_in_children("C3", &children);
        assert!(found_none.is_none());
    }
}
