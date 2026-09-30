//! Workflow compilation: turn N captured CapabilitySequences for the same
//! origin into one replayable WorkflowGraph.
//!
//! Two-pass:
//! 1. `auto_match::infer_auto_data_flows` — deterministic string matching
//!    across sequence step boundaries. No LLM. Handles common cases
//!    (session tokens, entity ids, pagination cursors).
//! 2. `llm_compile::compile_with_llm` — LLM call producing the final
//!    WorkflowGraph with `LlmInferred` data flows, parameter
//!    classifications, and skip conditions.

pub mod auto_match;
pub mod llm_compile;

pub use auto_match::infer_auto_data_flows;
pub use llm_compile::{compile_with_llm, CompilationError, CompilationInput};

use crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext;
use crate::magician_v2::api_mining::sequence::{CapabilitySequence, ExecutionPath};
use crate::magician_v2::api_mining::workflow::{
    AuthRequirements, BrowserFallbackStep, DataFlow, ParamSource, ReplayStats, WorkflowConfidence,
    WorkflowGraph, WorkflowMaturity, WorkflowStep,
};
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use std::collections::HashMap;
use std::sync::Arc;
use ulid::Ulid;

/// Orchestrate Pass 1 + Pass 2.
///
/// Runs auto-match deterministically, then hands the sequences + seeds to
/// the LLM. On LLM failure or validation failure (after one retry), falls
/// back to a Draft workflow containing only the auto-inferred flows.
pub async fn compile_from_sequences(
    router: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    origin_key: &str,
    sequences: &[CapabilitySequence],
) -> WorkflowGraph {
    compile_from_sequences_with_telemetry(router, prompt_manager, origin_key, sequences, None).await
}

pub async fn compile_from_sequences_with_telemetry(
    router: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    origin_key: &str,
    sequences: &[CapabilitySequence],
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> WorkflowGraph {
    let auto_data_flows = infer_auto_data_flows(sequences);
    match llm_compile::compile_with_llm_and_telemetry(
        router,
        prompt_manager,
        CompilationInput {
            origin_key,
            sequences,
            auto_data_flows: &auto_data_flows,
        },
        telemetry,
    )
    .await
    {
        Ok(mut wf) => {
            if llm_dropped_replayable_canonical_steps(&wf, sequences) {
                tracing::warn!(
                    "workflow_compiler: LLM graph for origin '{origin_key}' dropped replayable canonical steps. Falling back to deterministic Draft graph."
                );
                return draft_fallback(origin_key, sequences, auto_data_flows);
            }
            apply_deterministic_param_sources(&mut wf, sequences, &auto_data_flows);
            // Stamp last_compiled_at if the LLM left it at 0.
            if wf.last_compiled_at_ms == 0 {
                wf.last_compiled_at_ms = chrono::Utc::now().timestamp_millis();
            }
            wf
        },
        Err(err) => {
            tracing::warn!(
                "workflow_compiler: LLM compilation failed for origin '{origin_key}': {err}. Falling back to Draft graph with auto-inferred flows only."
            );
            draft_fallback(origin_key, sequences, auto_data_flows)
        },
    }
}

/// Build a minimal Draft workflow from the longest sequence as canonical
/// ordering. Used when the LLM call or validation fails after retry.
pub fn draft_fallback(
    origin_key: &str,
    sequences: &[CapabilitySequence],
    auto_data_flows: Vec<DataFlow>,
) -> WorkflowGraph {
    // Take the longest sequence as the canonical step ordering.
    let canonical = sequences
        .iter()
        .max_by_key(|s| s.steps.len())
        .expect("draft_fallback called with empty sequences");
    let flow_by_target = flow_by_target(&auto_data_flows);

    let steps: Vec<WorkflowStep> = canonical
        .steps
        .iter()
        .map(|step| {
            let step_id = format!("step_{}", step.step_index);
            WorkflowStep {
                id: step_id.clone(),
                step_index: step.step_index,
                capability_id: step.capability_id.clone(),
                param_sources: deterministic_param_sources_for_step(
                    &step_id,
                    &step.request_params,
                    &flow_by_target,
                ),
                skip_if: None,
                browser_only: step.capability_id.is_none()
                    && step.executed_via == ExecutionPath::Browser,
                browser_fallback: browser_fallback_from_sequence_step(step),
            }
        })
        .collect();

    WorkflowGraph {
        id: format!("wf_{}", Ulid::new()),
        origin_key: origin_key.to_string(),
        name: format!("Draft workflow for {origin_key}"),
        steps,
        data_flows: auto_data_flows,
        auth_requirements: AuthRequirements::default(),
        confidence: WorkflowConfidence {
            workflow_level: WorkflowMaturity::Draft,
            step_confidences: HashMap::new(),
        },
        compiled_from_sequence_ids: sequences.iter().map(|s| s.id.clone()).collect(),
        last_compiled_at_ms: chrono::Utc::now().timestamp_millis(),
        last_replayed_at_ms: None,
        replay_stats: ReplayStats::default(),
    }
}

fn browser_fallback_from_sequence_step(
    step: &crate::magician_v2::api_mining::sequence::SequenceStep,
) -> Option<BrowserFallbackStep> {
    let action = step.browser_action.as_ref()?.trim();
    if action.is_empty() {
        return None;
    }
    Some(BrowserFallbackStep {
        action: action.to_string(),
        arguments: step
            .browser_arguments
            .clone()
            .unwrap_or(serde_json::Value::Null),
        description: step.browser_action_desc.clone(),
    })
}

fn llm_dropped_replayable_canonical_steps(
    wf: &WorkflowGraph,
    sequences: &[CapabilitySequence],
) -> bool {
    let Some(canonical) = sequences.iter().max_by_key(|s| s.steps.len()) else {
        return false;
    };
    let expected = canonical
        .steps
        .iter()
        .filter(|step| step.capability_id.is_some())
        .count();
    let actual = wf
        .steps
        .iter()
        .filter(|step| step.capability_id.is_some() && !step.browser_only)
        .count();
    actual < expected
}

fn apply_deterministic_param_sources(
    wf: &mut WorkflowGraph,
    sequences: &[CapabilitySequence],
    data_flows: &[DataFlow],
) {
    let Some(canonical) = sequences.iter().max_by_key(|s| s.steps.len()) else {
        return;
    };
    let flow_by_target = flow_by_target(data_flows);
    for sequence_step in &canonical.steps {
        if sequence_step.request_params.is_empty() {
            continue;
        }
        let step_id = format!("step_{}", sequence_step.step_index);
        let Some(workflow_step) = wf.steps.iter_mut().find(|step| step.id == step_id) else {
            continue;
        };
        let defaults = deterministic_param_sources_for_step(
            &step_id,
            &sequence_step.request_params,
            &flow_by_target,
        );
        for (param, source) in defaults {
            workflow_step.param_sources.entry(param).or_insert(source);
        }
    }
}

fn deterministic_param_sources_for_step(
    step_id: &str,
    request_params: &HashMap<String, String>,
    flow_by_target: &HashMap<(String, String), String>,
) -> HashMap<String, ParamSource> {
    request_params
        .iter()
        .map(|(param, value)| {
            let source = flow_by_target
                .get(&(step_id.to_string(), param.clone()))
                .map(|data_flow_id| ParamSource::DataFlow {
                    data_flow_id: data_flow_id.clone(),
                })
                .unwrap_or_else(|| ParamSource::Literal {
                    value: value.clone(),
                });
            (param.clone(), source)
        })
        .collect()
}

fn flow_by_target(data_flows: &[DataFlow]) -> HashMap<(String, String), String> {
    data_flows
        .iter()
        .map(|flow| {
            (
                (flow.target_step.clone(), flow.target_param.clone()),
                flow.id.clone(),
            )
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::sequence::{
        CapabilitySequence, ExecutionPath, SequenceStep,
    };

    fn step(idx: usize, has_cap: bool) -> SequenceStep {
        SequenceStep {
            step_index: idx,
            capability_id: if has_cap {
                Some(format!("cap_{idx}"))
            } else {
                None
            },
            origin: "https://example.com".to_string(),
            concrete_url: format!("https://example.com/{idx}"),
            method: "GET".to_string(),
            request_params: HashMap::new(),
            request_body: None,
            response_status: None,
            response_body: None,
            action_binding_id: None,
            browser_action_desc: if has_cap {
                None
            } else {
                Some(format!("browser__click #step-{idx}"))
            },
            browser_action: if has_cap {
                None
            } else {
                Some("click".to_string())
            },
            browser_arguments: if has_cap {
                None
            } else {
                Some(serde_json::json!({"args":[format!("#step-{idx}")]}))
            },
            executed_via: if has_cap {
                ExecutionPath::ApiReplay
            } else {
                ExecutionPath::Browser
            },
            timestamp_ms: 0,
            duration_ms: 0,
        }
    }

    #[test]
    fn draft_fallback_uses_longest_sequence_as_canonical() {
        let short = CapabilitySequence {
            id: "seq_short".to_string(),
            task_id: "t".to_string(),
            execution_id: "e".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![step(0, true)],
            captured_at_ms: 0,
            finalized: true,
        };
        let long = CapabilitySequence {
            id: "seq_long".to_string(),
            task_id: "t".to_string(),
            execution_id: "e".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![step(0, true), step(1, true), step(2, false)],
            captured_at_ms: 0,
            finalized: true,
        };
        let wf = draft_fallback("example.com", &[short, long], vec![]);
        assert_eq!(wf.steps.len(), 3);
        assert!(wf.steps[2].browser_only);
        let fallback = wf.steps[2]
            .browser_fallback
            .as_ref()
            .expect("browser fallback should be preserved");
        assert_eq!(fallback.action, "click");
        assert_eq!(fallback.arguments, serde_json::json!({"args":["#step-2"]}));
        assert_eq!(wf.compiled_from_sequence_ids.len(), 2);
        assert!(matches!(
            wf.confidence.workflow_level,
            WorkflowMaturity::Draft
        ));
    }

    #[test]
    fn draft_fallback_populates_dataflow_and_literal_param_sources() {
        let mut first = step(0, true);
        first.response_body = Some(r#"{"hits":[{"story_id":22238335}]}"#.to_string());
        let mut second = step(1, true);
        second.request_params = HashMap::from([
            ("item_id".to_string(), "22238335".to_string()),
            ("format".to_string(), "json".to_string()),
        ]);
        let seq = CapabilitySequence {
            id: "seq".to_string(),
            task_id: "t".to_string(),
            execution_id: "e".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![first, second],
            captured_at_ms: 0,
            finalized: true,
        };
        let flow = DataFlow {
            id: "df_story".to_string(),
            source_step: "step_0".to_string(),
            source_path: "$..story_id".to_string(),
            target_step: "step_1".to_string(),
            target_param: "item_id".to_string(),
            inference_method: crate::magician_v2::api_mining::workflow::InferenceMethod::AutoMatch,
            confidence: 0.9,
        };

        let wf = draft_fallback("example.com", &[seq], vec![flow]);
        let step_1 = wf.steps.iter().find(|step| step.id == "step_1").unwrap();
        assert!(matches!(
            step_1.param_sources.get("item_id"),
            Some(ParamSource::DataFlow { data_flow_id }) if data_flow_id == "df_story"
        ));
        assert!(matches!(
            step_1.param_sources.get("format"),
            Some(ParamSource::Literal { value }) if value == "json"
        ));
    }
}
