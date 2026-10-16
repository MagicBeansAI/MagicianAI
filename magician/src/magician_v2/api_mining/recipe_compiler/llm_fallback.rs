//! Optional shape-only LLM refinement for unresolved recipe structure.

use crate::magician_v2::api_mining::recipe::*;
use crate::magician_v2::api_mining::workflow::InferenceMethod;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};
use magician_core::prompts::PromptManager;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_REFINEMENT_SOURCE_BODY_BYTES: usize = 1024 * 1024;

fn validate_derived_flows_against_capture(
    refined: &TaskRecipe,
    input: &super::RecipeCompileInput,
    evidence: &super::RecipeCompileEvidence,
) -> Result<(), String> {
    let Some(refined_version) = refined.current() else {
        return Err("recipe has no current version".into());
    };
    let derived: Vec<_> = refined_version
        .data_flows
        .iter()
        .filter(|flow| flow.inference == InferenceMethod::LlmInferred)
        .collect();
    if derived.is_empty() {
        return Ok(());
    }
    let mut traces: Vec<_> = input
        .traces
        .iter()
        .filter(|trace| super::usable(trace))
        .cloned()
        .collect();
    traces.sort_by_key(|trace| trace.timestamp);
    let task_context = format!("{} {}", input.task_title, input.task_text);
    let context = super::resolve::ResolveContext {
        traces: &traces,
        typed_inputs: &input.typed_inputs,
        task_text: &task_context,
    };

    for flow in derived {
        let source_index = *evidence
            .trace_index_by_step
            .get(&flow.source_step)
            .ok_or_else(|| format!("LLM flow {} has no captured source step", flow.id))?;
        let target_index = *evidence
            .trace_index_by_step
            .get(&flow.target_step)
            .ok_or_else(|| format!("LLM flow {} has no captured target step", flow.id))?;
        if target_index >= traces.len() {
            return Err(format!(
                "LLM flow {} target trace is out of bounds",
                flow.id
            ));
        }
        let target_request = super::resolve::resolve_request(&context, target_index);
        let expected = target_request
            .params
            .iter()
            .find(|(token, _)| token.name == flow.target_param)
            .map(|(token, _)| token.value.as_str())
            .filter(|value| !value.is_empty() && *value != "[REDACTED]")
            .ok_or_else(|| {
                format!(
                    "LLM flow {} target value is absent or redacted in the capture",
                    flow.id
                )
            })?;
        let Extractor::JsonPath { path } = &flow.extractor else {
            return Err(format!("LLM flow {} is not JSONPath-backed", flow.id));
        };
        let source_body = traces
            .get(source_index)
            .ok_or_else(|| format!("LLM flow {} source trace is out of bounds", flow.id))?
            .response_body
            .as_deref()
            .ok_or_else(|| format!("LLM flow {} source body was not captured", flow.id))?;
        if source_body.len() > MAX_REFINEMENT_SOURCE_BODY_BYTES {
            return Err(format!(
                "LLM flow {} source response exceeds the refinement proof limit",
                flow.id
            ));
        }
        let source_json: serde_json::Value = serde_json::from_str(source_body)
            .map_err(|_| format!("LLM flow {} source response was not JSON", flow.id))?;
        let observed = crate::magician_v2::api_mining::workflow_replay::jsonpath::extract_jsonpath(
            &source_json,
            path,
        )
        .ok_or_else(|| format!("LLM flow {} JSONPath missed in the capture", flow.id))?;
        if observed != expected {
            return Err(format!(
                "LLM flow {} did not reproduce its captured target value",
                flow.id
            ));
        }
    }
    Ok(())
}

pub fn needs_llm_refinement(recipe: &TaskRecipe) -> bool {
    let Some(version) = recipe.current() else {
        return false;
    };
    let volatile = version.steps.iter().any(|step| {
        step.param_sources
            .values()
            .any(|source| matches!(source, RecipeParamSource::Literal { volatile: true, .. }))
    });
    // A shape-only model cannot safely invent a new task input because it is
    // intentionally never shown the captured parameter value needed to bind
    // that input. LLM refinement is therefore limited to deriving unresolved
    // volatile values from earlier response shapes.
    volatile
}

pub fn serialize_shape(recipe: &TaskRecipe, _task_title: &str) -> String {
    let Some(version) = recipe.current() else {
        return "{}".into();
    };
    let steps: Vec<_> = version
        .steps
        .iter()
        .map(|step| {
            let host = url::Url::parse(&step.origin)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default();
            let mut param_names: Vec<_> = step
                .param_sources
                .iter()
                .map(|(name, source)| {
                    let kind = match source {
                        RecipeParamSource::Literal { volatile: true, .. } => "unresolved_volatile",
                        RecipeParamSource::Literal { .. } => "literal",
                        RecipeParamSource::DataFlow { .. } => "data_flow",
                        RecipeParamSource::TaskInput { .. } => "task_input",
                        RecipeParamSource::SessionAuth { .. } => "session_auth",
                        RecipeParamSource::Now { .. } => "now",
                    };
                    serde_json::json!({"name": name, "kind": kind})
                })
                .collect();
            param_names.sort_by(|left, right| {
                left["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["name"].as_str().unwrap_or_default())
            });
            serde_json::json!({
                "id": step.id,
                "method": step.method,
                "host": host,
                "param_names": param_names,
                "side_effects": step.side_effects,
            })
        })
        .collect();
    let unresolved: Vec<_> = version
        .steps
        .iter()
        .flat_map(|step| {
            step.param_sources
                .iter()
                .filter(|(_, source)| {
                    matches!(source, RecipeParamSource::Literal { volatile: true, .. })
                })
                .map(move |(name, _)| serde_json::json!({"step": step.id, "param": name}))
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "inputs": recipe.shape.inputs.iter().map(|input| serde_json::json!({"name": input.name, "schema": input.schema})).collect::<Vec<_>>(),
        "steps": steps,
        "unresolved_volatile": unresolved,
    }))
    .unwrap_or_else(|_| "{}".into())
}

pub fn apply_refinement(
    recipe: &mut TaskRecipe,
    refinement: &serde_json::Value,
) -> Result<(), String> {
    // The model never receives title/template literals. A legacy/reflexive
    // response may echo the already-bound template, but it cannot rewrite it.
    let template = refinement
        .get("template")
        .map(|value| value.as_str().ok_or("template must be a string"))
        .transpose()?
        .map(|value| value.trim().to_lowercase())
        .unwrap_or_else(|| recipe.shape.template.clone());
    if template.is_empty()
        || template.len() > 512
        || super::shape::template_regex(&template).is_none()
    {
        return Err("invalid template".into());
    }
    if template != recipe.shape.template {
        return Err("shape-only refinement cannot rewrite the task template".into());
    }

    let existing_inputs: HashMap<_, _> = recipe
        .shape
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.clone()))
        .collect();
    let expected_input_names: HashSet<_> = existing_inputs.keys().cloned().collect();
    let template_input_names = template_input_names(&template)?;
    if !template_input_names.is_subset(&expected_input_names) {
        return Err("refined template contains an unbound input placeholder".into());
    }

    let version = recipe.current().ok_or("recipe has no current version")?;
    let positions: HashMap<_, _> = version
        .steps
        .iter()
        .enumerate()
        .map(|(position, step)| (step.id.clone(), position))
        .collect();
    let volatile_targets: HashSet<_> = version
        .steps
        .iter()
        .flat_map(|step| {
            step.param_sources
                .iter()
                .filter(|(_, source)| {
                    matches!(source, RecipeParamSource::Literal { volatile: true, .. })
                })
                .map(move |(name, _)| (step.id.clone(), name.clone()))
        })
        .collect();

    let mut new_flows = Vec::new();
    let flows = refinement
        .get("derived_flows")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if flows.len() > volatile_targets.len() {
        return Err("too many derived flows for unresolved targets".into());
    }
    let mut claimed_targets = HashSet::new();
    for flow in flows {
        let source = flow
            .get("source_step")
            .and_then(serde_json::Value::as_str)
            .ok_or("derived flow without source_step")?;
        let target = flow
            .get("target_step")
            .and_then(serde_json::Value::as_str)
            .ok_or("derived flow without target_step")?;
        let param = flow
            .get("target_param")
            .and_then(serde_json::Value::as_str)
            .ok_or("derived flow without target_param")?;
        let (Some(source_position), Some(target_position)) =
            (positions.get(source), positions.get(target))
        else {
            return Err(format!(
                "derived flow references unknown step {source}->{target}"
            ));
        };
        if source_position >= target_position {
            return Err(format!(
                "derived flow is not backward-safe: {source}->{target}"
            ));
        }
        if !volatile_targets.contains(&(target.to_owned(), param.to_owned())) {
            return Err(format!(
                "derived flow target is not an unresolved volatile parameter: {target}.{param}"
            ));
        }
        if param.starts_with("opaque_body") {
            return Err("an opaque request body cannot be derived by the shape-only model".into());
        }
        if !claimed_targets.insert((target.to_owned(), param.to_owned())) {
            return Err(format!("duplicate derived flow target: {target}.{param}"));
        }
        let extractor: Extractor = serde_json::from_value(
            flow.get("extractor")
                .cloned()
                .ok_or("derived flow without extractor")?,
        )
        .map_err(|error| error.to_string())?;
        let Extractor::JsonPath { path } = &extractor else {
            return Err("LLM-derived flows must use a JSONPath extractor".into());
        };
        if path.len() > 256
            || !crate::magician_v2::api_mining::workflow_replay::jsonpath::is_supported_jsonpath(
                path,
            )
        {
            return Err("LLM-derived JSONPath is invalid or too long".into());
        }
        let flow_id = format!("df_{}", ulid::Ulid::new());
        new_flows.push(RecipeDataFlow {
            id: flow_id,
            source_step: source.to_owned(),
            extractor,
            target_step: target.to_owned(),
            target_param: param.to_owned(),
            confidence: 0.6,
            inference: InferenceMethod::LlmInferred,
        });
    }

    let input_specs = refinement
        .get("inputs")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if input_specs.len() > existing_inputs.len() {
        return Err("refinement introduced an unbound task input".into());
    }
    let mut refined_inputs = Vec::with_capacity(existing_inputs.len());
    let mut refined_names = HashSet::new();
    for input in input_specs {
        let name = input
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or("input without name")?;
        if name.is_empty()
            || !name.chars().enumerate().all(|(index, character)| {
                character.is_ascii_lowercase()
                    || index > 0 && (character == '_' || character.is_ascii_digit())
            })
        {
            return Err(format!("input name is not snake_case: {name}"));
        }
        if !refined_names.insert(name.to_owned()) {
            return Err(format!("duplicate refined input: {name}"));
        }
        let schema = match input
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("string")
        {
            "string" => TaskInputSchema::String,
            "number" => TaskInputSchema::Number,
            "boolean" => TaskInputSchema::Boolean,
            other => return Err(format!("unsupported input schema {other}")),
        };
        let Some(existing) = existing_inputs.get(name) else {
            return Err(format!("refinement introduced unbound input: {name}"));
        };
        if !value_matches_schema(&existing.example_value, schema) {
            return Err(format!(
                "refined schema does not accept the observed example for input {name}"
            ));
        }
        refined_inputs.push(TaskInput {
            name: name.to_owned(),
            schema,
            example_value: existing.example_value.clone(),
            source: existing.source,
        });
    }

    if !refined_inputs.is_empty() && refined_names != expected_input_names {
        return Err("refinement omitted one or more bound task inputs".into());
    }

    recipe.shape.template = template;
    if !refined_inputs.is_empty() {
        recipe.shape.inputs = refined_inputs;
    }
    if let Some(version) = recipe.current_mut() {
        for flow in &new_flows {
            if let Some(step) = version
                .steps
                .iter_mut()
                .find(|step| step.id == flow.target_step)
            {
                step.param_sources.insert(
                    flow.target_param.clone(),
                    RecipeParamSource::DataFlow {
                        flow_id: flow.id.clone(),
                    },
                );
            }
        }
        version.data_flows.extend(new_flows);
    }
    recipe.shape.fingerprint = super::shape::contextual_shape_fingerprint(
        &recipe.shape.template,
        recipe.shape.description_template.as_deref(),
        &recipe.agent_id,
        &recipe.scope_principal,
        &recipe.scope_workspace,
    );
    Ok(())
}

fn template_input_names(template: &str) -> Result<HashSet<String>, String> {
    let mut names = HashSet::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start + 1..]
            .find('}')
            .map(|offset| start + 1 + offset)
            .ok_or("template has an unclosed placeholder")?;
        names.insert(rest[start + 1..end].to_owned());
        rest = &rest[end + 1..];
    }
    Ok(names)
}

fn value_matches_schema(value: &str, schema: TaskInputSchema) -> bool {
    schema.accepts(value)
}

pub async fn refine_with_llm(
    router: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    recipe: &mut TaskRecipe,
    task_title: &str,
    compile_input: &super::RecipeCompileInput,
    compile_evidence: &super::RecipeCompileEvidence,
) -> Result<bool, String> {
    if !needs_llm_refinement(recipe) {
        return Ok(false);
    }
    let prompt = prompt_manager
        .get_prompt(
            crate::magician_v2::prompts::names::RECIPE_COMPILE_SYSTEM,
            crate::magician_v2::prompts::versions::RECIPE_COMPILE_SYSTEM,
        )
        .await
        .map_err(|error| error.to_string())?;
    let system = prompt
        .render(&HashMap::new())
        .map_err(|error| error.to_string())?;
    let raw = router
        .generate_for_operation_with_system(
            &LLMOperation::RecipeCompilation,
            Some(&system),
            &serialize_shape(recipe, task_title),
        )
        .await
        .map_err(|error| error.to_string())?;
    let cleaned = raw
        .content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let refinement = serde_json::from_str(cleaned)
        .map_err(|error| format!("recipe refinement was not JSON: {error}"))?;
    // Refine a clone first so any rejected provider output is atomic.
    let mut refined = recipe.clone();
    apply_refinement(&mut refined, &refinement)?;
    validate_derived_flows_against_capture(&refined, compile_input, compile_evidence)?;
    *recipe = refined;
    Ok(true)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::SideEffects;
    use crate::magician_v2::api_mining::recipe_compiler::values::{ReportedValue, ReportedValues};
    use crate::magician_v2::api_mining::types::{
        NetworkTraceEvent, RequestInitiator, RequestTiming,
    };
    use crate::magician_v2::api_mining::workflow::ReplayStats;

    fn trace(id: &str, timestamp: i64, url: &str, response_body: &str) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: id.into(),
            timestamp,
            method: "GET".into(),
            url: url.into(),
            resource_type: Some("Fetch".into()),
            frame_id: None,
            request_headers: HashMap::new(),
            request_body: None,
            tab_id: None,
            thread_id: None,
            status: 200,
            response_headers: HashMap::new(),
            response_body: Some(response_body.into()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 1.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".into(),
                stack: None,
                url: None,
            },
            request_size: 0,
            response_size: response_body.len() as u64,
            capture_source: Some("fixture".into()),
        }
    }

    fn unresolved_recipe() -> TaskRecipe {
        let origin = "https://fixture.example";
        let step = |id: &str, param_sources| RecipeStep {
            id: id.into(),
            origin: origin.into(),
            method: "GET".into(),
            url_template: format!("{origin}/api/{{query}}"),
            headers_template: HashMap::new(),
            body_template: None,
            capability_id: None,
            param_sources,
            body_param_types: HashMap::new(),
            side_effects: SideEffects::ReadOnly,
            request_shape_fingerprint: format!("shape_{id}"),
            verify_with: None,
            browser_fallback: None,
            transport_hint: None,
        };
        TaskRecipe {
            id: "recipe_llm_boundary".into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "search {query}".into(),
                fingerprint: "shape".into(),
                inputs: vec![TaskInput {
                    name: "query".into(),
                    schema: TaskInputSchema::String,
                    example_value: "private-search".into(),
                    source: TaskInputSource::TaskText,
                }],
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec![origin.into()],
                steps: vec![
                    step(
                        "s0",
                        HashMap::from([(
                            "query".into(),
                            RecipeParamSource::TaskInput {
                                name: "query".into(),
                            },
                        )]),
                    ),
                    step(
                        "s1",
                        HashMap::from([
                            (
                                "query".into(),
                                RecipeParamSource::TaskInput {
                                    name: "query".into(),
                                },
                            ),
                            (
                                "cursor".into(),
                                RecipeParamSource::Literal {
                                    value: String::new(),
                                    volatile: true,
                                },
                            ),
                        ]),
                    ),
                ],
                data_flows: Vec::new(),
                answer_spec: Vec::new(),
                auth: RecipeAuth::default(),
                maturity: RecipeMaturity::Draft,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task".into(),
                    execution_id: "execution".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 0,
                last_replayed_at_ms: None,
            }],
        }
    }

    #[test]
    fn shape_payload_omits_examples_and_volatile_values() {
        let recipe = unresolved_recipe();
        let payload = serialize_shape(&recipe, "Search private-search");
        assert!(!payload.contains("private-search"));
        assert!(payload.contains("unresolved_volatile"));
    }

    #[test]
    fn refinement_cannot_declare_a_non_renderable_numeric_input() {
        for value in ["+42", "01", "1.", ".5"] {
            let mut recipe = unresolved_recipe();
            recipe.shape.inputs[0].example_value = value.into();
            let error = apply_refinement(
                &mut recipe,
                &serde_json::json!({
                    "inputs": [{"name": "query", "schema": "number"}]
                }),
            )
            .unwrap_err();
            assert!(error.contains("does not accept the observed example"));
            assert_eq!(recipe.shape.inputs[0].schema, TaskInputSchema::String);
        }
        let mut recipe = unresolved_recipe();
        recipe.shape.inputs[0].example_value = "1e+3".into();
        apply_refinement(
            &mut recipe,
            &serde_json::json!({
                "inputs": [{"name": "query", "schema": "number"}]
            }),
        )
        .unwrap();
        assert_eq!(recipe.shape.inputs[0].schema, TaskInputSchema::Number);
    }

    #[test]
    fn valid_flow_refinement_preserves_bound_inputs() {
        let mut recipe = unresolved_recipe();
        apply_refinement(
            &mut recipe,
            &serde_json::json!({
                "template": "search {query}",
                "inputs": [{"name": "query", "schema": "string"}],
                "derived_flows": [{
                    "source_step": "s0",
                    "extractor": {"kind": "json_path", "path": "$.cursor"},
                    "target_step": "s1",
                    "target_param": "cursor"
                }]
            }),
        )
        .unwrap();
        assert_eq!(recipe.shape.inputs.len(), 1);
        assert!(matches!(
            recipe.current().unwrap().steps[1]
                .param_sources
                .get("cursor"),
            Some(RecipeParamSource::DataFlow { .. })
        ));
    }

    #[test]
    fn llm_flow_must_reproduce_the_captured_target_value() {
        let signature = "signatureValue123456789";
        let traces = vec![
            trace(
                "source",
                1,
                "https://fixture.example/api/bootstrap",
                &format!(r#"{{"id":"item-123","signature":"{signature}"}}"#),
            ),
            trace(
                "target",
                2,
                &format!("https://fixture.example/api/result?id=item-123&sig={signature}"),
                r#"{"answer":"done"}"#,
            ),
        ];
        let input = super::super::RecipeCompileInput {
            task_id: "task".into(),
            execution_id: "execution".into(),
            monitor_revision: None,
            agent_id: "assistant".into(),
            principal: "owner".into(),
            workspace: "default".into(),
            task_title: "fetch result".into(),
            task_text: String::new(),
            reported: ReportedValues {
                values: vec![ReportedValue {
                    field: Some("answer".into()),
                    raw: "done".into(),
                    normalized: "done".into(),
                }],
            },
            traces,
            typed_inputs: Vec::new(),
            trace_files: Vec::new(),
            sequences: Vec::new(),
        };
        let (recipe, evidence) = super::super::compile_task_recipe_with_evidence(&input).unwrap();
        let refinement = |path: &str| {
            serde_json::json!({
                "derived_flows": [{
                    "source_step": "s0",
                    "extractor": {"kind": "json_path", "path": path},
                    "target_step": "s1",
                    "target_param": "sig"
                }]
            })
        };

        let mut proven = recipe.clone();
        apply_refinement(&mut proven, &refinement("$.signature")).unwrap();
        assert!(validate_derived_flows_against_capture(&proven, &input, &evidence).is_ok());

        let mut wrong = recipe;
        apply_refinement(&mut wrong, &refinement("$.id")).unwrap();
        assert!(validate_derived_flows_against_capture(&wrong, &input, &evidence).is_err());
    }

    #[test]
    fn refinement_accepts_an_input_bound_from_task_description() {
        let mut recipe = unresolved_recipe();
        recipe.shape.template = "search records".into();
        apply_refinement(
            &mut recipe,
            &serde_json::json!({
                "template": "search records",
                "inputs": [{"name": "query", "schema": "string"}],
                "derived_flows": [{
                    "source_step": "s0",
                    "extractor": {"kind": "json_path", "path": "$.cursor"},
                    "target_step": "s1",
                    "target_param": "cursor"
                }]
            }),
        )
        .unwrap();

        assert_eq!(recipe.shape.inputs[0].name, "query");
        assert!(matches!(
            recipe.current().unwrap().steps[1]
                .param_sources
                .get("cursor"),
            Some(RecipeParamSource::DataFlow { .. })
        ));
    }

    #[test]
    fn refinement_cannot_add_or_drop_request_bound_inputs() {
        let mut recipe = unresolved_recipe();
        let added = serde_json::json!({
            "template": "search {query} on {site}",
            "inputs": [
                {"name": "query", "schema": "string"},
                {"name": "site", "schema": "string"}
            ],
            "derived_flows": []
        });
        assert!(apply_refinement(&mut recipe, &added).is_err());
        let dropped = serde_json::json!({
            "template": "search",
            "inputs": [],
            "derived_flows": []
        });
        assert!(apply_refinement(&mut recipe, &dropped).is_err());
        assert_eq!(recipe.shape.template, "search {query}");
    }
}
