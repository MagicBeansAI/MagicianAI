//! Pass 2 of workflow compilation: LLM-driven.
//!
//! Takes the sequences + Pass 1 auto data flows, calls the
//! `WorkflowCompilation` LLM operation, parses + validates the result, and
//! returns a finalized `WorkflowGraph`. Validation failures trigger one
//! retry with the validation errors fed back to the LLM; a second failure
//! returns an error and the caller can fall back to a Draft graph with
//! only the auto-inferred flows.

use crate::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use crate::magician_v2::api_mining::sequence::CapabilitySequence;
use crate::magician_v2::api_mining::workflow::{DataFlow, WorkflowGraph};
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};
use std::collections::HashMap;
use std::sync::Arc;

pub struct CompilationInput<'a> {
    pub origin_key: &'a str,
    pub sequences: &'a [CapabilitySequence],
    pub auto_data_flows: &'a [DataFlow],
}

#[derive(Debug)]
pub enum CompilationError {
    LlmCall(String),
    Parse(String),
    Validation(Vec<String>),
    EmptyInput,
    PromptLoad(String),
}

impl std::fmt::Display for CompilationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LlmCall(msg) => write!(f, "LLM call failed: {msg}"),
            Self::Parse(msg) => write!(f, "failed to parse LLM output as WorkflowGraph: {msg}"),
            Self::Validation(errs) => {
                write!(f, "WorkflowGraph validation failed: {}", errs.join("; "))
            },
            Self::EmptyInput => write!(f, "no sequences supplied for compilation"),
            Self::PromptLoad(msg) => {
                write!(f, "failed to load WorkflowCompilation system prompt: {msg}")
            },
        }
    }
}

impl std::error::Error for CompilationError {}

/// Run Pass 2: call the LLM, parse, validate, retry once on validation failure.
pub async fn compile_with_llm(
    router: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    input: CompilationInput<'_>,
) -> Result<WorkflowGraph, CompilationError> {
    compile_with_llm_and_telemetry(router, prompt_manager, input, None).await
}

pub async fn compile_with_llm_and_telemetry(
    router: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    input: CompilationInput<'_>,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<WorkflowGraph, CompilationError> {
    if input.sequences.is_empty() {
        return Err(CompilationError::EmptyInput);
    }
    let router = telemetry.map_or(router.clone(), |telemetry| {
        Arc::new(router.with_scope_context(Some(telemetry.scope())))
    });

    let system_prompt = load_system_prompt(&prompt_manager).await?;
    let user_prompt = serialize_input(&input);
    let mut last_validation_errs: Option<Vec<String>> = None;

    for attempt in 0..2 {
        let user_payload = match last_validation_errs.as_ref() {
            Some(errs) => format!(
                "{user_prompt}\n\n## Previous attempt validation errors\n{}",
                errs.join("\n")
            ),
            None => user_prompt.clone(),
        };

        let llm_started = std::time::Instant::now();
        let raw_response = router
            .generate_for_operation_with_system(
                &LLMOperation::WorkflowCompilation,
                Some(&system_prompt),
                &user_payload,
            )
            .await
            .map_err(|e| CompilationError::LlmCall(e.to_string()))?;
        let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let attribution = OperationLlmCallAttribution {
            attempt: Some(attempt + 1),
            ..OperationLlmCallAttribution::default()
        };
        let raw_text = raw_response.content.as_str();
        let parsed: WorkflowGraph = match parse_workflow_graph(raw_text) {
            Ok(wf) => wf,
            Err(parse_err) => {
                if let Some(telemetry) = telemetry {
                    telemetry.emit_validation_failure(
                        LLMOperation::WorkflowCompilation.as_str(),
                        &raw_response,
                        latency_ms,
                        attribution,
                        "workflow_compilation_json",
                        &parse_err.to_string(),
                    );
                }
                if attempt == 0 {
                    last_validation_errs = Some(vec![format!("parse error: {parse_err}")]);
                    continue;
                }
                return Err(CompilationError::Parse(parse_err));
            },
        };

        match validate(&parsed, input.sequences) {
            Ok(()) => {
                if let Some(telemetry) = telemetry {
                    telemetry.emit_validated_success(
                        LLMOperation::WorkflowCompilation.as_str(),
                        &raw_response,
                        latency_ms,
                        attribution,
                        "workflow_compilation_schema",
                    );
                }
                return Ok(parsed);
            },
            Err(errs) => {
                if let Some(telemetry) = telemetry {
                    telemetry.emit_validation_failure(
                        LLMOperation::WorkflowCompilation.as_str(),
                        &raw_response,
                        latency_ms,
                        attribution,
                        "workflow_compilation_schema",
                        &errs.join("; "),
                    );
                }
                if attempt == 0 {
                    last_validation_errs = Some(errs);
                    continue;
                }
                return Err(CompilationError::Validation(errs));
            },
        }
    }

    unreachable!("compile_with_llm exits via Ok or Err inside the loop")
}

/// Load + render the WorkflowCompilation system prompt from the registry.
async fn load_system_prompt(prompt_manager: &PromptManager) -> Result<String, CompilationError> {
    let prompt = prompt_manager
        .get_prompt(
            crate::magician_v2::prompts::names::WORKFLOW_COMPILATION_SYSTEM,
            crate::magician_v2::prompts::versions::WORKFLOW_COMPILATION_SYSTEM,
        )
        .await
        .map_err(|e| CompilationError::PromptLoad(e.to_string()))?;
    prompt
        .render(&HashMap::new())
        .map_err(|e| CompilationError::PromptLoad(e.to_string()))
}

/// Render the compilation input as the JSON user prompt the LLM sees.
fn serialize_input(input: &CompilationInput<'_>) -> String {
    // WorkflowCompilation may be mapped to a remote provider. Keep the prompt
    // structural: local deterministic matching has already consumed concrete
    // values, while the LLM only needs ordering, field shape, and flow seeds.
    // In particular, never serialize captured headers/cookies, URL query/path
    // values, browser fill arguments, or request/response scalar values here.
    let sequences = input
        .sequences
        .iter()
        .map(sequence_shape_for_llm)
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "origin_key": input.origin_key,
        "sequences": sequences,
        "auto_data_flows": input.auto_data_flows,
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn sequence_shape_for_llm(sequence: &CapabilitySequence) -> serde_json::Value {
    let steps = sequence
        .steps
        .iter()
        .map(|step| {
            let mut request_param_names = step.request_params.keys().cloned().collect::<Vec<_>>();
            request_param_names.sort();
            serde_json::json!({
                "step_index": step.step_index,
                "capability_id": step.capability_id,
                "origin": safe_origin(&step.origin),
                "concrete_url": safe_origin(&step.concrete_url),
                "method": step.method,
                "request_param_names": request_param_names,
                "request_body": step.request_body.as_deref().map(json_shape),
                "response_status": step.response_status,
                "response_body": step.response_body.as_deref().map(json_shape),
                "action_binding_id": step.action_binding_id,
                "browser_action": step.browser_action,
                "executed_via": step.executed_via,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "id": sequence.id,
        "origin_key": sequence.origin_key,
        "steps": steps,
        "finalized": sequence.finalized,
    })
}

fn safe_origin(value: &str) -> String {
    let Ok(parsed) = url::Url::parse(value) else {
        return String::new();
    };
    parsed.origin().unicode_serialization()
}

fn json_shape(body: &str) -> serde_json::Value {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return serde_json::Value::String("<non-json-body>".to_string());
    };
    scalar_free_json_shape(value)
}

fn scalar_free_json_shape(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, scalar_free_json_shape(value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .take(3)
                .map(scalar_free_json_shape)
                .collect(),
        ),
        serde_json::Value::String(_) => serde_json::Value::String("<string>".to_string()),
        serde_json::Value::Number(_) => serde_json::Value::String("<number>".to_string()),
        serde_json::Value::Bool(_) => serde_json::Value::String("<boolean>".to_string()),
        serde_json::Value::Null => serde_json::Value::Null,
    }
}

/// Parse the LLM's raw response into a WorkflowGraph. Strips a leading
/// `` ```json `` fence if present (some models include them despite instructions).
fn parse_workflow_graph(raw: &str) -> Result<WorkflowGraph, String> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let body = body.strip_suffix("```").unwrap_or(body).trim();
    serde_json::from_str(body).map_err(|e| e.to_string())
}

/// Validate referential integrity. Returns Ok(()) when the graph is
/// internally consistent; otherwise returns a list of human-readable errors
/// to feed back into the LLM on retry.
pub(super) fn validate(
    wf: &WorkflowGraph,
    sequences: &[CapabilitySequence],
) -> Result<(), Vec<String>> {
    let mut errs = Vec::new();

    if wf.origin_key.is_empty() {
        errs.push("origin_key is empty".to_string());
    }
    if wf.steps.is_empty() {
        errs.push("steps array is empty".to_string());
    }

    let step_ids: std::collections::HashSet<&str> =
        wf.steps.iter().map(|s| s.id.as_str()).collect();
    let step_id_to_index: HashMap<&str, usize> = wf
        .steps
        .iter()
        .map(|s| (s.id.as_str(), s.step_index))
        .collect();

    for (i, step) in wf.steps.iter().enumerate() {
        if step.step_index != i {
            errs.push(format!(
                "step '{}' has step_index={} but is at array position {}",
                step.id, step.step_index, i
            ));
        }
        if let Some(skip) = &step.skip_if {
            if !step_ids.contains(skip.source_step.as_str()) {
                errs.push(format!(
                    "step '{}' skip_if.source_step '{}' references unknown step",
                    step.id, skip.source_step
                ));
            } else if let Some(&src_idx) = step_id_to_index.get(skip.source_step.as_str()) {
                if src_idx >= step.step_index {
                    errs.push(format!(
                        "step '{}' skip_if.source_step '{}' is not a PRIOR step",
                        step.id, skip.source_step
                    ));
                }
            }
        }
    }

    for flow in &wf.data_flows {
        if !step_ids.contains(flow.source_step.as_str()) {
            errs.push(format!(
                "data_flow '{}' source_step '{}' references unknown step",
                flow.id, flow.source_step
            ));
        }
        if !step_ids.contains(flow.target_step.as_str()) {
            errs.push(format!(
                "data_flow '{}' target_step '{}' references unknown step",
                flow.id, flow.target_step
            ));
        }
        if let (Some(&src), Some(&tgt)) = (
            step_id_to_index.get(flow.source_step.as_str()),
            step_id_to_index.get(flow.target_step.as_str()),
        ) {
            if src >= tgt {
                errs.push(format!(
                    "data_flow '{}' source_step '{}' must precede target_step '{}'",
                    flow.id, flow.source_step, flow.target_step
                ));
            }
        }
    }

    // Soft check: compiled_from_sequence_ids should reference real sequences.
    let observed_ids: std::collections::HashSet<&str> =
        sequences.iter().map(|s| s.id.as_str()).collect();
    for sid in &wf.compiled_from_sequence_ids {
        if !observed_ids.contains(sid.as_str()) {
            errs.push(format!(
                "compiled_from_sequence_ids references unknown sequence id '{sid}'"
            ));
        }
    }

    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::sequence::{
        CapabilitySequence, ExecutionPath, SequenceStep,
    };
    use crate::magician_v2::api_mining::workflow::{
        AuthRequirements, DataFlow, InferenceMethod, ReplayStats, SkipCondition, SkipOperator,
        WorkflowConfidence, WorkflowMaturity, WorkflowStep,
    };
    use std::collections::HashMap;

    fn sample_sequence(id: &str) -> CapabilitySequence {
        CapabilitySequence {
            id: id.to_string(),
            task_id: "t".to_string(),
            execution_id: "e".to_string(),
            origin_key: "example.com".to_string(),
            steps: vec![],
            captured_at_ms: 0,
            finalized: true,
        }
    }

    fn minimal_workflow() -> WorkflowGraph {
        WorkflowGraph {
            id: "wf_1".to_string(),
            origin_key: "example.com".to_string(),
            name: "test".to_string(),
            steps: vec![
                WorkflowStep {
                    id: "step_0".to_string(),
                    step_index: 0,
                    capability_id: Some("cap_login".to_string()),
                    param_sources: HashMap::new(),
                    skip_if: None,
                    browser_only: false,
                    browser_fallback: None,
                },
                WorkflowStep {
                    id: "step_1".to_string(),
                    step_index: 1,
                    capability_id: Some("cap_fetch".to_string()),
                    param_sources: HashMap::new(),
                    skip_if: None,
                    browser_only: false,
                    browser_fallback: None,
                },
            ],
            data_flows: vec![],
            auth_requirements: AuthRequirements::default(),
            confidence: WorkflowConfidence {
                workflow_level: WorkflowMaturity::Draft,
                step_confidences: HashMap::new(),
            },
            compiled_from_sequence_ids: vec!["seq_1".to_string()],
            last_compiled_at_ms: 0,
            last_replayed_at_ms: None,
            replay_stats: ReplayStats::default(),
        }
    }

    #[test]
    fn validate_accepts_well_formed_workflow() {
        let wf = minimal_workflow();
        let result = validate(&wf, &[sample_sequence("seq_1")]);
        assert!(result.is_ok(), "got: {:?}", result);
    }

    #[test]
    fn validate_rejects_mismatched_step_index() {
        let mut wf = minimal_workflow();
        wf.steps[0].step_index = 5;
        let errs = validate(&wf, &[sample_sequence("seq_1")]).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("step_index")),
            "got: {:?}",
            errs
        );
    }

    #[test]
    fn validate_rejects_backward_data_flow() {
        let mut wf = minimal_workflow();
        wf.data_flows.push(DataFlow {
            id: "df_bad".to_string(),
            source_step: "step_1".to_string(), // later
            source_path: "$.x".to_string(),
            target_step: "step_0".to_string(), // earlier
            target_param: "x".to_string(),
            inference_method: InferenceMethod::LlmInferred,
            confidence: 0.5,
        });
        let errs = validate(&wf, &[sample_sequence("seq_1")]).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("must precede")),
            "got: {:?}",
            errs
        );
    }

    #[test]
    fn validate_rejects_skip_if_referencing_future_step() {
        let mut wf = minimal_workflow();
        wf.steps[0].skip_if = Some(SkipCondition {
            source_step: "step_1".to_string(),
            source_path: "$.ok".to_string(),
            operator: SkipOperator::IsEmpty,
            value: serde_json::Value::Null,
        });
        let errs = validate(&wf, &[sample_sequence("seq_1")]).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("PRIOR step")),
            "got: {:?}",
            errs
        );
    }

    #[test]
    fn validate_rejects_unknown_sequence_reference() {
        let mut wf = minimal_workflow();
        wf.compiled_from_sequence_ids = vec!["seq_nope".to_string()];
        let errs = validate(&wf, &[sample_sequence("seq_1")]).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("unknown sequence")),
            "got: {:?}",
            errs
        );
    }

    #[test]
    fn parse_workflow_graph_strips_json_fence() {
        let raw = "```json\n{\"id\":\"wf_x\",\"origin_key\":\"o\",\"name\":\"n\",\"steps\":[],\"data_flows\":[],\"auth_requirements\":{},\"confidence\":{\"workflow_level\":\"draft\",\"step_confidences\":{}},\"compiled_from_sequence_ids\":[],\"last_compiled_at_ms\":0,\"replay_stats\":{}}\n```";
        let wf = parse_workflow_graph(raw).unwrap();
        assert_eq!(wf.id, "wf_x");
    }

    #[test]
    fn workflow_compilation_prompt_contains_shape_but_no_captured_values() {
        let sequence = CapabilitySequence {
            id: "seq_secret_boundary".to_string(),
            task_id: "task-secret".to_string(),
            execution_id: "execution-secret".to_string(),
            origin_key: "api.example.com".to_string(),
            steps: vec![SequenceStep {
                step_index: 0,
                capability_id: Some("cap_fetch_account".to_string()),
                origin: "https://user:password@api.example.com".to_string(),
                concrete_url:
                    "https://api.example.com/private/raw-path-secret?access_token=raw-query-secret"
                        .to_string(),
                method: "POST".to_string(),
                request_params: HashMap::from([(
                    "account_id".to_string(),
                    "raw-param-secret".to_string(),
                )]),
                request_body: Some(
                    r#"{"account_id":"raw-body-secret","nested":{"enabled":true}}"#.to_string(),
                ),
                response_status: Some(200),
                response_body: Some(
                    r#"{"session":{"access_token":"raw-response-secret"}}"#.to_string(),
                ),
                action_binding_id: Some("binding-1".to_string()),
                browser_action_desc: Some("fill raw-description-secret".to_string()),
                browser_action: Some("fill".to_string()),
                browser_arguments: Some(serde_json::json!({
                    "selector": "#password",
                    "value": "raw-browser-secret"
                })),
                executed_via: ExecutionPath::Browser,
                timestamp_ms: 1,
                duration_ms: 2,
            }],
            captured_at_ms: 3,
            finalized: true,
        };
        let input = CompilationInput {
            origin_key: "api.example.com",
            sequences: std::slice::from_ref(&sequence),
            auto_data_flows: &[],
        };

        let prompt = serialize_input(&input);

        for secret in [
            "task-secret",
            "execution-secret",
            "password@",
            "raw-path-secret",
            "raw-query-secret",
            "raw-param-secret",
            "raw-body-secret",
            "raw-response-secret",
            "raw-description-secret",
            "raw-browser-secret",
        ] {
            assert!(!prompt.contains(secret), "workflow prompt leaked {secret}");
        }
        assert!(prompt.contains("account_id"));
        assert!(prompt.contains("access_token"));
        assert!(prompt.contains("<string>"));
        assert!(prompt.contains("cap_fetch_account"));
        assert!(prompt.contains("https://api.example.com"));
    }
}
