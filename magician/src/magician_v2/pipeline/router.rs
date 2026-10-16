//! # Pipeline Router
//!
//! Defines the [`RoutingDecision`] enum and the [`RouterAgent`] trait that
//! drive stage-to-stage routing within a pipeline. A [`RoutingRule`] function
//! pointer provides lightweight deterministic routing, while [`RouterLLM`]
//! offers an async LLM fallback path.
//!
//! The [`TieredRouter`] evaluates a prioritised list of deterministic code
//! rules (first `Some` wins) before optionally falling back to an LLM.

use async_trait::async_trait;
use std::sync::Arc;

use super::agent::{
    PipelineContext, AGENT_ID_INTENT_CLASSIFIER, AGENT_ID_PLAN_PATCHER, MAX_REFINEMENT_ROUNDS,
    MAX_REFINEMENT_USER_PAUSES,
};
use super::artifact::{ArtifactStore, ArtifactType};
use super::system_agents::intent_classifier::IntentClassificationResult;
use crate::magician_v2::strategy::plan::{PlanGraph, StepReadiness};

// ---------------------------------------------------------------------------
// RoutingDecision
// ---------------------------------------------------------------------------

/// The outcome of a routing evaluation — tells the orchestrator what to do
/// after the current pipeline stage completes.
#[derive(Debug, Clone)]
pub enum RoutingDecision {
    /// Advance to the next stage, handled by the given agent.
    NextStage { agent_id: String, reason: String },
    /// Pause execution and ask the user a clarifying question.
    Pause {
        question_text: String,
        slot_ids: Vec<String>,
    },
    /// The pipeline has completed successfully.
    Complete { reason: String },
    /// An unrecoverable error occurred during routing.
    Error { reason: String },
    /// Retry the same agent after a back-off delay (I-02).
    /// Only emitted for transient ServiceError failures.
    /// Does NOT consume the iteration budget.
    Retry {
        agent_id: String,
        reason: String,
        delay_ms: u64,
    },
}

// ---------------------------------------------------------------------------
// RouterAgent trait
// ---------------------------------------------------------------------------

/// Async trait for agents that decide the next routing step based on the
/// current artifact store and pipeline context.
///
/// `context` is taken as `&mut` so that the router can increment counters
/// such as `llm_routing_calls` when an LLM fallback is used.
#[async_trait]
pub trait RouterAgent: Send + Sync {
    /// Evaluate the current state and return a [`RoutingDecision`].
    async fn route(&self, store: &ArtifactStore, context: &mut PipelineContext) -> RoutingDecision;
}

// ---------------------------------------------------------------------------
// RoutingRule (function-pointer shorthand)
// ---------------------------------------------------------------------------

/// Lightweight deterministic routing rule. Returns `Some(decision)` when the
/// rule matches, or `None` to defer to the next rule / LLM fallback.
pub type RoutingRule = fn(&ArtifactStore, &PipelineContext) -> Option<RoutingDecision>;

// ---------------------------------------------------------------------------
// RouterLLM trait
// ---------------------------------------------------------------------------

/// Async trait for LLM-backed routing fallback. The orchestrator builds a
/// prompt from the current state and asks the LLM to pick the next stage.
#[async_trait]
pub trait RouterLLM: Send + Sync {
    /// Send a routing prompt to the LLM and return the raw response string.
    async fn route(&self, prompt: &str) -> Result<String, String>;
}

// ---------------------------------------------------------------------------
// TieredRouter
// ---------------------------------------------------------------------------

/// A deterministic-first router that evaluates an ordered list of code rules
/// before optionally falling back to an LLM. The first rule that returns
/// `Some(decision)` wins.
pub struct TieredRouter {
    rules: Vec<RoutingRule>,
    llm: Option<Arc<dyn RouterLLM>>,
    available_agents: Vec<String>,
}

impl TieredRouter {
    /// Create a new `TieredRouter` with the default 18 code rules.
    pub fn new(llm: Option<Arc<dyn RouterLLM>>, available_agents: Vec<String>) -> Self {
        Self {
            rules: default_rules(),
            llm,
            available_agents,
        }
    }

    /// Test-only constructor with custom rules, LLM, and agent list.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn new_with_rules(
        rules: Vec<RoutingRule>,
        llm: Option<Arc<dyn RouterLLM>>,
        available_agents: Vec<String>,
    ) -> Self {
        Self {
            rules,
            llm,
            available_agents,
        }
    }
}

#[async_trait]
impl RouterAgent for TieredRouter {
    async fn route(&self, store: &ArtifactStore, context: &mut PipelineContext) -> RoutingDecision {
        for rule in &self.rules {
            if let Some(decision) = rule(store, context) {
                return decision;
            }
        }
        // LLM fallback path
        let llm = match &self.llm {
            Some(llm) => llm,
            None => {
                return RoutingDecision::Error {
                    reason: "no routing rule matched and no LLM fallback configured".to_string(),
                }
            },
        };

        // P1-B fix: use >= 2 so the 3rd call is blocked (max 2 calls per run).
        if context.llm_routing_calls >= 2 {
            return RoutingDecision::Error {
                reason: "LLM routing budget exhausted (max 2 calls per run)".to_string(),
            };
        }

        // P1-A fix: increment the counter before making the LLM call so the
        // budget is accurately tracked even though the trait takes &mut context.
        context.llm_routing_calls += 1;

        let prompt = build_routing_prompt(store, context, &self.available_agents);
        let response = match llm.route(&prompt).await {
            Ok(resp) => resp,
            Err(err) => {
                return RoutingDecision::Error {
                    reason: format!("LLM routing failed: {err}"),
                }
            },
        };

        let decision = match parse_routing_response(&response) {
            Ok(d) => d,
            Err(err) => {
                return RoutingDecision::Error {
                    reason: format!("LLM returned unparseable routing decision: {err}"),
                }
            },
        };

        // Validate agent_id for NextStage decisions
        if let RoutingDecision::NextStage { ref agent_id, .. } = decision {
            if !self.available_agents.contains(agent_id) {
                return RoutingDecision::Error {
                    reason: format!("LLM suggested unknown agent: {agent_id}"),
                };
            }
        }

        decision
    }
}

// ---------------------------------------------------------------------------
// LLM routing helpers
// ---------------------------------------------------------------------------

/// Build a routing prompt for the LLM fallback, summarising the current
/// pipeline state, artifact store contents, and available agents.
fn build_routing_prompt(
    store: &ArtifactStore,
    context: &PipelineContext,
    agents: &[String],
) -> String {
    let mut prompt = String::new();

    // --- Pipeline context summary ---
    prompt.push_str("## Pipeline Context\n");
    prompt.push_str(&format!("- Query: {}\n", context.query));
    prompt.push_str(&format!("- Iteration: {}\n", context.iteration));
    prompt.push_str(&format!(
        "- Resume mode: {}\n",
        context.resume_mode.as_deref().unwrap_or("none")
    ));
    if let Some(started_at) = context.run_started_at {
        prompt.push_str(&format!("- Run started at: {}\n", started_at.to_rfc3339()));
    }
    prompt.push('\n');

    // --- Artifact store summary ---
    prompt.push_str("## Artifact Store\n");
    if store.is_empty() {
        prompt.push_str("(empty)\n");
    } else {
        // Collect artifacts grouped by type
        let mut by_type: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        for (_, artifact) in store.iter() {
            let type_name = artifact.artifact_type.to_string();
            by_type.entry(type_name).or_default().push((
                artifact.producer_agent_id.clone(),
                artifact.produced_at.to_rfc3339(),
            ));
        }
        for (type_name, entries) in &by_type {
            for (producer, timestamp) in entries {
                prompt.push_str(&format!(
                    "- {type_name}: producer={producer}, produced_at={timestamp}\n"
                ));
            }
        }
    }
    prompt.push('\n');

    // --- Available agents ---
    prompt.push_str("## Available Agents\n");
    for agent in agents {
        prompt.push_str(&format!("- {agent}\n"));
    }
    prompt.push('\n');

    // --- Instructions ---
    prompt.push_str("## Instructions\n");
    prompt.push_str(
        "Based on the pipeline context and artifact store above, decide the next routing step.\n\
         Return your decision as a JSON object with these fields:\n\
         - \"decision\": one of \"next_stage\", \"pause\", \"complete\", or \"error\"\n\
         - \"agent_id\": (required for next_stage) the agent ID to route to\n\
         - \"reason\": (required for next_stage, complete, error) explanation of the decision\n\
         - \"question_text\": (required for pause) the clarification question to ask\n\
         - \"slot_ids\": (required for pause) array of slot IDs that need clarification\n\n\
         Example: {\"decision\": \"next_stage\", \"agent_id\": \"system:planner\", \"reason\": \"ready to plan\"}\n",
    );

    prompt
}

/// Parse a JSON response from the LLM into a [`RoutingDecision`].
fn parse_routing_response(json: &str) -> Result<RoutingDecision, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;

    let decision = value
        .get("decision")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing or non-string \"decision\" field".to_string())?;

    match decision {
        "next_stage" => {
            let agent_id = value
                .get("agent_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "next_stage requires \"agent_id\" field".to_string())?
                .to_string();
            let reason = value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("LLM routing decision")
                .to_string();
            Ok(RoutingDecision::NextStage { agent_id, reason })
        },
        "pause" => {
            let question_text = value
                .get("question_text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "pause requires \"question_text\" field".to_string())?
                .to_string();
            let slot_ids = value
                .get("slot_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Ok(RoutingDecision::Pause {
                question_text,
                slot_ids,
            })
        },
        "complete" => {
            let reason = value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("LLM routing decision")
                .to_string();
            Ok(RoutingDecision::Complete { reason })
        },
        "error" => {
            let reason = value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("LLM routing decision")
                .to_string();
            Ok(RoutingDecision::Error { reason })
        },
        other => Err(format!("unknown decision type: \"{other}\"")),
    }
}

// ---------------------------------------------------------------------------
// Artifact content helpers
// ---------------------------------------------------------------------------

/// Extract a boolean field from an artifact's JSON content.
fn artifact_bool(artifact: &super::artifact::AgentArtifact, key: &str) -> bool {
    artifact
        .content
        .get(key)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Extract a string field from an artifact's JSON content.
fn artifact_str(artifact: &super::artifact::AgentArtifact, key: &str) -> Option<String> {
    artifact
        .content
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
}

// ---------------------------------------------------------------------------
// Default rule set (18 rules, priority order)
// ---------------------------------------------------------------------------

/// Returns the default ordered set of 18 deterministic routing rules.
fn default_rules() -> Vec<RoutingRule> {
    vec![
        rule_resume_full_replan,
        rule_resume_partial_replan,
        rule_user_answer_needs_interpretation,
        rule_interpreted_answer_needs_replan,
        rule_interpreted_answer_light_update,
        rule_permanent_agent_error,
        rule_service_error_retry, // rule 3.6
        rule_cold_start,          // Rule 4 — route to intent classifier
        rule_non_task_complete,   // Rule 4b — non-task intent → complete
        rule_needs_slot_extraction,
        rule_needs_elicitation,
        rule_needs_clarification,
        rule_slots_resolved,
        rule_ready_to_plan,
        // Phase C: during refinement, route to patcher when chain completes.
        rule_refinement_chain_complete,
        // Phase C: start refinement for Weak steps when no Ready/JIT remain.
        rule_needs_refinement,
        rule_planning_complete,
    ]
}

// ---------------------------------------------------------------------------
// Resume rules (0, 0b, 1, 2, 3)
// ---------------------------------------------------------------------------

/// Rule 0 — full_replan: restart from slot extraction when no SlotGraph has
/// been produced since `run_started_at`.
///
/// **Known limitation (I-11):** `full_replan` routes to `system:slot-extractor`,
/// reusing the `QueryAnalysis` artifact that IntentAwareProcessor produced
/// before suspension.  If the user's clarifying answer changes the intent
/// class or entity list, that change does not propagate to slot extraction.
/// This is an intentional trade-off (re-running query analysis costs an LLM call).
fn rule_resume_full_replan(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    let run_started_at = context.run_started_at?;
    if context.resume_mode.as_deref() != Some("full_replan") {
        return None;
    }
    if store
        .latest_of_type_since(&ArtifactType::SlotGraph, run_started_at)
        .is_some()
    {
        return None; // already produced this run
    }
    Some(RoutingDecision::NextStage {
        agent_id: "system:slot-extractor".to_string(),
        reason: "full_replan: restart from slot extraction".to_string(),
    })
}

/// Rule 0b — partial_replan: restart from query rewrite when no ClarifiedTask
/// has been produced since `run_started_at`.
fn rule_resume_partial_replan(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    let run_started_at = context.run_started_at?;
    if context.resume_mode.as_deref() != Some("partial_replan") {
        return None;
    }
    if store
        .latest_of_type_since(&ArtifactType::ClarifiedTask, run_started_at)
        .is_some()
    {
        return None;
    }
    Some(RoutingDecision::NextStage {
        agent_id: "system:query-rewriter".to_string(),
        reason: "partial_replan: restart from query rewrite".to_string(),
    })
}

/// Rule 1 — user answer needs interpretation: the user answered a
/// clarification question and no InterpretedAnswer exists yet for this run.
fn rule_user_answer_needs_interpretation(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    // Only fire when user_answer is present
    context.user_answer.as_ref()?;

    // Rules 0/0b handle full_replan and partial_replan
    match context.resume_mode.as_deref() {
        Some("full_replan") | Some("partial_replan") => return None,
        _ => {},
    }

    let run_started_at = context.run_started_at?;
    if store
        .latest_of_type_since(&ArtifactType::InterpretedAnswer, run_started_at)
        .is_some()
    {
        return None;
    }
    Some(RoutingDecision::NextStage {
        agent_id: "system:answer-interpreter".to_string(),
        reason: "user answer needs interpretation".to_string(),
    })
}

/// Rule 2 — interpreted answer requires replan: the latest InterpretedAnswer
/// has `requires_replan == true` and no ClarifiedTask exists after it.
fn rule_interpreted_answer_needs_replan(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    let run_started_at = context.run_started_at?;
    let interp = store.latest_of_type_since(&ArtifactType::InterpretedAnswer, run_started_at)?;
    if !artifact_bool(interp, "requires_replan") {
        return None;
    }
    if store
        .latest_of_type_after(&ArtifactType::ClarifiedTask, interp.produced_at)
        .is_some()
    {
        return None;
    }
    Some(RoutingDecision::NextStage {
        agent_id: "system:query-rewriter".to_string(),
        reason: "interpreted answer requires replanning".to_string(),
    })
}

/// Rule 3 — light slot update: the latest InterpretedAnswer has
/// `requires_replan == false` and no ElicitationResult exists after it.
fn rule_interpreted_answer_light_update(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    let run_started_at = context.run_started_at?;
    let interp = store.latest_of_type_since(&ArtifactType::InterpretedAnswer, run_started_at)?;
    if artifact_bool(interp, "requires_replan") {
        return None;
    }
    if store
        .latest_of_type_after(&ArtifactType::ElicitationResult, interp.produced_at)
        .is_some()
    {
        return None;
    }
    Some(RoutingDecision::NextStage {
        agent_id: "system:elicitor".to_string(),
        reason: "light slot update: re-evaluate elicitation".to_string(),
    })
}

// ---------------------------------------------------------------------------
// Error short-circuit (rule 3.5)
// ---------------------------------------------------------------------------

/// Rule 3.5 — permanent agent error: if ANY AgentError artifact has
/// `recoverable == false`, immediately return an error.
///
/// Note: `recoverable` defaults to `true` when the field is absent so that
/// artifacts with no explicit value are treated as transient, not fatal.
/// `failed_agent_id` is read from the artifact `content` rather than
/// `producer_agent_id` because the orchestrator always sets
/// `producer_agent_id` to `"system:orchestrator"`.
fn rule_permanent_agent_error(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    let errors = store.all_of_type(&ArtifactType::AgentError);
    for err_artifact in errors {
        // P2-4: safe default — treat missing "recoverable" as recoverable (true).
        let is_recoverable = err_artifact
            .content
            .get("recoverable")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !is_recoverable {
            // P2-1: read the actual failing agent from content, not producer_agent_id.
            let agent_id = artifact_str(err_artifact, "failed_agent_id")
                .unwrap_or_else(|| "unknown-agent".to_string());
            let error_text =
                artifact_str(err_artifact, "error").unwrap_or_else(|| "unknown error".to_string());
            return Some(RoutingDecision::Error {
                reason: format!("permanent failure: {}: {}", agent_id, error_text),
            });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Forward rules (4–10)
// ---------------------------------------------------------------------------

/// Rule 3.6 — service-error retry: the latest AgentError artifact is a
/// transient ServiceError (retry_count < 2). Emit Retry{delay_ms: 1000}
/// to back off before re-executing the same agent.
fn rule_service_error_retry(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    const MAX_SERVICE_RETRIES: u64 = 2;

    let errors = store.all_of_type(&ArtifactType::AgentError);
    for err_artifact in errors.iter().rev() {
        // Match on the stable `error_kind` field written by the orchestrator,
        // falling back to the legacy string-prefix check for pre-existing
        // artifacts that lack the field.
        let is_service_error = err_artifact
            .content
            .get("error_kind")
            .and_then(|v| v.as_str())
            .map(|kind| kind == "service_error")
            .unwrap_or_else(|| {
                // Legacy fallback: match on display-string prefix.
                err_artifact
                    .content
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(|t| t.starts_with("underlying service error:"))
                    .unwrap_or(false)
            });
        if !is_service_error {
            continue;
        }
        let retry_count = err_artifact
            .content
            .get("retry_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if retry_count >= MAX_SERVICE_RETRIES {
            return None;
        }
        let agent_id = err_artifact
            .content
            .get("failed_agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown-agent")
            .to_string();
        let error_text = err_artifact
            .content
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Some(RoutingDecision::Retry {
            agent_id,
            reason: format!(
                "transient service error (retry {}): {}",
                retry_count + 1,
                error_text
            ),
            delay_ms: 1000,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Autonomous cycle short-circuit (rule 3.7)
// ---------------------------------------------------------------------------

// Rule 3.7 (rule_autonomous_agent) — REMOVED.
// The one-shot autonomous planner has been replaced by direct agentic execution.
// Autonomous cycles now bypass the pipeline entirely via execute_agentic_direct_with_outcome().

/// Rule 4 — cold start: no QueryAnalysis produced *this run* — route to intent
/// classifier.  Uses `run_started_at` freshness guard so the classifier fires on
/// every fresh pipeline invocation, not just when the store is empty (the store
/// may carry persisted artifacts from prior turns).
fn rule_cold_start(store: &ArtifactStore, context: &PipelineContext) -> Option<RoutingDecision> {
    // On resume-from-suspension the QA from the prior run is still valid —
    // re-classifying would wipe the store via clear_all on NewTask.
    if context.resume_mode.is_some() {
        return None;
    }
    let run_started_at = context.run_started_at?;
    if store
        .latest_of_type_since(&ArtifactType::QueryAnalysis, run_started_at)
        .is_none()
    {
        return Some(RoutingDecision::NextStage {
            agent_id: AGENT_ID_INTENT_CLASSIFIER.to_string(),
            reason: "cold start: no QueryAnalysis this run — route to intent classifier"
                .to_string(),
        });
    }
    None
}

/// Rule 4b — non-task intent detected *this run*: pipeline should complete
/// immediately.  Uses `run_started_at` freshness guard so a stale
/// IntentClassification from a prior turn (e.g. StatusResponse) does not
/// prematurely terminate a new-task pipeline run.
fn rule_non_task_complete(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    // On resume-from-suspension a stale non-task classification must not
    // terminate the resumed pipeline.
    if context.resume_mode.is_some() {
        return None;
    }
    let run_started_at = context.run_started_at?;
    let artifact =
        store.latest_of_type_since(&ArtifactType::IntentClassification, run_started_at)?;
    let result: IntentClassificationResult = artifact.deserialize_content().ok()?;
    if !result.requires_pipeline {
        return Some(RoutingDecision::Complete {
            reason: format!("non-task intent: {:?}", result.intent),
        });
    }
    None
}

/// Rule 5 — needs slot extraction: QueryAnalysis exists but no SlotGraph.
fn rule_needs_slot_extraction(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    if store.latest_of_type(&ArtifactType::QueryAnalysis).is_some()
        && store.latest_of_type(&ArtifactType::SlotGraph).is_none()
    {
        return Some(RoutingDecision::NextStage {
            agent_id: "system:slot-extractor".to_string(),
            reason: "query analyzed, need slot extraction".to_string(),
        });
    }
    None
}

/// Rule 6 — needs elicitation: SlotGraph exists but no ElicitationResult.
fn rule_needs_elicitation(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    if store.latest_of_type(&ArtifactType::SlotGraph).is_some()
        && store
            .latest_of_type(&ArtifactType::ElicitationResult)
            .is_none()
    {
        return Some(RoutingDecision::NextStage {
            agent_id: "system:elicitor".to_string(),
            reason: "slots extracted, need elicitation".to_string(),
        });
    }
    None
}

/// Rule 7 — needs clarification: latest ElicitationResult has
/// `needs_clarification: true`, and no InterpretedAnswer or ClarifiedTask
/// exists after it.
fn rule_needs_clarification(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    let elicitation = store.latest_of_type(&ArtifactType::ElicitationResult)?;
    if !artifact_bool(elicitation, "needs_clarification") {
        return None;
    }
    // Skip if an InterpretedAnswer was produced after the elicitation
    if store
        .latest_of_type_after(&ArtifactType::InterpretedAnswer, elicitation.produced_at)
        .is_some()
    {
        return None;
    }
    // Skip if a ClarifiedTask was produced after the elicitation
    if store
        .latest_of_type_after(&ArtifactType::ClarifiedTask, elicitation.produced_at)
        .is_some()
    {
        return None;
    }

    let question_text = artifact_str(elicitation, "question_text")
        .unwrap_or_else(|| "Please provide more information".to_string());
    let slot_ids: Vec<String> = elicitation
        .content
        .get("slot_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if slot_ids.is_empty() {
        return Some(RoutingDecision::Error {
            reason: "ElicitationResult has needs_clarification=true but slot_ids is absent or empty — elicitor produced invalid output".to_string(),
        });
    }

    Some(RoutingDecision::Pause {
        question_text,
        slot_ids,
    })
}

/// Rule 8 — slots resolved: latest ElicitationResult has
/// `needs_clarification: false` and no ClarifiedTask exists after it.
fn rule_slots_resolved(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    let elicitation = store.latest_of_type(&ArtifactType::ElicitationResult)?;
    if artifact_bool(elicitation, "needs_clarification") {
        return None;
    }
    if store
        .latest_of_type_after(&ArtifactType::ClarifiedTask, elicitation.produced_at)
        .is_some()
    {
        return None;
    }
    Some(RoutingDecision::NextStage {
        agent_id: "system:query-rewriter".to_string(),
        reason: "slots resolved, need query rewrite".to_string(),
    })
}

/// Rule 9 — ready to plan: ClarifiedTask (or resolved ElicitationResult) exists
/// but no PlanGraph.
///
/// The primary trigger is a `ClarifiedTask` artifact (produced by query-rewriter).
/// As a fallback, an `ElicitationResult` with `needs_clarification == false` also
/// fires this rule — this makes the pipeline resilient to scenarios where the
/// query-rewriter stage is skipped or absent from the agent registry.
fn rule_ready_to_plan(
    store: &ArtifactStore,
    _context: &PipelineContext,
) -> Option<RoutingDecision> {
    if store.latest_of_type(&ArtifactType::PlanGraph).is_some() {
        return None;
    }

    if store.latest_of_type(&ArtifactType::ClarifiedTask).is_some() {
        return Some(RoutingDecision::NextStage {
            agent_id: "system:planner".to_string(),
            reason: "task clarified, ready to plan".to_string(),
        });
    }

    // Fallback: ElicitationResult with needs_clarification == false means slots
    // are resolved but query-rewriter was skipped or unavailable.
    if let Some(elicitation) = store.latest_of_type(&ArtifactType::ElicitationResult) {
        if !artifact_bool(elicitation, "needs_clarification") {
            return Some(RoutingDecision::NextStage {
                agent_id: "system:planner".to_string(),
                reason: "elicitation resolved (no ClarifiedTask), ready to plan".to_string(),
            });
        }
    }

    None
}

/// Rule 8b — refinement chain complete: in refinement mode and the
/// slot-extractor → elicitor → query-rewriter chain has produced usable
/// results (ClarifiedTask or SlotGraph). Route to plan-patcher.
fn rule_refinement_chain_complete(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    // Only during refinement
    context.refinement_step_id.as_ref()?;

    // Check if refinement produced usable results:
    // Either ClarifiedTask exists, or SlotGraph exists with resolved data
    let has_clarified = store.latest_of_type(&ArtifactType::ClarifiedTask).is_some();
    let has_slots = store.latest_of_type(&ArtifactType::SlotGraph).is_some();

    if has_clarified || has_slots {
        Some(RoutingDecision::NextStage {
            agent_id: AGENT_ID_PLAN_PATCHER.to_string(),
            reason: "refinement chain complete, patching PlanGraph".to_string(),
        })
    } else {
        None
    }
}

/// Rule 10b — refinement needed: PlanGraph exists, Weak steps remain,
/// but no refinement is currently in progress and budget allows it.
fn rule_needs_refinement(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    // Only when a PlanGraph exists
    let plan_artifact = store.latest_of_type(&ArtifactType::PlanGraph)?;
    let plan_graph: PlanGraph = plan_artifact.deserialize_content().ok()?;

    // Already in refinement? Let existing rules handle it.
    if context.refinement_step_id.is_some() {
        return None;
    }

    // Budget check: max 2 rounds, max 2 user pauses
    if context.refinement_rounds >= MAX_REFINEMENT_ROUNDS
        || context.refinement_user_pauses >= MAX_REFINEMENT_USER_PAUSES
    {
        return None; // Fall through to rule_planning_complete which will execute Weak as-is
    }

    // Find the first remaining Weak step by order. Planning no longer writes
    // per-step execution artifacts, so presence in the graph is enough.
    let _weak_step = plan_graph
        .steps
        .iter()
        .find(|step| step.readiness == Some(StepReadiness::Weak))?;

    Some(RoutingDecision::NextStage {
        agent_id: "system:slot-extractor".to_string(),
        reason: format!("refining weak step: {}", _weak_step.id),
    })
}

/// Rule 10 — planning complete: a PlanGraph exists and no further refinement
/// round should run inside the planning pipeline.
fn rule_planning_complete(
    store: &ArtifactStore,
    context: &PipelineContext,
) -> Option<RoutingDecision> {
    if context.refinement_step_id.is_some() {
        return None;
    }

    let plan_artifact = store.latest_of_type(&ArtifactType::PlanGraph)?;
    let plan_graph: PlanGraph = plan_artifact.deserialize_content().ok()?;

    Some(RoutingDecision::Complete {
        reason: format!(
            "PlanGraph ready for execution ({} planned steps)",
            plan_graph.steps.len()
        ),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use serde_json::json;
    use std::sync::Arc;

    use super::super::artifact::{
        AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION,
    };

    // -- test helpers -------------------------------------------------------

    fn make_artifact_at(
        id: &str,
        artifact_type: ArtifactType,
        produced_at: DateTime<Utc>,
        content: serde_json::Value,
    ) -> AgentArtifact {
        AgentArtifact {
            artifact_id: id.to_string(),
            artifact_type,
            producer_agent_id: "test-agent".to_string(),
            producer_cycle_id: "cycle-1".to_string(),
            content,
            schema_version: 1,
            produced_at,
            render_hints: None,
        }
    }

    fn base_context() -> PipelineContext {
        PipelineContext {
            chain_id: "test-chain".to_string(),
            cycle_id: "cycle-1".to_string(),
            workflow_id: "wf-1".to_string(),
            query: "test query".to_string(),
            iteration: 0,
            agent_id: None,
            correlation_id: None,
            user_answer: None,
            run_started_at: Some(Utc::now()),
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

    // -- routing_decision_debug_format ------------------------------------

    #[test]
    fn routing_decision_debug_format() {
        let next_stage = RoutingDecision::NextStage {
            agent_id: "agent:planner".to_string(),
            reason: "slots filled".to_string(),
        };
        let pause = RoutingDecision::Pause {
            question_text: "Which city?".to_string(),
            slot_ids: vec!["city".to_string()],
        };
        let complete = RoutingDecision::Complete {
            reason: "all done".to_string(),
        };
        let error = RoutingDecision::Error {
            reason: "timeout".to_string(),
        };

        // Each variant must produce non-empty Debug output.
        assert!(!format!("{:?}", next_stage).is_empty());
        assert!(!format!("{:?}", pause).is_empty());
        assert!(!format!("{:?}", complete).is_empty());
        assert!(!format!("{:?}", error).is_empty());
    }

    // -- mock_router_compiles ---------------------------------------------

    /// A trivial mock that always returns `Complete`.
    struct MockRouter;

    #[async_trait]
    impl RouterAgent for MockRouter {
        async fn route(
            &self,
            _store: &ArtifactStore,
            _context: &mut PipelineContext,
        ) -> RoutingDecision {
            RoutingDecision::Complete {
                reason: "mock".to_string(),
            }
        }
    }

    #[tokio::test]
    async fn mock_router_compiles() {
        let router: Arc<dyn RouterAgent> = Arc::new(MockRouter);

        let store = ArtifactStore::new("test-chain".to_string());
        let mut context = PipelineContext {
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
        };

        let decision = router.route(&store, &mut context).await;
        match decision {
            RoutingDecision::Complete { reason } => assert_eq!(reason, "mock"),
            other => panic!("expected Complete, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // P5.5-B-04 — Resume rule tests (1–12)
    // -----------------------------------------------------------------------

    // 1. rule_0_full_replan_routes_to_slot_extractor
    #[test]
    fn rule_0_full_replan_routes_to_slot_extractor() {
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();
        ctx.resume_mode = Some("full_replan".to_string());

        let decision = rule_resume_full_replan(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, reason }) => {
                assert_eq!(agent_id, "system:slot-extractor");
                assert!(reason.contains("full_replan"));
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 2. rule_0_full_replan_skips_when_slot_graph_fresh
    #[test]
    fn rule_0_full_replan_skips_when_slot_graph_fresh() {
        let mut ctx = base_context();
        ctx.resume_mode = Some("full_replan".to_string());
        let run_started = ctx.run_started_at.unwrap();

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "sg-1",
            ArtifactType::SlotGraph,
            run_started + Duration::milliseconds(10),
            json!({}),
        ));

        assert!(rule_resume_full_replan(&store, &ctx).is_none());
    }

    // 3. rule_0b_partial_replan_routes_to_rewriter
    #[test]
    fn rule_0b_partial_replan_routes_to_rewriter() {
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();
        ctx.resume_mode = Some("partial_replan".to_string());

        let decision = rule_resume_partial_replan(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, reason }) => {
                assert_eq!(agent_id, "system:query-rewriter");
                assert!(reason.contains("partial_replan"));
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 4. rule_0b_partial_replan_skips_when_clarified_fresh
    #[test]
    fn rule_0b_partial_replan_skips_when_clarified_fresh() {
        let mut ctx = base_context();
        ctx.resume_mode = Some("partial_replan".to_string());
        let run_started = ctx.run_started_at.unwrap();

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "ct-1",
            ArtifactType::ClarifiedTask,
            run_started + Duration::milliseconds(10),
            json!({}),
        ));

        assert!(rule_resume_partial_replan(&store, &ctx).is_none());
    }

    // 5. rule_1_user_answer_routes_to_interpreter
    #[test]
    fn rule_1_user_answer_routes_to_interpreter() {
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();
        ctx.user_answer = Some("yes".to_string());

        let decision = rule_user_answer_needs_interpretation(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, reason }) => {
                assert_eq!(agent_id, "system:answer-interpreter");
                assert!(reason.contains("user answer"));
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 6. rule_1_user_answer_with_fresh_interpretation_skips
    #[test]
    fn rule_1_user_answer_with_fresh_interpretation_skips() {
        let mut ctx = base_context();
        ctx.user_answer = Some("yes".to_string());
        let run_started = ctx.run_started_at.unwrap();

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "ia-1",
            ArtifactType::InterpretedAnswer,
            run_started + Duration::milliseconds(10),
            json!({"requires_replan": false}),
        ));

        assert!(rule_user_answer_needs_interpretation(&store, &ctx).is_none());
    }

    // 7. rule_1_skips_on_full_replan
    #[test]
    fn rule_1_skips_on_full_replan() {
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();
        ctx.user_answer = Some("yes".to_string());
        ctx.resume_mode = Some("full_replan".to_string());

        assert!(rule_user_answer_needs_interpretation(&store, &ctx).is_none());
    }

    // 8. rule_1_skips_on_partial_replan
    #[test]
    fn rule_1_skips_on_partial_replan() {
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();
        ctx.user_answer = Some("yes".to_string());
        ctx.resume_mode = Some("partial_replan".to_string());

        assert!(rule_user_answer_needs_interpretation(&store, &ctx).is_none());
    }

    // 9. rule_2_replan_routes_to_rewriter
    #[test]
    fn rule_2_replan_routes_to_rewriter() {
        let mut ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        ctx.user_answer = Some("yes".to_string());

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "ia-1",
            ArtifactType::InterpretedAnswer,
            run_started + Duration::milliseconds(10),
            json!({"requires_replan": true}),
        ));

        let decision = rule_interpreted_answer_needs_replan(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, reason }) => {
                assert_eq!(agent_id, "system:query-rewriter");
                assert!(reason.contains("replanning"));
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 10. rule_2_replan_skips_when_clarified_task_exists
    #[test]
    fn rule_2_replan_skips_when_clarified_task_exists() {
        let mut ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        ctx.user_answer = Some("yes".to_string());

        let ia_time = run_started + Duration::milliseconds(10);
        let ct_time = ia_time + Duration::milliseconds(10);

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "ia-1",
            ArtifactType::InterpretedAnswer,
            ia_time,
            json!({"requires_replan": true}),
        ));
        store.put(make_artifact_at(
            "ct-1",
            ArtifactType::ClarifiedTask,
            ct_time,
            json!({}),
        ));

        assert!(rule_interpreted_answer_needs_replan(&store, &ctx).is_none());
    }

    // 11. rule_3_light_update_routes_to_elicitor
    #[test]
    fn rule_3_light_update_routes_to_elicitor() {
        let mut ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        ctx.user_answer = Some("yes".to_string());

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "ia-1",
            ArtifactType::InterpretedAnswer,
            run_started + Duration::milliseconds(10),
            json!({"requires_replan": false}),
        ));

        let decision = rule_interpreted_answer_light_update(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, reason }) => {
                assert_eq!(agent_id, "system:elicitor");
                assert!(reason.contains("light slot update"));
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 12. rule_3_light_update_skips_when_elicitation_exists
    #[test]
    fn rule_3_light_update_skips_when_elicitation_exists() {
        let mut ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        ctx.user_answer = Some("yes".to_string());

        let ia_time = run_started + Duration::milliseconds(10);
        let er_time = ia_time + Duration::milliseconds(10);

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "ia-1",
            ArtifactType::InterpretedAnswer,
            ia_time,
            json!({"requires_replan": false}),
        ));
        store.put(make_artifact_at(
            "er-1",
            ArtifactType::ElicitationResult,
            er_time,
            json!({"needs_clarification": false}),
        ));

        assert!(rule_interpreted_answer_light_update(&store, &ctx).is_none());
    }

    // -----------------------------------------------------------------------
    // P5.5-B-04 — Error rule tests (13–15)
    // -----------------------------------------------------------------------

    // 13. rule_3_5_permanent_error_returns_error
    #[test]
    fn rule_3_5_permanent_error_returns_error() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "err-1",
            ArtifactType::AgentError,
            Utc::now(),
            json!({"recoverable": false, "error": "disk full"}),
        ));

        let decision = rule_permanent_agent_error(&store, &ctx);
        match decision {
            Some(RoutingDecision::Error { reason }) => {
                assert!(reason.contains("permanent failure"));
                assert!(reason.contains("disk full"));
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // 14. rule_3_5_recoverable_error_passes_through
    #[test]
    fn rule_3_5_recoverable_error_passes_through() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "err-1",
            ArtifactType::AgentError,
            Utc::now(),
            json!({"recoverable": true, "error": "transient timeout"}),
        ));

        assert!(rule_permanent_agent_error(&store, &ctx).is_none());
    }

    // 15. rule_3_5_catches_non_latest_error
    #[test]
    fn rule_3_5_catches_non_latest_error() {
        let ctx = base_context();
        let t1 = Utc::now();
        let t2 = t1 + Duration::milliseconds(100);

        let mut store = ArtifactStore::new("test-chain");
        // First error: non-recoverable
        store.put(make_artifact_at(
            "err-1",
            ArtifactType::AgentError,
            t1,
            json!({"recoverable": false, "error": "fatal"}),
        ));
        // Second error: recoverable
        store.put(make_artifact_at(
            "err-2",
            ArtifactType::AgentError,
            t2,
            json!({"recoverable": true, "error": "transient"}),
        ));

        let decision = rule_permanent_agent_error(&store, &ctx);
        match decision {
            Some(RoutingDecision::Error { reason }) => {
                assert!(reason.contains("permanent failure"));
                assert!(reason.contains("fatal"));
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // P5.5-B-04 — Forward rule tests (16–24)
    // -----------------------------------------------------------------------

    // 16. rule_4_cold_start_routes_to_intent_classifier
    #[test]
    fn rule_4_cold_start_routes_to_intent_classifier() {
        let store = ArtifactStore::new("test-chain");
        let ctx = base_context();

        let decision = rule_cold_start(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:intent-classifier");
            },
            other => panic!(
                "expected NextStage(system:intent-classifier), got {:?}",
                other
            ),
        }
    }

    // 16a. rule_4_cold_start_skips_when_fresh_qa_exists
    #[test]
    fn rule_4_cold_start_skips_when_fresh_qa_exists() {
        let ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");
        // Seed a QueryAnalysis produced AFTER run_started_at.
        store.put(make_artifact_at(
            "qa-fresh",
            ArtifactType::QueryAnalysis,
            run_started + chrono::Duration::milliseconds(10),
            json!({}),
        ));

        assert!(
            rule_cold_start(&store, &ctx).is_none(),
            "should not fire when fresh QueryAnalysis exists"
        );
    }

    // 16a2. rule_4_cold_start_fires_when_only_stale_qa_exists
    #[test]
    fn rule_4_cold_start_fires_when_only_stale_qa_exists() {
        let ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");
        // Seed a QueryAnalysis produced BEFORE run_started_at (stale from prior turn).
        store.put(make_artifact_at(
            "qa-stale",
            ArtifactType::QueryAnalysis,
            run_started - chrono::Duration::seconds(5),
            json!({}),
        ));

        match rule_cold_start(&store, &ctx) {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:intent-classifier");
            },
            other => panic!(
                "expected NextStage(system:intent-classifier) for stale QA, got {:?}",
                other
            ),
        }
    }

    // 16b. rule_4b_non_task_complete
    #[test]
    fn rule_4b_non_task_complete() {
        use crate::magician_v2::pipeline::system_agents::intent_classifier::IntentClassificationResult;
        use crate::magician_v2::query_analysis::intent::QueryIntent;

        let ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");

        // Seed fresh IntentClassification (produced after run_started_at).
        let ic = IntentClassificationResult {
            intent: QueryIntent::StatusQuery,
            requires_pipeline: false,
            user_answer: None,
            is_new_task: false,
            classification_duration_ms: 0,
        };
        store.put(make_artifact_at(
            "ic-1",
            ArtifactType::IntentClassification,
            run_started + chrono::Duration::milliseconds(10),
            serde_json::to_value(&ic).unwrap(),
        ));

        let decision = rule_non_task_complete(&store, &ctx);
        match decision {
            Some(RoutingDecision::Complete { reason }) => {
                assert!(reason.contains("non-task intent"), "reason: {reason}");
            },
            other => panic!("expected Complete, got {:?}", other),
        }
    }

    // 16b2. rule_4b_ignores_stale_non_task_classification
    #[test]
    fn rule_4b_ignores_stale_non_task_classification() {
        use crate::magician_v2::pipeline::system_agents::intent_classifier::IntentClassificationResult;
        use crate::magician_v2::query_analysis::intent::QueryIntent;

        let ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");

        // Seed STALE IntentClassification from prior turn (before run_started_at).
        let ic = IntentClassificationResult {
            intent: QueryIntent::StatusQuery,
            requires_pipeline: false,
            user_answer: None,
            is_new_task: false,
            classification_duration_ms: 0,
        };
        store.put(make_artifact_at(
            "ic-stale",
            ArtifactType::IntentClassification,
            run_started - chrono::Duration::seconds(5),
            serde_json::to_value(&ic).unwrap(),
        ));

        assert!(
            rule_non_task_complete(&store, &ctx).is_none(),
            "should ignore stale non-task IntentClassification from prior turn"
        );
    }

    // 16c. rule_4b_passes_through_for_task_intents
    #[test]
    fn rule_4b_passes_through_for_task_intents() {
        use crate::magician_v2::pipeline::system_agents::intent_classifier::IntentClassificationResult;
        use crate::magician_v2::query_analysis::intent::QueryIntent;

        let ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");

        let ic = IntentClassificationResult {
            intent: QueryIntent::NewTask,
            requires_pipeline: true,
            user_answer: None,
            is_new_task: true,
            classification_duration_ms: 0,
        };
        store.put(make_artifact_at(
            "ic-1",
            ArtifactType::IntentClassification,
            run_started + chrono::Duration::milliseconds(10),
            serde_json::to_value(&ic).unwrap(),
        ));

        assert!(
            rule_non_task_complete(&store, &ctx).is_none(),
            "should pass through for task intents"
        );
    }

    // 16d. rule_4_cold_start_skips_on_resume
    #[test]
    fn rule_4_cold_start_skips_on_resume() {
        let mut ctx = base_context();
        ctx.resume_mode = Some("light_slot_update".to_string());
        let store = ArtifactStore::new("test-chain");
        // Empty store with no QA — normally rule_cold_start would fire.
        // But on resume it must be suppressed.
        assert!(
            rule_cold_start(&store, &ctx).is_none(),
            "rule_cold_start must not fire on resume-from-suspension"
        );
    }

    // 16e. rule_4b_non_task_complete_skips_on_resume
    #[test]
    fn rule_4b_non_task_complete_skips_on_resume() {
        use crate::magician_v2::pipeline::system_agents::intent_classifier::IntentClassificationResult;
        use crate::magician_v2::query_analysis::intent::QueryIntent;

        let mut ctx = base_context();
        ctx.resume_mode = Some("full_replan".to_string());
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");

        // Seed a fresh non-task IntentClassification — normally would terminate pipeline.
        let ic = IntentClassificationResult {
            intent: QueryIntent::StatusQuery,
            requires_pipeline: false,
            user_answer: None,
            is_new_task: false,
            classification_duration_ms: 0,
        };
        store.put(make_artifact_at(
            "ic-1",
            ArtifactType::IntentClassification,
            run_started + chrono::Duration::milliseconds(10),
            serde_json::to_value(&ic).unwrap(),
        ));

        assert!(
            rule_non_task_complete(&store, &ctx).is_none(),
            "rule_non_task_complete must not fire on resume-from-suspension"
        );
    }

    // 17. rule_5_analysis_routes_to_slot_extractor
    #[test]
    fn rule_5_analysis_routes_to_slot_extractor() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            Utc::now(),
            json!({}),
        ));

        let decision = rule_needs_slot_extraction(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:slot-extractor");
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 18. rule_6_slots_route_to_elicitor
    #[test]
    fn rule_6_slots_route_to_elicitor() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "sg-1",
            ArtifactType::SlotGraph,
            Utc::now(),
            json!({}),
        ));

        let decision = rule_needs_elicitation(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:elicitor");
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 19. rule_7_clarification_returns_pause
    #[test]
    fn rule_7_clarification_returns_pause() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "sg-1",
            ArtifactType::SlotGraph,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "er-1",
            ArtifactType::ElicitationResult,
            Utc::now(),
            json!({
                "needs_clarification": true,
                "question_text": "What city?",
                "slot_ids": ["city", "date"]
            }),
        ));

        let decision = rule_needs_clarification(&store, &ctx);
        match decision {
            Some(RoutingDecision::Pause {
                question_text,
                slot_ids,
            }) => {
                assert_eq!(question_text, "What city?");
                assert_eq!(slot_ids, vec!["city", "date"]);
            },
            other => panic!("expected Pause, got {:?}", other),
        }
    }

    // 20. rule_7_stale_clarification_skips
    #[test]
    fn rule_7_stale_clarification_skips() {
        let ctx = base_context();
        let t1 = Utc::now();
        let t2 = t1 + Duration::milliseconds(100);

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "er-1",
            ArtifactType::ElicitationResult,
            t1,
            json!({"needs_clarification": true, "question_text": "What city?"}),
        ));
        // InterpretedAnswer produced after elicitation
        store.put(make_artifact_at(
            "ia-1",
            ArtifactType::InterpretedAnswer,
            t2,
            json!({"requires_replan": false}),
        ));

        assert!(rule_needs_clarification(&store, &ctx).is_none());
    }

    // 21. rule_7_stale_clarification_skips_via_clarified_task
    #[test]
    fn rule_7_stale_clarification_skips_via_clarified_task() {
        let ctx = base_context();
        let t1 = Utc::now();
        let t2 = t1 + Duration::milliseconds(100);

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "er-1",
            ArtifactType::ElicitationResult,
            t1,
            json!({"needs_clarification": true, "question_text": "What city?"}),
        ));
        // ClarifiedTask produced after elicitation
        store.put(make_artifact_at(
            "ct-1",
            ArtifactType::ClarifiedTask,
            t2,
            json!({}),
        ));

        assert!(rule_needs_clarification(&store, &ctx).is_none());
    }

    // 21b. rule_needs_clarification_returns_error_when_slot_ids_empty
    #[test]
    fn rule_needs_clarification_returns_error_when_slot_ids_empty() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("chain-1");
        // ElicitationResult with needs_clarification=true but empty slot_ids
        store.put(make_artifact_at(
            "elicit-1",
            ArtifactType::ElicitationResult,
            Utc::now(),
            json!({ "needs_clarification": true, "slot_ids": [] }),
        ));
        let result = rule_needs_clarification(&store, &ctx);
        assert!(
            matches!(result, Some(RoutingDecision::Error { .. })),
            "should return Error when slot_ids is empty but needs_clarification=true, got {:?}",
            result
        );
    }

    // 22. rule_8_resolved_routes_to_rewriter
    #[test]
    fn rule_8_resolved_routes_to_rewriter() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "sg-1",
            ArtifactType::SlotGraph,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "er-1",
            ArtifactType::ElicitationResult,
            Utc::now(),
            json!({"needs_clarification": false}),
        ));

        let decision = rule_slots_resolved(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:query-rewriter");
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 23. rule_9_clarified_routes_to_planner
    #[test]
    fn rule_9_clarified_routes_to_planner() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "sg-1",
            ArtifactType::SlotGraph,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "er-1",
            ArtifactType::ElicitationResult,
            Utc::now(),
            json!({"needs_clarification": false}),
        ));
        store.put(make_artifact_at(
            "ct-1",
            ArtifactType::ClarifiedTask,
            Utc::now() + Duration::milliseconds(10),
            json!({}),
        ));

        let decision = rule_ready_to_plan(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:planner");
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 24a. rule_planning_complete defers (None) when PlanGraph fails to deserialize
    #[test]
    fn rule_10_plangraph_defers_on_deser_failure() {
        // C-03: malformed PlanGraph content should return None (defer), NOT Complete.
        // Previously returned Complete, which caused the pipeline to declare success
        // without executing any steps.
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "pg-1",
            ArtifactType::PlanGraph,
            Utc::now(),
            // Wrong type for steps (string instead of array) → deserialization fails
            json!({"steps": "not-an-array"}),
        ));

        let decision = rule_planning_complete(&store, &ctx);
        assert!(
            decision.is_none(),
            "expected None (defer) on deser failure, got {:?}",
            decision
        );
    }

    // 24b. rule_planning_complete returns Complete for a valid empty-steps plan
    #[test]
    fn rule_10_plangraph_returns_complete_when_all_steps_done() {
        let ctx = base_context();
        let mut store = ArtifactStore::new("test-chain");
        // Valid PlanGraph with no steps — vacuously all steps are done → Complete.
        store.put(make_artifact_at(
            "pg-1",
            ArtifactType::PlanGraph,
            Utc::now(),
            json!({
                "steps": [],
                "edges": [],
                "unresolved_inputs": [],
                "confidence": 1.0,
                "provenance": {"strategy": "test"}
            }),
        ));

        let decision = rule_planning_complete(&store, &ctx);
        match decision {
            Some(RoutingDecision::Complete { .. }) => {},
            other => panic!("expected Complete, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // P5.5-B-04 — Priority test (25)
    // -----------------------------------------------------------------------

    // 25. rule_priority_resume_over_forward
    #[test]
    fn rule_priority_resume_over_forward() {
        // user_answer present + empty store (no QueryAnalysis) — rule 1
        // should fire before rule 4 (cold_start).
        let mut ctx = base_context();
        ctx.user_answer = Some("yes".to_string());

        let store = ArtifactStore::new("test-chain");

        let rules = default_rules();
        let mut result = None;
        for rule in &rules {
            if let Some(decision) = rule(&store, &ctx) {
                result = Some(decision);
                break;
            }
        }

        match result {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:answer-interpreter");
            },
            other => panic!(
                "expected NextStage(system:answer-interpreter), got {:?}",
                other
            ),
        }
    }

    // -----------------------------------------------------------------------
    // P5.5-B-04 — TieredRouter async tests (26–27)
    // -----------------------------------------------------------------------

    // 26. tiered_router_cold_start_routes_to_intent_classifier
    #[tokio::test]
    async fn tiered_router_cold_start_routes_to_intent_classifier() {
        let router = TieredRouter::new(None, vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::NextStage { agent_id, .. } => {
                assert_eq!(agent_id, "system:intent-classifier");
            },
            other => panic!(
                "expected NextStage(system:intent-classifier) for missing QueryAnalysis, got {:?}",
                other,
            ),
        }
    }

    // 27. tiered_router_no_rule_match_no_llm_returns_error
    #[tokio::test]
    async fn tiered_router_no_rule_match_no_llm_returns_error() {
        // Construct a TieredRouter with an empty rules vec so no rule matches.
        let router = TieredRouter::new_with_rules(vec![], None, vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Error { reason } => {
                assert!(reason.contains("no routing rule matched"));
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // P5.5-B-05 — LLM fallback tests (28–36)
    // -----------------------------------------------------------------------

    /// Mock LLM that returns a pre-configured response string.
    struct MockRouterLLM {
        response: String,
    }

    #[async_trait]
    impl RouterLLM for MockRouterLLM {
        async fn route(&self, _prompt: &str) -> Result<String, String> {
            Ok(self.response.clone())
        }
    }

    // 28. llm_fallback_returns_valid_decision
    #[tokio::test]
    async fn llm_fallback_returns_valid_decision() {
        let mock = Arc::new(MockRouterLLM {
            response:
                r#"{"decision":"next_stage","agent_id":"system:planner","reason":"ready to plan"}"#
                    .to_string(),
        });
        let router =
            TieredRouter::new_with_rules(vec![], Some(mock), vec!["system:planner".to_string()]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::NextStage { agent_id, reason } => {
                assert_eq!(agent_id, "system:planner");
                assert_eq!(reason, "ready to plan");
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
        // Counter should have been incremented.
        assert_eq!(ctx.llm_routing_calls, 1);
    }

    // 29. llm_fallback_parse_pause
    #[tokio::test]
    async fn llm_fallback_parse_pause() {
        let mock = Arc::new(MockRouterLLM {
            response:
                r#"{"decision":"pause","question_text":"Which city?","slot_ids":["city","date"]}"#
                    .to_string(),
        });
        let router = TieredRouter::new_with_rules(vec![], Some(mock), vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Pause {
                question_text,
                slot_ids,
            } => {
                assert_eq!(question_text, "Which city?");
                assert_eq!(slot_ids, vec!["city", "date"]);
            },
            other => panic!("expected Pause, got {:?}", other),
        }
    }

    // 30. llm_fallback_parse_complete
    #[tokio::test]
    async fn llm_fallback_parse_complete() {
        let mock = Arc::new(MockRouterLLM {
            response: r#"{"decision":"complete","reason":"all tasks done"}"#.to_string(),
        });
        let router = TieredRouter::new_with_rules(vec![], Some(mock), vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Complete { reason } => {
                assert_eq!(reason, "all tasks done");
            },
            other => panic!("expected Complete, got {:?}", other),
        }
    }

    // 31. llm_fallback_parse_error
    #[tokio::test]
    async fn llm_fallback_parse_error() {
        let mock = Arc::new(MockRouterLLM {
            response: r#"{"decision":"error","reason":"something went wrong"}"#.to_string(),
        });
        let router = TieredRouter::new_with_rules(vec![], Some(mock), vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Error { reason } => {
                assert_eq!(reason, "something went wrong");
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // 32. llm_fallback_invalid_json_returns_error
    #[tokio::test]
    async fn llm_fallback_invalid_json_returns_error() {
        let mock = Arc::new(MockRouterLLM {
            response: "this is not json at all!".to_string(),
        });
        let router = TieredRouter::new_with_rules(vec![], Some(mock), vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Error { reason } => {
                assert!(
                    reason.contains("LLM returned unparseable"),
                    "expected 'LLM returned unparseable' in reason, got: {reason}"
                );
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // 33. llm_fallback_budget_cap_at_2
    // Verifies that llm_routing_calls >= 2 triggers the budget cap.
    // With max=2, once 2 calls have been made the 3rd must be blocked.
    #[tokio::test]
    async fn llm_fallback_budget_cap_at_2() {
        let mock = Arc::new(MockRouterLLM {
            response: r#"{"decision":"complete","reason":"done"}"#.to_string(),
        });
        let router = TieredRouter::new_with_rules(vec![], Some(mock), vec![]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();
        // Set counter to exactly 2 — the boundary value that should block the call.
        ctx.llm_routing_calls = 2;

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Error { reason } => {
                assert!(
                    reason.contains("LLM routing budget exhausted"),
                    "expected budget exhausted message, got: {reason}"
                );
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // 34. llm_fallback_unknown_agent_returns_error
    #[tokio::test]
    async fn llm_fallback_unknown_agent_returns_error() {
        let mock = Arc::new(MockRouterLLM {
            response:
                r#"{"decision":"next_stage","agent_id":"system:nonexistent","reason":"pick this"}"#
                    .to_string(),
        });
        let router =
            TieredRouter::new_with_rules(vec![], Some(mock), vec!["system:planner".to_string()]);
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::Error { reason } => {
                assert!(
                    reason.contains("LLM suggested unknown agent: system:nonexistent"),
                    "expected unknown agent message, got: {reason}"
                );
            },
            other => panic!("expected Error, got {:?}", other),
        }
    }

    // 35. llm_fallback_valid_agent_passes_validation
    #[tokio::test]
    async fn llm_fallback_valid_agent_passes_validation() {
        let mock = Arc::new(MockRouterLLM {
            response: r#"{"decision":"next_stage","agent_id":"system:elicitor","reason":"need elicitation"}"#
                .to_string(),
        });
        let router = TieredRouter::new_with_rules(
            vec![],
            Some(mock),
            vec!["system:planner".to_string(), "system:elicitor".to_string()],
        );
        let store = ArtifactStore::new("test-chain");
        let mut ctx = base_context();

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::NextStage { agent_id, reason } => {
                assert_eq!(agent_id, "system:elicitor");
                assert_eq!(reason, "need elicitation");
            },
            other => panic!("expected NextStage, got {:?}", other),
        }
    }

    // 36. build_routing_prompt_includes_artifacts_and_agents
    #[test]
    fn build_routing_prompt_includes_artifacts_and_agents() {
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            Utc::now(),
            json!({}),
        ));
        store.put(make_artifact_at(
            "sg-1",
            ArtifactType::SlotGraph,
            Utc::now(),
            json!({}),
        ));

        let ctx = base_context();
        let agents = vec!["system:planner".to_string(), "system:elicitor".to_string()];

        let prompt = build_routing_prompt(&store, &ctx, &agents);

        // Verify artifact types appear
        assert!(
            prompt.contains("query_analysis"),
            "prompt should mention query_analysis, got:\n{prompt}"
        );
        assert!(
            prompt.contains("slot_graph"),
            "prompt should mention slot_graph, got:\n{prompt}"
        );
        // Verify agents appear
        assert!(
            prompt.contains("system:planner"),
            "prompt should mention system:planner, got:\n{prompt}"
        );
        assert!(
            prompt.contains("system:elicitor"),
            "prompt should mention system:elicitor, got:\n{prompt}"
        );
        // Verify pipeline context
        assert!(
            prompt.contains("test query"),
            "prompt should mention the query, got:\n{prompt}"
        );
    }

    #[test]
    fn retry_decision_is_constructible() {
        let d = RoutingDecision::Retry {
            agent_id: "system:elicitor".to_string(),
            reason: "LLM timeout".to_string(),
            delay_ms: 1000,
        };
        match d {
            RoutingDecision::Retry {
                agent_id, delay_ms, ..
            } => {
                assert_eq!(agent_id, "system:elicitor");
                assert_eq!(delay_ms, 1000);
            },
            _ => panic!("expected Retry"),
        }
    }

    #[test]
    fn rule_service_error_retry_fires_for_service_errors() {
        let mut store = ArtifactStore::new("test-chain".to_string());
        store.put(AgentArtifact {
            artifact_id: "e1".to_string(),
            artifact_type: ArtifactType::AgentError,
            producer_agent_id: "system:orchestrator".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({
                "failed_agent_id": "system:elicitor",
                "error": "underlying service error: connection timeout",
                "retry_count": 0,
                "recoverable": true,
            }),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: chrono::Utc::now(),
            render_hints: None,
        });
        let context = PipelineContext {
            workflow_id: "wf-1".to_string(),
            chain_id: "chain-1".to_string(),
            cycle_id: "c1".to_string(),
            query: "test".to_string(),
            ..Default::default()
        };
        let decision = rule_service_error_retry(&store, &context);
        assert!(
            matches!(decision, Some(RoutingDecision::Retry { ref agent_id, delay_ms: 1000, .. })
                if agent_id == "system:elicitor"),
            "expected Retry for system:elicitor, got {:?}",
            decision
        );
    }

    #[test]
    fn rule_service_error_retry_does_not_fire_for_hard_errors() {
        let mut store = ArtifactStore::new("test-chain".to_string());
        store.put(AgentArtifact {
            artifact_id: "e1".to_string(),
            artifact_type: ArtifactType::AgentError,
            producer_agent_id: "system:orchestrator".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({
                "failed_agent_id": "system:elicitor",
                "error": "agent execution failed: missing required slot",
                "retry_count": 0,
                "recoverable": true,
            }),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: chrono::Utc::now(),
            render_hints: None,
        });
        let context = PipelineContext {
            workflow_id: "wf-1".to_string(),
            chain_id: "chain-1".to_string(),
            cycle_id: "c1".to_string(),
            query: "test".to_string(),
            ..Default::default()
        };
        assert!(rule_service_error_retry(&store, &context).is_none());
    }

    #[test]
    fn rule_service_error_retry_does_not_fire_when_budget_exhausted() {
        let mut store = ArtifactStore::new("test-chain".to_string());
        store.put(AgentArtifact {
            artifact_id: "e1".to_string(),
            artifact_type: ArtifactType::AgentError,
            producer_agent_id: "system:orchestrator".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({
                "failed_agent_id": "system:elicitor",
                "error": "underlying service error: timeout",
                "retry_count": 2,
                "recoverable": false,
            }),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: chrono::Utc::now(),
            render_hints: None,
        });
        let context = PipelineContext {
            workflow_id: "wf-1".to_string(),
            chain_id: "chain-1".to_string(),
            cycle_id: "c1".to_string(),
            query: "test".to_string(),
            ..Default::default()
        };
        assert!(rule_service_error_retry(&store, &context).is_none());
    }

    // -----------------------------------------------------------------------
    // Phase C — refinement budget tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn default_router_keeps_plannable_queries_on_full_staged_path() {
        let router = TieredRouter::new(None, vec![]);
        let mut ctx = base_context();
        let run_started = ctx.run_started_at.unwrap();
        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "qa-1",
            ArtifactType::QueryAnalysis,
            run_started + Duration::milliseconds(10),
            json!({
                "original_query": "test query",
                "complexity": {"score": 0.1, "factors": [], "reasoning": ""},
                "categories": {"categories": [], "reasoning": ""},
                "dependencies": {"is_multi_step": false, "dependencies": [], "workflow_steps": [], "reasoning": "", "required_capabilities": []},
                "resource_estimate": {"expected_tokens": 0},
                "intent": "new_task",
                "llm_calls_used": 0,
                "task_clarity": "Plannable"
            }),
        ));

        let decision = router.route(&store, &mut ctx).await;
        match decision {
            RoutingDecision::NextStage { agent_id, reason } => {
                assert_eq!(agent_id, "system:slot-extractor");
                assert!(
                    reason.contains("slot extraction"),
                    "expected staged pipeline reason, got: {reason}"
                );
            },
            other => panic!(
                "expected plannable queries to continue into slot extraction, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn rule_needs_refinement_allows_second_round_before_budget_exhaustion() {
        let mut ctx = base_context();
        ctx.refinement_rounds = MAX_REFINEMENT_ROUNDS - 1;
        ctx.refinement_user_pauses = MAX_REFINEMENT_USER_PAUSES - 1;
        let run_started = ctx.run_started_at.unwrap();
        let plan_graph = PlanGraph {
            steps: vec![crate::magician_v2::strategy::plan::PlanStep {
                id: "step-1".to_string(),
                task: "Gather the missing target details".to_string(),
                confidence: 0.8,
                readiness: Some(StepReadiness::Weak),
                ..Default::default()
            }],
            confidence: 0.8,
            provenance: crate::magician_v2::strategy::plan::PlanProvenance {
                strategy: "test".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "pg-1",
            ArtifactType::PlanGraph,
            run_started + Duration::milliseconds(10),
            serde_json::to_value(&plan_graph).unwrap(),
        ));

        let decision = rule_needs_refinement(&store, &ctx);
        match decision {
            Some(RoutingDecision::NextStage { agent_id, .. }) => {
                assert_eq!(agent_id, "system:slot-extractor");
            },
            other => panic!(
                "expected refinement to remain available below the budget, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn rule_needs_refinement_blocks_when_round_budget_is_exhausted() {
        let mut ctx = base_context();
        ctx.refinement_rounds = MAX_REFINEMENT_ROUNDS;
        let run_started = ctx.run_started_at.unwrap();
        let plan_graph = PlanGraph {
            steps: vec![crate::magician_v2::strategy::plan::PlanStep {
                id: "step-1".to_string(),
                task: "Gather the missing target details".to_string(),
                confidence: 0.8,
                readiness: Some(StepReadiness::Weak),
                ..Default::default()
            }],
            confidence: 0.8,
            provenance: crate::magician_v2::strategy::plan::PlanProvenance {
                strategy: "test".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let mut store = ArtifactStore::new("test-chain");
        store.put(make_artifact_at(
            "pg-1",
            ArtifactType::PlanGraph,
            run_started + Duration::milliseconds(10),
            serde_json::to_value(&plan_graph).unwrap(),
        ));

        assert!(
            rule_needs_refinement(&store, &ctx).is_none(),
            "round budget exhaustion should stop additional refinement"
        );
    }
}
