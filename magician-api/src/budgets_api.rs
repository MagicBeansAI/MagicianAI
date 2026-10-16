//! Run budgets: the owner's defaults and per-task grants.
//!
//! Contract: `docs/plans/2026-10-03-slash-commands-and-run-budgets.md`.
//!
//! - `GET/PUT /api/magician/v2/budgets/defaults` — `execution.budgets` in the
//!   live `magician-config.yaml` (`native_max_cost_usd`, `harness_max_cost_usd`;
//!   each a positive number or `"unlimited"`). PUT takes a partial body and
//!   leaves a missing field unchanged.
//! - `GET/POST /api/magician/v2/tasks/{task_id}/budget` — the effective budget
//!   of the task's latest root execution; POST records one grant in the run's
//!   `budget_grants.json` and, when the run is paused on its budget, continues
//!   it through the HITL confirmation path.
//!
//! Every route is session-bearer only (`plane_api::owner_identity`): a budget
//! is owner authority, not something a grant or PAT may move. Errors are JSON
//! `{ "error": <code>, "message": <human text> }`.

use std::path::{Path, PathBuf};

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};

use magician::config::{
    configure_execution_budgets, execution_budget_settings, BudgetCapUsd, ExecutionBudgetSettings,
};
use magician::magician_v2::artifact_v2::{ScopeRef, V3ReadApi};
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::execution::agentic::run_budget::{
    append_budget_grant, budget_grants_path, live_run_budget, load_budget_grants,
    BudgetGrantRequest, EffectiveRunBudget, LiveRunBudget, OWNER_GRANT_SOURCE,
};
use magician::magician_v2::execution::agentic::ExecutionPauseKind;
use magician::magician_v2::runtime_settings::{
    config_file_lock, runtime_settings_paths, write_text_file_atomic,
};

use crate::web_api::{AgenticResumeRequest, AgenticResumeValue, MagicianV2Api};

/// The realtime event a client refreshes its budget view on.
pub const TASK_BUDGET_UPDATED_EVENT: &str = "task.budget.updated";

fn error(
    status: actix_web::http::StatusCode,
    code: &str,
    message: impl Into<String>,
) -> HttpResponse {
    HttpResponse::build(status).json(json!({ "error": code, "message": message.into() }))
}

fn bad_request(code: &str, message: impl Into<String>) -> HttpResponse {
    error(actix_web::http::StatusCode::BAD_REQUEST, code, message)
}

/// Session bearer only; refusals carry `error` and a human `message`.
fn require_owner(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
) -> Result<(), HttpResponse> {
    crate::plane_api::owner_identity(req, auth.map(|data| data.get_ref())).map(|_| ())
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn defaults_body(settings: &ExecutionBudgetSettings) -> Value {
    json!({
        "native_max_cost_usd": settings.native_max_cost_usd.to_json(),
        "harness_max_cost_usd": settings.harness_max_cost_usd.to_json(),
        "native_env_override": std::env::var_os("MAGICIAN_AGENTIC_MAX_COST_USD").is_some(),
    })
}

/// A partial update: each present field replaces the current value.
#[derive(Debug, Default, PartialEq)]
struct DefaultsUpdate {
    native: Option<BudgetCapUsd>,
    harness: Option<BudgetCapUsd>,
}

fn parse_defaults_update(body: &Value) -> Result<DefaultsUpdate, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    let mut update = DefaultsUpdate::default();
    for (key, value) in object {
        match key.as_str() {
            "native_max_cost_usd" => {
                update.native = Some(
                    BudgetCapUsd::from_json(value)
                        .map_err(|why| format!("native_max_cost_usd: {why}"))?,
                )
            },
            "harness_max_cost_usd" => {
                update.harness = Some(
                    BudgetCapUsd::from_json(value)
                        .map_err(|why| format!("harness_max_cost_usd: {why}"))?,
                )
            },
            other => return Err(format!("unknown field `{other}`")),
        }
    }
    if update.native.is_none() && update.harness.is_none() {
        return Err("name native_max_cost_usd and/or harness_max_cost_usd".to_string());
    }
    Ok(update)
}

impl DefaultsUpdate {
    fn apply(&self, settings: &mut ExecutionBudgetSettings) {
        if let Some(native) = self.native {
            settings.native_max_cost_usd = native;
        }
        if let Some(harness) = self.harness {
            settings.harness_max_cost_usd = harness;
        }
    }
}

fn yaml_cap(cap: BudgetCapUsd) -> String {
    match cap {
        BudgetCapUsd::Usd(value) => format!("{value}"),
        BudgetCapUsd::Unlimited => "unlimited".to_string(),
    }
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Replace (or add) `budgets:` inside the top-level `execution:` block,
/// touching no other line of the file — comments and sibling keys survive.
fn replace_execution_budgets(contents: &str, settings: &ExecutionBudgetSettings) -> String {
    replace_execution_child(contents, "budgets", |indent| {
        let pad = " ".repeat(indent);
        let child = " ".repeat(indent * 2);
        format!(
            "{pad}budgets:\n{child}native_max_cost_usd: {}\n{child}harness_max_cost_usd: {}\n",
            yaml_cap(settings.native_max_cost_usd),
            yaml_cap(settings.harness_max_cost_usd),
        )
    })
}

/// Replace (or add) the `key:` child block of the top-level `execution:`
/// block with `render(indent)`, touching no other line of the file —
/// comments and sibling keys survive.
pub(crate) fn replace_execution_child(
    contents: &str,
    key: &str,
    render: impl Fn(usize) -> String,
) -> String {
    let lines: Vec<&str> = contents.split_inclusive('\n').collect();
    let is_top_level_key = |line: &str| {
        !line.starts_with(' ')
            && !line.starts_with('\t')
            && !line.trim().is_empty()
            && !line.trim_start().starts_with('#')
    };
    let Some(start) = lines.iter().position(|line| {
        is_top_level_key(line) && line.trim_end().trim_end_matches(':').trim() == "execution"
    }) else {
        let mut out = contents.trim_end_matches('\n').to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("execution:\n");
        out.push_str(&render(2));
        return out;
    };
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| is_top_level_key(line))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    let indent = lines[start + 1..end]
        .iter()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| leading_spaces(line))
        .filter(|indent| *indent > 0)
        .unwrap_or(2);
    let prefix = format!("{key}:");
    let child_at = (start + 1..end).find(|&index| {
        let line = lines[index];
        leading_spaces(line) == indent && line.trim_start().starts_with(prefix.as_str())
    });
    let mut out = String::new();
    match child_at {
        Some(at) => {
            // The old block: every following line indented deeper (blank
            // lines inside it included), up to the next sibling key.
            let mut stop = at + 1;
            let mut last_content = at;
            while stop < end {
                let line = lines[stop];
                if line.trim().is_empty() {
                    stop += 1;
                    continue;
                }
                if leading_spaces(line) > indent {
                    last_content = stop;
                    stop += 1;
                    continue;
                }
                break;
            }
            for line in &lines[..at] {
                out.push_str(line);
            }
            out.push_str(&render(indent));
            for line in &lines[last_content + 1..] {
                out.push_str(line);
            }
        },
        None => {
            // Append after the execution block's last content line.
            let last_content = (start..end)
                .rev()
                .find(|&index| !lines[index].trim().is_empty())
                .unwrap_or(start);
            for line in &lines[..=last_content] {
                out.push_str(line);
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&render(indent));
            for line in &lines[last_content + 1..] {
                out.push_str(line);
            }
        },
    }
    out
}

/// The `execution` block of a config file's text (defaults when absent).
/// Read on its own: the whole-config loader also needs the router tables,
/// which this write never touches.
pub(crate) fn execution_settings_in(
    contents: &str,
) -> anyhow::Result<magician::config::MagicianExecutionSettings> {
    let document: serde_yaml::Value = serde_yaml::from_str(contents)?;
    Ok(match document.get("execution") {
        Some(execution) => serde_yaml::from_value(execution.clone())?,
        None => magician::config::MagicianExecutionSettings::default(),
    })
}

/// Read-modify-write the live config under the shared config-file lock, then
/// publish the new ceilings before releasing it.
fn save_defaults(path: &Path, update: &DefaultsUpdate) -> anyhow::Result<ExecutionBudgetSettings> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let mut settings = execution_settings_in(&original)?.budgets;
    update.apply(&mut settings);
    let next = replace_execution_budgets(&original, &settings);
    // Refuse to write a file whose execution block would not read back.
    anyhow::ensure!(
        execution_settings_in(&next)?.budgets == settings,
        "budgets did not round-trip"
    );
    write_text_file_atomic(path, &next, None)?;
    configure_execution_budgets(&settings);
    Ok(settings)
}

/// `GET /api/magician/v2/budgets/defaults`
pub async fn budget_defaults_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()) {
        return response;
    }
    HttpResponse::Ok().json(defaults_body(&execution_budget_settings()))
}

/// `PUT /api/magician/v2/budgets/defaults` — partial body.
pub async fn budget_defaults_put_handler(
    req: HttpRequest,
    body: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()) {
        return response;
    }
    let update = match parse_defaults_update(&body.into_inner()) {
        Ok(update) => update,
        Err(message) => return bad_request("invalid_budget_defaults", message),
    };
    put_defaults_at(runtime_settings_paths().config_path, update).await
}

async fn put_defaults_at(path: PathBuf, update: DefaultsUpdate) -> HttpResponse {
    match tokio::task::spawn_blocking(move || save_defaults(&path, &update)).await {
        Ok(Ok(settings)) => HttpResponse::Ok().json(defaults_body(&settings)),
        result => {
            tracing::error!(?result, "[BUDGETS] could not save execution.budgets");
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "budget_defaults_not_saved",
                "Could not save the budget defaults to magician-config.yaml.",
            )
        },
    }
}

// ---------------------------------------------------------------------------
// Per-task budget
// ---------------------------------------------------------------------------

struct TaskRun {
    scope: ScopeRef,
    task_id: String,
    execution_id: String,
    status: String,
    grants_path: PathBuf,
    engine_pin_path: PathBuf,
}

/// The newest execution that is not a delegated child.
fn latest_root_execution(
    entries: &[magician::magician_v2::artifact_v2::models::ExecutionIndexEntry],
) -> Option<&magician::magician_v2::artifact_v2::models::ExecutionIndexEntry> {
    entries
        .iter()
        .filter(|entry| {
            entry.parent_execution_id.is_none()
                && entry
                    .root_execution_id
                    .as_deref()
                    .is_none_or(|root| root == entry.execution_id)
        })
        .max_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then_with(|| a.execution_id.cmp(&b.execution_id))
        })
}

async fn resolve_task_run(
    api: &MagicianV2Api,
    req: &HttpRequest,
    task_id: &str,
) -> Result<TaskRun, HttpResponse> {
    let scope = crate::scope::resolve_required_scope_ref(req.headers(), None)?;
    let Some(service) = api.orchestrator().artifact_v2_service() else {
        return Err(error(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "task_store_unavailable",
            "The task store is not available.",
        ));
    };
    let entries = service
        .list_executions(&scope, task_id)
        .await
        .map_err(|err| {
            error(
                actix_web::http::StatusCode::NOT_FOUND,
                "task_not_found",
                format!("No task {task_id} in this workspace ({err})."),
            )
        })?;
    let Some(entry) = latest_root_execution(&entries) else {
        return Err(error(
            actix_web::http::StatusCode::NOT_FOUND,
            "task_has_no_run",
            "This task has not started a run yet.",
        ));
    };
    match service
        .execution_has_external_output_owner(&scope, task_id, &entry.execution_id)
        .await
    {
        Ok(true) => {
            return Err(error(
                actix_web::http::StatusCode::CONFLICT,
                "external_controller_budget_frozen",
                "This run has reviewed quality limits. Open its quality review to approve continuation for reached allowances.",
            ));
        },
        Ok(false) => {},
        Err(err) => {
            return Err(error(
                actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                "execution_budget_owner_unavailable",
                format!("The run's budget ownership could not be verified ({err})."),
            ));
        },
    }
    let principal = scope.principal();
    let workspace = scope.workspace();
    let execution_dir =
        service
            .workspace()
            .execution_dir(&principal, &workspace, task_id, &entry.execution_id);
    Ok(TaskRun {
        grants_path: budget_grants_path(
            service.workspace(),
            &principal,
            &workspace,
            Some(task_id),
            &entry.execution_id,
        ),
        engine_pin_path: execution_dir.join("engine_pin.json"),
        scope,
        task_id: task_id.to_string(),
        execution_id: entry.execution_id.clone(),
        status: entry.status.clone(),
    })
}

/// What is known about the run: the running loop's live snapshot, else its
/// pause record, else only its engine.
fn run_snapshot(api: &MagicianV2Api, run: &TaskRun) -> LiveRunBudget {
    if let Some(live) = api
        .full_pause_store()
        .budget_snapshot_for_execution(&run.execution_id)
    {
        return live;
    }
    if let Some(live) = live_run_budget(&run.execution_id) {
        return live;
    }
    if let Some(snapshot) =
        magician::magician_v2::execution::agentic::run_budget::persisted_run_budget(
            &run.grants_path.with_file_name(
                magician::magician_v2::execution::agentic::run_budget::BUDGET_USAGE_FILE,
            ),
            &run.execution_id,
        )
    {
        return snapshot;
    }
    let engine = std::fs::read(&run.engine_pin_path)
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<magician::magician_v2::execution::plane::RunEnginePin>(&bytes)
                .ok()
        })
        .map(|pin| pin.engine)
        .unwrap_or_else(|| {
            magician::magician_v2::execution::plane::harness_engine_snapshot().engine
        });
    LiveRunBudget {
        harness: magician::magician_v2::execution::agentic::run_budget::is_harness_engine(&engine),
        base_tokens: None,
        plane_cost_ceiling: None,
        cost_used_usd: 0.0,
        tokens_used: 0,
        iterations_used: 0,
        iterations_limit: 0,
    }
}

fn effective_budget(api: &MagicianV2Api, run: &TaskRun, resumed: bool) -> EffectiveRunBudget {
    let grants = load_budget_grants(&run.grants_path);
    EffectiveRunBudget::compute(&run.execution_id, &run_snapshot(api, run), &grants, resumed)
}

fn emit_budget_updated(api: &MagicianV2Api, run: &TaskRun, budget: &EffectiveRunBudget) {
    if let Some(broadcaster) = api.orchestrator().event_broadcaster() {
        broadcaster.emit_named(
            TASK_BUDGET_UPDATED_EVENT,
            "budget",
            Some(run.scope.principal()),
            Some(run.scope.workspace()),
            json!({
                "task_id": run.task_id,
                "execution_id": run.execution_id,
                "budget": budget,
            }),
        );
    }
}

/// `GET /api/magician/v2/tasks/{task_id}/budget`
pub async fn task_budget_get_handler(
    api: web::Data<MagicianV2Api>,
    req: HttpRequest,
    path: web::Path<String>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()) {
        return response;
    }
    let task_id = path.into_inner();
    match resolve_task_run(&api, &req, &task_id).await {
        Ok(run) => HttpResponse::Ok().json(effective_budget(&api, &run, false)),
        Err(response) => response,
    }
}

/// The HITL answer that continues a run paused on its budget: the same
/// "Continue execution?" confirmation a person gives, addressed to the
/// pause's canonical correlation id. `None` when the run is not paused on its
/// budget (an ask, an approval, or running) — a grant never answers those.
fn budget_pause_continue_request(
    pending: &[magician::magician_v2::execution::agentic::PendingPauseInfo],
) -> Option<AgenticResumeRequest> {
    let pause = pending
        .iter()
        .find(|pending| pending.pause_kind == Some(ExecutionPauseKind::Budget))?;
    let correlation = if pause.hitl_correlation_id.is_empty() {
        pause.key.clone()
    } else {
        pause.hitl_correlation_id.clone()
    };
    Some(AgenticResumeRequest {
        pause_state_id: Some(correlation),
        plan_id: None,
        step_id: None,
        input_type: "confirmation".to_string(),
        value: AgenticResumeValue::Confirmation { confirmed: true },
        agent_id: None,
        goal_id: None,
        cycle_id: None,
        answer_channel: Some("budget".to_string()),
    })
}

/// `POST /api/magician/v2/tasks/{task_id}/budget`
pub async fn task_budget_post_handler(
    api: web::Data<MagicianV2Api>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()) {
        return response;
    }
    let request = match serde_json::from_value::<BudgetGrantRequest>(body.into_inner()) {
        Ok(request) => request,
        Err(why) => {
            return bad_request(
                "invalid_budget_grant",
                format!(
                "Send one of add_cost_usd, add_tokens, add_iterations, add_duration_secs or unlimited: true ({why})."
            ),
            )
        },
    };
    if let Err(message) = request.validate() {
        return bad_request("invalid_budget_grant", message);
    }
    let task_id = path.into_inner();
    let run = match resolve_task_run(&api, &req, &task_id).await {
        Ok(run) => run,
        Err(response) => return response,
    };
    if matches!(run.status.as_str(), "completed" | "failed" | "cancelled") {
        return error(
            actix_web::http::StatusCode::CONFLICT,
            "run_finished",
            format!(
                "The task's latest run has {}; there is nothing to extend.",
                run.status
            ),
        );
    }
    let grant = request.into_grant(chrono::Utc::now(), OWNER_GRANT_SOURCE);
    let grants_path = run.grants_path.clone();
    match tokio::task::spawn_blocking(move || append_budget_grant(&grants_path, grant)).await {
        Ok(Ok(_)) => {},
        result => {
            tracing::error!(?result, task_id = %run.task_id, "[BUDGETS] grant not recorded");
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "budget_grant_not_saved",
                "Could not record the budget grant.",
            );
        },
    }

    let budget_resume = budget_pause_continue_request(
        &api.full_pause_store()
            .get_pending_for_execution(&run.execution_id),
    );
    let mut resumed = false;
    if let Some(resume) = budget_resume {
        match api
            .resume_agentic_execution_acknowledged_when_admitted(
                web::Path::from(run.execution_id.clone()),
                web::Json(resume),
                Some((
                    run.scope.principal().to_string(),
                    run.scope.workspace().to_string(),
                )),
                "budget".to_string(),
            )
            .await
        {
            Ok(response) if response.status().is_success() => resumed = true,
            Ok(response) => tracing::warn!(
                task_id = %run.task_id,
                execution_id = %run.execution_id,
                status = %response.status(),
                "[BUDGETS] grant recorded but the budget pause did not resume"
            ),
            Err(why) => tracing::warn!(
                task_id = %run.task_id,
                execution_id = %run.execution_id,
                error = %why,
                "[BUDGETS] grant recorded but the budget pause did not resume"
            ),
        }
    }
    let budget = effective_budget(&api, &run, resumed);
    emit_budget_updated(&api, &run, &budget);
    HttpResponse::Ok().json(budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_LIKE: &str = "\
plane:
  enabled: true
# operator note above execution
execution:
  magicutor_base_url: http://127.0.0.1:3003/
  # the loop's engine
  harness_engine: claude_code
  file_sandbox:
    mode: workspace_write
    allowed_roots:
    - .
  harness_turn_max_seconds: 2400
chat:
  harness_engine: magician
";

    fn settings(native: BudgetCapUsd, harness: BudgetCapUsd) -> ExecutionBudgetSettings {
        ExecutionBudgetSettings {
            native_max_cost_usd: native,
            harness_max_cost_usd: harness,
        }
    }

    #[test]
    fn budgets_are_added_inside_execution_without_touching_other_lines() {
        let next = replace_execution_budgets(
            LIVE_LIKE,
            &settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0)),
        );
        assert!(next.contains(
            "  harness_turn_max_seconds: 2400\n  budgets:\n    native_max_cost_usd: 15\n    harness_max_cost_usd: 100\nchat:\n"
        ), "{next}");
        assert!(next.contains("# operator note above execution\n"));
        assert!(next.contains("  # the loop's engine\n"));
        let parsed: serde_yaml::Value = serde_yaml::from_str(&next).unwrap();
        assert_eq!(parsed["chat"]["harness_engine"], "magician");
        assert_eq!(parsed["execution"]["file_sandbox"]["allowed_roots"][0], ".");
    }

    #[test]
    fn an_existing_budgets_block_is_replaced_in_place() {
        let first = replace_execution_budgets(
            LIVE_LIKE,
            &settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0)),
        );
        let second = replace_execution_budgets(
            &first,
            &settings(BudgetCapUsd::Usd(5.0), BudgetCapUsd::Unlimited),
        );
        assert_eq!(second.matches("budgets:").count(), 1, "{second}");
        assert!(
            second.contains("    native_max_cost_usd: 5\n    harness_max_cost_usd: unlimited\n")
        );
        let config: magician::config::MagicianExecutionSettings = serde_yaml::from_value(
            serde_yaml::from_str::<serde_yaml::Value>(&second).unwrap()["execution"].clone(),
        )
        .unwrap();
        assert_eq!(config.budgets.harness_max_cost_usd, BudgetCapUsd::Unlimited);
        assert_eq!(config.budgets.native_max_cost_usd, BudgetCapUsd::Usd(5.0));
    }

    #[test]
    fn a_file_without_execution_gets_one() {
        let next = replace_execution_budgets(
            "chat:\n  harness_engine: magician\n",
            &settings(BudgetCapUsd::Usd(2.5), BudgetCapUsd::Usd(100.0)),
        );
        assert!(next.ends_with("execution:\n  budgets:\n    native_max_cost_usd: 2.5\n    harness_max_cost_usd: 100\n"), "{next}");
    }

    #[test]
    fn defaults_updates_are_partial_and_validated() {
        assert_eq!(
            parse_defaults_update(&json!({"harness_max_cost_usd": "unlimited"})),
            Ok(DefaultsUpdate {
                native: None,
                harness: Some(BudgetCapUsd::Unlimited),
            })
        );
        assert_eq!(
            parse_defaults_update(&json!({"native_max_cost_usd": 50})),
            Ok(DefaultsUpdate {
                native: Some(BudgetCapUsd::Usd(50.0)),
                harness: None,
            })
        );
        for bad in [
            json!({}),
            json!({"native_max_cost_usd": -1}),
            json!({"native_max_cost_usd": "NaN"}),
            json!({"harness_max_cost_usd": null}),
            json!({"harness_max_cost_usd": 100, "bogus": 1}),
            json!([1]),
        ] {
            assert!(
                parse_defaults_update(&bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn a_partial_put_round_trips_and_leaves_the_other_field_unchanged() {
        let _budgets = magician::config::execution_budget_test_lock();
        let previous = execution_budget_settings();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician-config.yaml");
        std::fs::write(
            &path,
            "execution:\n  harness_engine: magician\n  budgets:\n    native_max_cost_usd: 15\n    harness_max_cost_usd: 100\n",
        )
        .unwrap();

        let saved = save_defaults(
            &path,
            &parse_defaults_update(&json!({"harness_max_cost_usd": 250})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved,
            settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(250.0))
        );
        assert_eq!(execution_budget_settings(), saved, "published live");

        let saved = save_defaults(
            &path,
            &parse_defaults_update(&json!({"native_max_cost_usd": "unlimited"})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved,
            settings(BudgetCapUsd::Unlimited, BudgetCapUsd::Usd(250.0))
        );

        // GET reads what PUT wrote, from the file and the live cell alike.
        let reloaded = execution_settings_in(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded.budgets, saved);
        assert_eq!(reloaded.harness_engine, "magician");
        let body = defaults_body(&execution_budget_settings());
        assert_eq!(body["native_max_cost_usd"], json!("unlimited"));
        assert_eq!(body["harness_max_cost_usd"], json!(250.0));

        configure_execution_budgets(&previous);
    }

    #[test]
    fn a_grant_continues_only_a_budget_pause() {
        use magician::magician_v2::execution::agentic::{PendingPauseInfo, UserInputType};
        let confirmation = || UserInputType::Confirmation {
            confirm_label: Some("Resume".to_string()),
            deny_label: Some("Cancel".to_string()),
            destructive: false,
        };
        let mut ask = PendingPauseInfo::for_test("exec_1:plan:step", "exec_1", confirmation());
        ask.pause_kind = Some(ExecutionPauseKind::UserInput);
        assert!(budget_pause_continue_request(std::slice::from_ref(&ask)).is_none());
        assert!(budget_pause_continue_request(&[]).is_none());

        let mut budget = PendingPauseInfo::for_test("exec_1:plan:step", "exec_1", confirmation());
        budget.pause_kind = Some(ExecutionPauseKind::Budget);
        let resume = budget_pause_continue_request(&[ask, budget.clone()])
            .expect("a budget pause is continued");
        assert_eq!(
            resume.pause_state_id.as_deref(),
            Some(budget.hitl_correlation_id.as_str())
        );
        assert_eq!(resume.input_type, "confirmation");
        assert!(matches!(
            resume.value,
            AgenticResumeValue::Confirmation { confirmed: true }
        ));
    }

    #[test]
    fn the_latest_root_execution_ignores_delegated_children() {
        let entry = |id: &str, started: &str, parent: Option<&str>| {
            serde_json::from_value::<magician::magician_v2::artifact_v2::models::ExecutionIndexEntry>(json!({
                "execution_id": id,
                "task_id": "task_1",
                "root_execution_id": parent.map(|_| "exec_root"),
                "parent_execution_id": parent,
                "agent_id": "web-researcher",
                "relationship_type": "root",
                "status": "running",
                "plan_id": null,
                "primary_execution_output_id": null,
                "started_at": started,
                "completed_at": null,
                "updated_at": started,
            }))
            .unwrap()
        };
        let entries = vec![
            entry("exec_old", "2026-10-03T08:00:00Z", None),
            entry("exec_root", "2026-10-03T09:00:00Z", None),
            entry("exec_child", "2026-10-03T09:30:00Z", Some("exec_root")),
        ];
        assert_eq!(
            latest_root_execution(&entries).map(|entry| entry.execution_id.as_str()),
            Some("exec_root")
        );
    }
}
