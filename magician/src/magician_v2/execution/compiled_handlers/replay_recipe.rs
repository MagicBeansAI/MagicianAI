//! `replay_recipe` — run a learned task-shaped API DAG without a browser.

use super::shared::require_scope_str;
use crate::magician_v2::api_mining::approval::{
    apply_decision, build_api_replay_approval_request, decision_is_approval, ApprovalSubject,
    OPTION_APPROVE_ALWAYS_STEP,
};
use crate::magician_v2::api_mining::capability::SideEffects;
use crate::magician_v2::api_mining::origin_policy::OriginPolicyStore;
use crate::magician_v2::api_mining::recipe_runner::{
    RecipeRunInputs, RecipeRunner, ReqwestTransport,
};
use crate::magician_v2::api_mining::recipe_store::RecipeStore;
use crate::magician_v2::api_mining::replay_grants::{
    is_denylisted_url_template, GrantKey, ReplayGrantStore,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn validate_recipe_pack_binding(
    invoked_pack: &str,
    expected_recipe_pack: Option<&str>,
) -> Result<(), String> {
    if invoked_pack == crate::magician_v2::api_mining::recipe_packs::PROVIDER_NAME {
        return expected_recipe_pack
            .map(|_| ())
            .ok_or_else(|| "draft recipes are not available as planner tools".into());
    }
    match expected_recipe_pack {
        Some(expected) if expected == invoked_pack => Ok(()),
        Some(_) => Err("recipe id is not bound to the invoked recipe tool".into()),
        None => Err("draft recipes are not available as generated tools".into()),
    }
}

pub fn validate_args(args: &Value) -> Result<(String, HashMap<String, String>), String> {
    let recipe_id = args
        .get("recipe_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("recipe_id is required")?
        .to_owned();
    let inputs = args
        .get("inputs")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((recipe_id, inputs))
}

fn merge_top_level_inputs(
    inputs: &mut HashMap<String, String>,
    declared: &[crate::magician_v2::api_mining::recipe::TaskInput],
    args: &Value,
) {
    for input in declared {
        if inputs.contains_key(&input.name)
            || crate::magician_v2::api_mining::recipe_packs::is_reserved_input_name(&input.name)
        {
            continue;
        }
        if let Some(value) = args.get(&input.name) {
            inputs.insert(
                input.name.clone(),
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
    }
}

fn api_mining_effective_for_scope(
    resources: &AgentResources,
    principal: &str,
    workspace: &str,
) -> bool {
    let mining_base = resources
        .artifact_workspace
        .api_mining_root(principal, workspace);
    // The owner-level toggle writes the live runtime YAML. Compiled handlers
    // may outlive the AgentResources config snapshot, so the execution boundary
    // reads that tiny flag from the authoritative file just like scoped tool
    // discovery does.
    let process_enabled = crate::magician_v2::api_mining::switch::runtime_api_mining_config()
        .map(|config| config.enabled)
        .unwrap_or(false);
    crate::magician_v2::api_mining::switch::ApiMiningSwitch::effective_from_disk(
        process_enabled,
        &mining_base,
    )
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "replay_recipe")?;
    let workspace = require_scope_str(&args, "__workspace", "replay_recipe")?;
    let invoked_pack = require_scope_str(&args, "__compiled_pack_name", "replay_recipe")?;
    let (recipe_id, mut inputs) = match validate_args(&args) {
        Ok(values) => values,
        Err(reason) => return Ok(json!({"status": "error", "reason": reason})),
    };
    let mining_base = resources
        .artifact_workspace
        .api_mining_root(&principal, &workspace);
    if !api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace) {
        return Ok(json!({"status": "disabled", "reason": "api_mining_disabled"}));
    }
    let runtime_config = crate::magician_v2::api_mining::switch::runtime_api_mining_config();
    let recipe_reqwest_enabled = runtime_config.as_ref().is_some_and(|config| {
        config.recipes.enabled
            && config
                .recipes
                .transport_ladder
                .iter()
                .any(|transport| transport.eq_ignore_ascii_case("reqwest"))
    });
    if !recipe_reqwest_enabled {
        return Ok(json!({
            "status": "disabled",
            "reason": "recipe_reqwest_transport_disabled",
        }));
    }
    let store = RecipeStore::new(mining_base.clone());
    let replay_lock = match store.replay_lock(&recipe_id) {
        Ok(lock) => lock,
        Err(error) => {
            return Ok(json!({"status": "error", "reason": error.to_string()}));
        },
    };
    let _replay_guard = replay_lock.lock().await;
    if !api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace) {
        return Ok(json!({"status": "disabled", "reason": "api_mining_disabled"}));
    }
    let mut recipe = match store.load_for_scope(&recipe_id, &principal, &workspace) {
        Ok(Some(recipe)) => recipe,
        Ok(None) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("unknown recipe {recipe_id}"),
            }));
        },
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("could not load recipe {recipe_id}: {error}"),
            }));
        },
    };
    let expected_recipe_pack =
        crate::magician_v2::api_mining::recipe_packs::recipe_to_pack_definition(&recipe)
            .map(|definition| definition.name);
    if let Err(reason) =
        validate_recipe_pack_binding(&invoked_pack, expected_recipe_pack.as_deref())
    {
        return Ok(json!({"status": "error", "reason": reason}));
    }
    merge_top_level_inputs(&mut inputs, &recipe.shape.inputs, &args);
    let grants = ReplayGrantStore::open(&mining_base);
    let policy = OriginPolicyStore::open(&mining_base);
    let metrics = crate::magician_v2::api_mining::recipe_metrics_for_scope(&principal, &workspace);
    let Some(secret_store) = resources
        .secret_store_resolver
        .as_ref()
        .and_then(|resolver| resolver.resolve_for_scope(&principal, &workspace).ok())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "scoped secret store is unavailable",
        }));
    };
    let mut run_inputs = RecipeRunInputs {
        inputs,
        timeout_ms: Some(15_000),
        approved_write_steps: HashSet::new(),
    };
    let recipe_preflight_ok =
        crate::magician_v2::api_mining::recipe_runner::validate_recipe_preflight(
            &recipe,
            &run_inputs,
        )
        .is_ok();

    // Approve every write before the first request. This makes a multi-write
    // recipe atomic with respect to user intent: it cannot pause after an
    // earlier mutation and duplicate that mutation on a rerun.
    let mut approved_write_steps = HashSet::new();
    let replay_started_at = std::time::Instant::now();
    let mut approval_request_ids = Vec::new();
    let task_id = args
        .get("__task_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("recipe_{}", recipe.id));
    let execution_id = args
        .get("__execution_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("exec_{}", ulid::Ulid::new()));
    if let Some(version) = recipe.current().filter(|_| recipe_preflight_ok) {
        for step in version
            .steps
            .iter()
            .filter(|step| step.side_effects == SideEffects::Write)
        {
            let key = GrantKey {
                recipe_id: Some(recipe.id.clone()),
                step_id: Some(step.id.clone()),
                capability_id: step.capability_id.clone(),
                request_shape_fingerprint: step.effective_request_shape_fingerprint(),
            };
            let always_ask = is_denylisted_url_template(&step.url_template)
                || !crate::magician_v2::api_mining::recipe::request_body_shape_is_grantable(
                    step.body_template.as_deref(),
                );
            if !always_ask && grants.lookup(&key).is_some() {
                metrics.record_grant_used();
                continue;
            }
            let subject = ApprovalSubject {
                grant_key: key,
                origin: step.origin.clone(),
                method: step.method.clone(),
                url_template: step.url_template.clone(),
                side_effects: step.side_effects.clone(),
                request_preview: json!({
                    "recipe_id": recipe.id,
                    "step_id": step.id,
                    "method": step.method,
                    "url_template": crate::magician_v2::api_mining::approval::approval_url_shape(
                        &step.url_template,
                    ),
                    "body_shape": crate::magician_v2::api_mining::recipe::request_body_shape(
                        step.body_template.as_deref(),
                    ),
                }),
                durable_grant_allowed:
                    crate::magician_v2::api_mining::recipe::request_body_shape_is_grantable(
                        step.body_template.as_deref(),
                    ),
                policy_reason: if always_ask {
                    "denylist_floor_always_asks".into()
                } else {
                    "write_replay_requires_grant".into()
                },
                owner_agent_id: Some(recipe.agent_id.clone()),
            };
            let Some(request_service) = resources.user_request_service.as_ref() else {
                if !api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace) {
                    return Ok(json!({"status": "disabled", "reason": "api_mining_disabled"}));
                }
                let record = crate::magician_v2::api_mining::recipe_runs::RecipeRunRecord::denied(
                    &recipe,
                    task_id.clone(),
                    execution_id.clone(),
                    step.id.clone(),
                    replay_started_at.elapsed().as_millis() as u64,
                    approval_request_ids,
                );
                if let Err(error) =
                    crate::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(&mining_base)
                        .append(&record)
                        .await
                {
                    tracing::warn!(recipe_id = %recipe.id, %error, "compiled replay ledger append failed");
                }
                return Ok(json!({
                    "status": "needs_approval",
                    "recipe_id": recipe_id,
                    "step_id": step.id,
                    "method": step.method,
                    "url_template": crate::magician_v2::api_mining::approval::approval_url_shape(
                        &step.url_template,
                    ),
                    "grant_key": subject.grant_key,
                }));
            };
            let request = build_api_replay_approval_request(
                &subject,
                &principal,
                &workspace,
                args.get("__execution_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                args.get("__task_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            let response = request_service.ask(request).await;
            if !api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace) {
                if !decision_is_approval(&response.decision) {
                    return Ok(json!({
                        "status": "denied",
                        "recipe_id": recipe_id,
                        "step_id": step.id,
                    }));
                }
                return Ok(json!({"status": "disabled", "reason": "api_mining_disabled"}));
            }
            approval_request_ids.push(response.request_id.clone());
            let durable_grant_requested = response.decision == OPTION_APPROVE_ALWAYS_STEP;
            let approved =
                apply_decision(&grants, &subject, &response.decision, &response.request_id)
                    .unwrap_or(false);
            if !approved {
                metrics.record_approval_denied();
                let record = crate::magician_v2::api_mining::recipe_runs::RecipeRunRecord::denied(
                    &recipe,
                    task_id.clone(),
                    execution_id.clone(),
                    step.id.clone(),
                    replay_started_at.elapsed().as_millis() as u64,
                    approval_request_ids,
                );
                if let Err(error) =
                    crate::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(&mining_base)
                        .append(&record)
                        .await
                {
                    tracing::warn!(recipe_id = %recipe.id, %error, "compiled replay ledger append failed");
                }
                return Ok(json!({
                    "status": "denied",
                    "recipe_id": recipe_id,
                    "step_id": step.id,
                }));
            }
            if durable_grant_requested {
                metrics.record_grant_created();
            }
            if crate::magician_v2::api_mining::approval::decision_is_one_run_approval(
                &response.decision,
            ) {
                approved_write_steps.insert(step.id.clone());
            }
        }
    }
    run_inputs.approved_write_steps = approved_write_steps;

    let session_lookup = |origin: &str, url: &str| {
        secret_store
            .get_session(origin, url)
            .map(|(session, _lease)| session)
    };
    let feedback_sink =
        crate::magician_v2::api_mining::recipe_feedback::RecipeFeedbackSink::for_scope(
            &resources.artifact_workspace,
            &principal,
            &workspace,
        )
        .map_err(|error| {
            tracing::warn!(%error, "recipe feedback sink unavailable for compiled replay");
            error
        })
        .ok();
    let step_feedback = |origin: &str,
                         capability_id: &str,
                         url_template: &str,
                         success: bool,
                         auth_stale: bool,
                         status: u16,
                         response_body: &str| {
        if api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace) {
            if let Some(sink) = &feedback_sink {
                sink.record(
                    origin,
                    capability_id,
                    url_template,
                    success,
                    auth_stale,
                    status,
                    response_body,
                );
            }
        }
    };
    let can_continue =
        || api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace);
    let runner = RecipeRunner {
        can_continue: Some(&can_continue),
        transports: vec![Box::new(ReqwestTransport::default())],
        grants: &grants,
        origin_policy: &policy,
        session_lookup: &session_lookup,
        auth_healer: None,
        max_auth_heals: 0,
        step_feedback: Some(&step_feedback),
        observer: None,
    };
    metrics.record_replay_started();
    let result = runner.run(&mut recipe, &run_inputs).await;
    let failed_write = result.write_outcome_uncertain(&recipe);
    if !api_mining_effective_for_scope(resources.as_ref(), &principal, &workspace) {
        return Ok(replay_tool_response(
            &recipe_id, &result, failed_write,
            Some("API mining was disabled during replay; execution outcome is preserved, but no replay state was persisted"),
        ));
    }
    for _ in 0..result.auth_heals {
        metrics.record_auth_heal();
    }
    if result.success {
        metrics.record_replay_succeeded();
    } else {
        metrics.record_replay_failed(
            result
                .failure
                .as_ref()
                .map(|failure| failure.class)
                .or_else(|| result.fallback.as_ref().map(|fallback| fallback.class)),
        );
        if result.fallback.is_some() {
            metrics.record_fallback_handoff();
        }
    }
    match crate::magician_v2::api_mining::recipe_runs::persist_capability_sequence(
        &mining_base,
        &recipe,
        &result,
        &task_id,
        &execution_id,
    ) {
        Ok(Some(_)) => {
            let sequence_metrics =
                crate::magician_v2::api_mining::sequence_metrics_for_scope(&principal, &workspace);
            sequence_metrics.record_started();
            sequence_metrics.record_finalized();
        },
        Ok(None) => {},
        Err(error) => tracing::warn!(
            recipe_id = %recipe.id,
            %error,
            "compiled replay sequence persistence failed"
        ),
    }
    let record = crate::magician_v2::api_mining::recipe_runs::RecipeRunRecord::from_result(
        &recipe,
        &result,
        task_id,
        execution_id,
        if result.success || failed_write {
            "api"
        } else {
            "api_then_browser"
        },
        replay_started_at.elapsed().as_millis() as u64,
        approval_request_ids,
    );
    if let Err(error) =
        crate::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(&mining_base)
            .append(&record)
            .await
    {
        tracing::warn!(recipe_id = %recipe.id, %error, "compiled replay ledger append failed");
    }
    if let Err(error) = store.save(&recipe) {
        tracing::warn!(recipe_id = %recipe.id, %error, "replay completed but statistics persistence failed");
        return Ok(replay_tool_response(
            &recipe_id,
            &result,
            failed_write,
            Some("Recipe statistics could not be saved; do not repeat completed writes"),
        ));
    }
    let pack_store = crate::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
        &resources.artifact_workspace,
        &principal,
        &workspace,
    );
    if let Err(error) = crate::magician_v2::api_mining::recipe_packs::publish_recipe_pack(
        &pack_store,
        &resources
            .artifact_workspace
            .scope_skills_root(&principal, &workspace),
        &recipe,
    ) {
        tracing::warn!(recipe_id = %recipe.id, %error, "recipe pack upsert failed after tool replay");
    }
    Ok(replay_tool_response(
        &recipe_id,
        &result,
        failed_write,
        None,
    ))
}

fn replay_tool_response(
    recipe_id: &str,
    result: &crate::magician_v2::api_mining::recipe_runner::RecipeRunResult,
    failed_write: bool,
    state_warning: Option<&str>,
) -> Value {
    let mut response = if result.success {
        json!({
            "status": "ok",
            "recipe_id": recipe_id,
            "answer": result.answer,
            "steps": result.steps,
            "auth_heals": result.auth_heals,
        })
    } else if failed_write {
        json!({
            "status": "write_outcome_uncertain",
            "recipe_id": recipe_id,
            "failure": result.failure,
            "steps": result.steps,
            "effect_uncertain": true,
            "retryable": false,
            "browser_retry_allowed": false,
        })
    } else if let Some(pending) = &result.pending_approval {
        json!({
            "status": "needs_approval",
            "recipe_id": recipe_id,
            "step_id": pending.step_id,
            "method": pending.method,
            "url_template": crate::magician_v2::api_mining::approval::approval_url_shape(
                &pending.url_template,
            ),
            "grant_key": pending.grant_key,
        })
    } else {
        json!({
            "status": "fallback",
            "recipe_id": recipe_id,
            "fallback": result.fallback,
            "failure": result.failure,
            "steps": result.steps,
        })
    };
    if let Some(warning) = state_warning {
        response["state_persistence_warning"] = json!(warning);
    }
    response
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn persistence_warnings_preserve_success_and_terminal_write_outcomes() {
        use crate::magician_v2::api_mining::recipe_runner::{
            FailureClass, RecipeRunFailure, RecipeRunResult,
        };
        let mut result = RecipeRunResult {
            success: true,
            auth_heals: 0,
            answer: serde_json::Map::new(),
            steps: vec![],
            failure: None,
            fallback: None,
            pending_approval: None,
        };
        result.answer.insert("item".into(), json!("saved"));
        let response = replay_tool_response("r", &result, false, Some("statistics unavailable"));
        assert_eq!(response["status"], "ok");
        assert_eq!(response["answer"]["item"], "saved");
        assert!(response.get("state_persistence_warning").is_some());
        result.success = false;
        result.failure = Some(RecipeRunFailure {
            step_id: "write".into(),
            class: FailureClass::Network,
            detail: "timeout".into(),
        });
        let response = replay_tool_response("r", &result, true, Some("mining disabled"));
        assert_eq!(response["status"], "write_outcome_uncertain");
        assert_eq!(response["retryable"], false);
        assert_eq!(response["browser_retry_allowed"], false);
        assert_eq!(response["failure"]["step_id"], "write");
    }

    #[test]
    fn nested_reserved_input_names_cannot_replace_runtime_controls() {
        let (id, inputs) = validate_args(&json!({
            "recipe_id": "server-recipe", "__principal": "server-principal",
            "inputs": {"recipe_id": "task-recipe", "inputs": "task-input", "__principal": "task-principal"},
        })).unwrap();
        assert_eq!(id, "server-recipe");
        assert_eq!(inputs["recipe_id"], "task-recipe");
        assert_eq!(inputs["inputs"], "task-input");
        assert_eq!(inputs["__principal"], "task-principal");
        assert!(validate_recipe_pack_binding("recipe__task_v1", Some("recipe__task_v2")).is_err());
    }

    #[test]
    fn missing_nested_inputs_cannot_borrow_runtime_control_values() {
        use crate::magician_v2::api_mining::recipe::{TaskInput, TaskInputSchema, TaskInputSource};
        let args = json!({"recipe_id": "server-recipe", "__principal": "server-principal", "timeout_secs": 60, "query": "current-query", "inputs": {}});
        let (_, mut inputs) = validate_args(&args).unwrap();
        let declared: Vec<_> = [
            "recipe_id",
            "inputs",
            "timeout_secs",
            "__principal",
            "query",
        ]
        .into_iter()
        .map(|name| TaskInput {
            name: name.into(),
            schema: TaskInputSchema::String,
            example_value: "old".into(),
            source: TaskInputSource::TaskText,
        })
        .collect();
        merge_top_level_inputs(&mut inputs, &declared, &args);
        assert_eq!(
            inputs,
            HashMap::from([("query".into(), "current-query".into())])
        );
    }

    #[test]
    fn missing_recipe_id_is_a_structured_error_not_a_panic() {
        assert_eq!(
            validate_args(&json!({"inputs": {}})).unwrap_err(),
            "recipe_id is required"
        );
        let (_, inputs) =
            validate_args(&json!({"recipe_id": "rcp_1", "inputs": {"query": "go"}})).unwrap();
        assert_eq!(inputs.get("query").map(String::as_str), Some("go"));
    }

    #[test]
    fn generated_recipe_tool_cannot_override_its_bound_recipe_id() {
        assert!(validate_recipe_pack_binding("replay_recipe", Some("recipe__search_abcd")).is_ok());
        assert_eq!(
            validate_recipe_pack_binding("replay_recipe", None).unwrap_err(),
            "draft recipes are not available as planner tools"
        );
        assert!(
            validate_recipe_pack_binding("recipe__search_abcd", Some("recipe__search_abcd"))
                .is_ok()
        );
        assert_eq!(
            validate_recipe_pack_binding("recipe__search_abcd", Some("recipe__profile_efgh"))
                .unwrap_err(),
            "recipe id is not bound to the invoked recipe tool"
        );
        assert_eq!(
            validate_recipe_pack_binding("recipe__search_abcd", None).unwrap_err(),
            "draft recipes are not available as generated tools"
        );
    }
}
