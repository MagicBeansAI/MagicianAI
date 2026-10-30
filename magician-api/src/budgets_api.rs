//! Run budgets: the owner's defaults and per-task grants.
//!
//! Contract: `docs/archive/plans/2026-10-03-slash-commands-and-run-budgets.md`.
//!
//! - `GET/PUT /api/magician/v2/budgets/defaults` — `execution.budgets` in the
//!   live `magician-config.yaml` (`native_max_cost_usd`, `harness_max_cost_usd`;
//!   each a positive number or `"unlimited"`). PUT takes a partial body and
//!   leaves a missing field unchanged.
//! - `GET/PUT /api/magician/v2/budgets/envelope` — the owner-set spend envelope
//!   across all autonomous LLM work (`execution.budgets.spend_envelope`:
//!   `enabled`, `daily_usd`, `total_usd`, `window`, `reset_offset_minutes`,
//!   `warn_at_pct`, `unpriced`). GET also returns this window's live status
//!   (spent, remaining, per-class and per-scope breakdown, reset time). PUT
//!   takes a partial body.
//! - `POST /api/magician/v2/budgets/envelope/grant` — `{ add_cost_usd, note? }`
//!   adds money to the current window (append-only
//!   `system/spend_envelope_grants.json`; there is no unlimited grant) and
//!   clears the `spend_envelope:exhausted` attention item.
//! - `GET/POST /api/magician/v2/tasks/{task_id}/budget` — the effective budget
//!   of the task's latest root execution; POST records one grant in the run's
//!   `budget_grants.json` and, when the run is paused on its budget, continues
//!   it through the HITL confirmation path.
//!
//! Every route needs the owner: a login session (`plane_api::owner_identity`)
//! or the owner's verified paired iOS/Android phone
//! (`programs_api::require_owner_session_or_phone`). A budget is owner
//! authority, not something a grant, PAT or automation device may move. Errors
//! are JSON `{ "error": <code>, "message": <human text> }`.

use std::path::{Path, PathBuf};

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use magician::config::{
    configure_execution_budgets, execution_budget_settings, BudgetCapUsd, ExecutionBudgetSettings,
    SpendEnvelopeSettings, SpendEnvelopeWindow, UnpricedPolicy,
};
use magician::magician_v2::artifact_v2::{ScopeRef, V3ReadApi};
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::execution::agentic::run_budget::{
    append_budget_grant, budget_grants_path, live_run_budget, load_budget_grants,
    BudgetGrantRequest, EffectiveRunBudget, LiveRunBudget, OWNER_GRANT_SOURCE,
};
use magician::magician_v2::execution::agentic::ExecutionPauseKind;
use magician::magician_v2::resource_authority::canonicalize_commodity;
use magician::magician_v2::resource_authority::config::{
    BudgetRow, BudgetScope, ModelSpendConfig, ResourceAuthorityConfig,
};
use magician::magician_v2::resource_authority::token::{CarryoverPolicy, CeilingPeriod};
use magician::magician_v2::runtime_settings::{
    apply_yaml_section, config_file_lock, runtime_settings_paths, write_text_file_atomic,
};
use magician::magician_v2::spend_envelope::{
    self, append_grant, validate_grant_usd, EnvelopeGrant,
    OWNER_GRANT_SOURCE as ENVELOPE_GRANT_SOURCE,
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

/// Owner session or verified paired owner phone; refusals carry `error` and a
/// human `message`.
async fn require_owner(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
) -> Result<(), HttpResponse> {
    crate::programs_api::require_owner_session_or_phone(req, auth.map(|data| data.get_ref())).await
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

/// The owner's budgets as one config file's text holds them: behaviour in
/// `execution.budgets`, and the model spend limits resource authority keeps
/// (`resource_authority.model_spend`; absent in a config that predates the
/// move, which then takes the legacy `execution.budgets` values).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BudgetFile {
    execution: ExecutionBudgetSettings,
    model_spend: Option<ModelSpendConfig>,
}

impl BudgetFile {
    /// What the per-run valves and the spend envelope enforce.
    pub(crate) fn effective(&self) -> ExecutionBudgetSettings {
        self.model_spend
            .unwrap_or_else(|| ModelSpendConfig::from_legacy(&self.execution))
            .apply_to(&self.execution)
    }
}

pub(crate) fn budgets_in(contents: &str) -> anyhow::Result<BudgetFile> {
    let document: serde_yaml::Value = serde_yaml::from_str(contents)?;
    let model_spend = match document
        .get("resource_authority")
        .and_then(|section| section.get("model_spend"))
    {
        Some(section) if !section.is_null() => Some(serde_yaml::from_value(section.clone())?),
        _ => None,
    };
    Ok(BudgetFile {
        execution: execution_settings_in(contents)?.budgets,
        model_spend,
    })
}

/// Write `effective` back: its limits into `resource_authority.model_spend`,
/// its envelope behaviour into `execution.budgets.spend_envelope`. Only
/// changed values are touched — comments and sibling keys survive — and the
/// legacy limit keys in `execution.budgets` are left as they are (ignored once
/// the resource authority section exists).
fn write_budgets(
    contents: &str,
    before: &BudgetFile,
    effective: &ExecutionBudgetSettings,
) -> anyhow::Result<String> {
    let model_spend = ModelSpendConfig {
        window_usd: effective.spend_envelope.daily_usd,
        window: effective.spend_envelope.window,
        lifetime_usd: effective.spend_envelope.total_usd,
        run_magician_usd: effective.native_max_cost_usd,
        run_harness_usd: effective.harness_max_cost_usd,
    };
    let next = apply_yaml_section(
        contents,
        &["resource_authority", "model_spend"],
        &before.model_spend,
        &Some(model_spend),
    )?;
    let mut execution = before.execution;
    execution.spend_envelope.enabled = effective.spend_envelope.enabled;
    execution.spend_envelope.reset_offset_minutes = effective.spend_envelope.reset_offset_minutes;
    execution.spend_envelope.warn_at_pct = effective.spend_envelope.warn_at_pct;
    execution.spend_envelope.unpriced = effective.spend_envelope.unpriced;
    Ok(apply_yaml_section(
        &next,
        &["execution", "budgets"],
        &before.execution,
        &execution,
    )?)
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
/// publish the new limits before releasing it. `edit` changes the effective
/// budgets; `validate` refuses a combination before anything is written.
fn save_budgets(
    path: &Path,
    edit: impl FnOnce(&mut ExecutionBudgetSettings),
    validate: impl FnOnce(&ExecutionBudgetSettings) -> Result<(), String>,
) -> anyhow::Result<ExecutionBudgetSettings> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let before = budgets_in(&original)?;
    let mut effective = before.effective();
    edit(&mut effective);
    validate(&effective).map_err(anyhow::Error::msg)?;
    let next = write_budgets(&original, &before, &effective)?;
    // Refuse to write a file whose budgets would not read back.
    anyhow::ensure!(
        budgets_in(&next)?.effective() == effective,
        "budgets did not round-trip"
    );
    if next != original {
        write_text_file_atomic(path, &next, None)?;
    }
    configure_execution_budgets(&effective);
    Ok(effective)
}

fn save_defaults(path: &Path, update: &DefaultsUpdate) -> anyhow::Result<ExecutionBudgetSettings> {
    save_budgets(path, |settings| update.apply(settings), |_| Ok(()))
}

/// `GET /api/magician/v2/budgets/defaults`
pub async fn budget_defaults_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
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
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
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
// Action limits (resource authority budget rows)
// ---------------------------------------------------------------------------

/// The `resource_authority` block of a config file's text (defaults when absent).
fn resource_authority_in(contents: &str) -> anyhow::Result<ResourceAuthorityConfig> {
    let document: serde_yaml::Value = serde_yaml::from_str(contents)?;
    Ok(match document.get("resource_authority") {
        Some(section) if !section.is_null() => serde_yaml::from_value(section.clone())?,
        _ => ResourceAuthorityConfig::default(),
    })
}

fn limits_body(config: &ResourceAuthorityConfig) -> Value {
    json!({
        "enabled": config.enabled,
        "limits": config.budgets.iter().map(|row| json!({
            "scope": row.scope,
            "id": row.id,
            "commodity": row.commodity,
            "ceiling": row.ceiling.to_string(),
            "period": row.period,
        })).collect::<Vec<_>>(),
        // The dispatch gate reads its rows at boot; an edit applies on the
        // next restart.
        "applies": "on_restart",
    })
}

/// One action limit to add or change: `{scope, id, commodity, ceiling, period}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitUpsert {
    scope: BudgetScope,
    id: String,
    commodity: String,
    ceiling: Value,
    period: CeilingPeriod,
}

/// Which action limit to remove.
#[derive(Debug, Deserialize)]
pub struct LimitKey {
    scope: BudgetScope,
    id: String,
    commodity: String,
}

fn parse_ceiling(value: &Value) -> Result<rust_decimal::Decimal, String> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.trim().trim_start_matches('$').replace(',', ""),
        _ => return Err("ceiling must be a positive number".to_string()),
    };
    let ceiling: rust_decimal::Decimal = text
        .parse()
        .map_err(|_| format!("ceiling must be a positive number, got {text:?}"))?;
    if ceiling <= rust_decimal::Decimal::ZERO {
        return Err("ceiling must be greater than zero; remove the limit instead".to_string());
    }
    Ok(ceiling)
}

/// Read-modify-write `resource_authority.budgets` under the config-file lock.
fn save_limits(
    path: &Path,
    edit: impl FnOnce(&mut Vec<BudgetRow>) -> Result<(), String>,
) -> anyhow::Result<Result<ResourceAuthorityConfig, String>> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let before = resource_authority_in(&original)?;
    let mut after = before.clone();
    if let Err(message) = edit(&mut after.budgets) {
        return Ok(Err(message));
    }
    if let Err(error) = after.validate() {
        return Ok(Err(error.to_string()));
    }
    let reads_back = |text: &str| -> anyhow::Result<bool> {
        Ok(serde_json::to_value(&resource_authority_in(text)?.budgets)?
            == serde_json::to_value(&after.budgets)?)
    };
    // Row by row first, so the comments between rows and each row's own
    // spelling survive; the whole list only when that cannot be done.
    let next = match edit_limit_rows(&original, &before.budgets, &after.budgets) {
        Some(next) if reads_back(&next).unwrap_or(false) => next,
        _ => apply_yaml_section(
            &original,
            &["resource_authority", "budgets"],
            &before.budgets,
            &after.budgets,
        )?,
    };
    anyhow::ensure!(reads_back(&next)?, "action limits did not round-trip");
    if next != original {
        write_text_file_atomic(path, &next, None)?;
    }
    Ok(Ok(after))
}

fn limit_key(row: &BudgetRow) -> (BudgetScope, String, String) {
    (
        row.scope,
        row.id.trim().to_string(),
        canonicalize_commodity(&row.commodity),
    )
}

/// One `- ` item of `resource_authority.budgets` in the file's text.
struct RowSpan {
    /// Line indices `[start, end)` of the item, comments above it excluded.
    start: usize,
    end: usize,
    key: (BudgetScope, String, String),
}

/// Edit `resource_authority.budgets` in place: a changed row has only its
/// `ceiling:` and `period:` values replaced, a removed row loses only its
/// own lines, an added row is appended after the last. `None` when the
/// list's shape is one this does not edit (flow style, a row that does not
/// parse, a key it would have to add) — the caller then rewrites the list.
fn edit_limit_rows(contents: &str, before: &[BudgetRow], after: &[BudgetRow]) -> Option<String> {
    let mut lines: Vec<String> = contents.split_inclusive('\n').map(str::to_string).collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let blank_or_comment = |line: &str| {
        let trimmed = line.trim();
        trimmed.is_empty() || trimmed.starts_with('#')
    };
    let block = lines
        .iter()
        .position(|line| line.trim_end() == "resource_authority:")?;
    let block_end = (block + 1..lines.len())
        .find(|&index| !blank_or_comment(&lines[index]) && indent(&lines[index]) == 0)
        .unwrap_or(lines.len());
    let key_line = (block + 1..block_end).find(|&index| {
        let trimmed = lines[index].trim_start();
        trimmed.starts_with("budgets:")
            && trimmed
                .trim_end()
                .trim_end_matches(|c: char| c != ':')
                .ends_with("budgets:")
    })?;
    if lines[key_line]
        .trim_start()
        .trim_start_matches("budgets:")
        .trim()
        .starts_with('[')
    {
        return None;
    }
    let key_indent = indent(&lines[key_line]);
    // Items: `- ` at the list's indent (indentless lists sit at the key's).
    let mut spans: Vec<RowSpan> = Vec::new();
    let mut item_indent = None;
    let mut index = key_line + 1;
    let mut list_end = key_line + 1;
    while index < block_end {
        let line = &lines[index];
        if blank_or_comment(line) {
            index += 1;
            continue;
        }
        let here = indent(line);
        if line.trim_start().starts_with("- ")
            && here >= key_indent
            && item_indent.is_none_or(|item| item == here)
        {
            item_indent = Some(here);
            let start = index;
            let mut end = index + 1;
            while end < block_end {
                let next = &lines[end];
                if !blank_or_comment(next) && (indent(next) <= here) {
                    break;
                }
                end += 1;
            }
            // Trailing blank/comment lines belong to what follows.
            let mut item_end = end;
            while item_end > start + 1 && blank_or_comment(&lines[item_end - 1]) {
                item_end -= 1;
            }
            let text: String = lines[start..item_end]
                .iter()
                .enumerate()
                .map(|(offset, line)| {
                    let line = line.get(here..).unwrap_or_else(|| line.trim_start());
                    if offset == 0 {
                        line.replacen("- ", "  ", 1)
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            let row: BudgetRow = serde_yaml::from_str(&text).ok()?;
            spans.push(RowSpan {
                start,
                end: item_end,
                key: limit_key(&row),
            });
            list_end = item_end;
            index = end;
            continue;
        }
        break;
    }
    let item_indent = item_indent?;
    let before_keys: Vec<_> = before.iter().map(limit_key).collect();
    if spans
        .iter()
        .map(|span| span.key.clone())
        .collect::<Vec<_>>()
        != before_keys
    {
        return None;
    }
    let value_re = |field: &str| {
        regex::Regex::new(&format!(
            r"^(\s*(?:- )?{field}:\s*)([^#\n]*?)(\s*(?:#.*)?\n?)$"
        ))
        .ok()
    };
    let (ceiling_re, period_re) = (value_re("ceiling")?, value_re("period")?);
    let period_text = |period: &CeilingPeriod| {
        serde_json::to_value(period)
            .ok()?
            .as_str()
            .map(str::to_string)
    };
    // Changes inside existing rows, and removals, from the bottom up.
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for span in spans.iter().rev() {
        match after.iter().find(|row| limit_key(row) == span.key) {
            None => removals.push((span.start, span.end)),
            Some(row) => {
                let old = before
                    .iter()
                    .find(|candidate| limit_key(candidate) == span.key)?;
                for (changed, re, value) in [
                    (
                        old.ceiling != row.ceiling,
                        &ceiling_re,
                        format!("'{}'", row.ceiling),
                    ),
                    (
                        old.period != row.period,
                        &period_re,
                        period_text(&row.period)?,
                    ),
                ] {
                    if !changed {
                        continue;
                    }
                    let line = (span.start..span.end).find(|&index| re.is_match(&lines[index]))?;
                    lines[line] = re
                        .replace(&lines[line], format!("${{1}}{value}${{3}}"))
                        .into_owned();
                }
            },
        }
    }
    let added: Vec<&BudgetRow> = after
        .iter()
        .filter(|row| !before_keys.contains(&limit_key(row)))
        .collect();
    let mut insert: Vec<String> = Vec::new();
    for row in added {
        let pad = " ".repeat(item_indent);
        insert.push(format!(
            "{pad}- scope: {}\n{pad}  id: '{}'\n{pad}  commodity: {}\n{pad}  ceiling: '{}'\n{pad}  period: {}\n{pad}  carryover:\n{pad}    type: none\n",
            serde_json::to_value(row.scope).ok()?.as_str()?,
            row.id.replace('\'', "''"),
            row.commodity,
            row.ceiling,
            period_text(&row.period)?,
        ));
    }
    if !insert.is_empty() {
        if lines[list_end - 1].ends_with('\n') {
            lines.splice(list_end..list_end, insert);
        } else {
            return None;
        }
    }
    for (start, end) in removals {
        lines.drain(start..end);
    }
    Some(lines.concat())
}

async fn run_limits_save(
    edit: impl FnOnce(&mut Vec<BudgetRow>) -> Result<(), String> + Send + 'static,
) -> HttpResponse {
    let path = runtime_settings_paths().config_path;
    match tokio::task::spawn_blocking(move || save_limits(&path, edit)).await {
        Ok(Ok(Ok(config))) => HttpResponse::Ok().json(limits_body(&config)),
        Ok(Ok(Err(message))) => bad_request("invalid_action_limit", message),
        result => {
            tracing::error!(
                ?result,
                "[BUDGETS] could not save resource_authority.budgets"
            );
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "action_limits_not_saved",
                "Could not save the action limits to magician-config.yaml.",
            )
        },
    }
}

/// `GET /api/magician/v2/budgets/limits` — resource authority's action
/// limits (emails, messages, orders per period) as the live config holds them.
pub async fn limits_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let path = runtime_settings_paths().config_path;
    match tokio::task::spawn_blocking(move || -> anyhow::Result<ResourceAuthorityConfig> {
        let lock = config_file_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        resource_authority_in(&std::fs::read_to_string(&path)?)
    })
    .await
    {
        Ok(Ok(config)) => HttpResponse::Ok().json(limits_body(&config)),
        result => {
            tracing::error!(
                ?result,
                "[BUDGETS] could not read resource_authority.budgets"
            );
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "action_limits_unavailable",
                "Could not read the action limits.",
            )
        },
    }
}

/// `PUT /api/magician/v2/budgets/limits` — add a limit, or change one's
/// ceiling and period (its carryover and system-ceiling link are kept).
pub async fn limits_put_handler(
    req: HttpRequest,
    body: web::Json<LimitUpsert>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let upsert = body.into_inner();
    let ceiling = match parse_ceiling(&upsert.ceiling) {
        Ok(ceiling) => ceiling,
        Err(message) => return bad_request("invalid_action_limit", message),
    };
    run_limits_save(move |rows| {
        let id = upsert.id.trim().to_string();
        let commodity = canonicalize_commodity(&upsert.commodity);
        if id.is_empty() || commodity.is_empty() {
            return Err("name the limit's id and commodity".to_string());
        }
        match rows.iter_mut().find(|row| {
            row.scope == upsert.scope
                && row.id.trim() == id
                && canonicalize_commodity(&row.commodity) == commodity
        }) {
            Some(row) => {
                row.ceiling = ceiling;
                row.period = upsert.period;
            },
            None => rows.push(BudgetRow {
                scope: upsert.scope,
                id,
                commodity,
                ceiling,
                period: upsert.period,
                carryover: CarryoverPolicy::None,
                system_ceiling_id: None,
            }),
        }
        Ok(())
    })
    .await
}

/// `DELETE /api/magician/v2/budgets/limits?scope=&id=&commodity=` — remove a
/// limit. What a tool that declares spend does with no limit left for it is
/// the dispatch gate's rule, unchanged here.
pub async fn limits_delete_handler(
    req: HttpRequest,
    query: web::Query<LimitKey>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let key = query.into_inner();
    run_limits_save(move |rows| {
        let commodity = canonicalize_commodity(&key.commodity);
        let before = rows.len();
        rows.retain(|row| {
            !(row.scope == key.scope
                && row.id.trim() == key.id.trim()
                && canonicalize_commodity(&row.commodity) == commodity)
        });
        if rows.len() == before {
            return Err("no such limit".to_string());
        }
        Ok(())
    })
    .await
}

// ---------------------------------------------------------------------------
// Spend envelope
// ---------------------------------------------------------------------------

fn envelope_settings_body(settings: &SpendEnvelopeSettings) -> Value {
    json!({
        "enabled": settings.enabled,
        "daily_usd": settings.daily_usd.to_json(),
        "total_usd": settings.total_usd.to_json(),
        "window": settings.window.as_str(),
        "reset_offset_minutes": settings.reset_offset_minutes,
        "warn_at_pct": settings.warn_at_pct,
        "unpriced": settings.unpriced.as_str(),
    })
}

/// The settings plus the live window status (absent where no envelope is
/// installed, as in a CLI command).
fn envelope_body(settings: &SpendEnvelopeSettings) -> Value {
    let status = spend_envelope::envelope()
        .map(|envelope| envelope.status_at(chrono::Utc::now().timestamp_millis()));
    json!({
        "settings": envelope_settings_body(settings),
        "installed": status.is_some(),
        "status": status,
    })
}

/// A partial update: each present field replaces the current value.
#[derive(Debug, Default, PartialEq)]
struct EnvelopeUpdate {
    enabled: Option<bool>,
    daily_usd: Option<BudgetCapUsd>,
    total_usd: Option<BudgetCapUsd>,
    window: Option<SpendEnvelopeWindow>,
    reset_offset_minutes: Option<i32>,
    warn_at_pct: Option<u8>,
    unpriced: Option<UnpricedPolicy>,
}

fn parse_envelope_update(body: &Value) -> Result<EnvelopeUpdate, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    let mut update = EnvelopeUpdate::default();
    for (key, value) in object {
        match key.as_str() {
            "enabled" => {
                update.enabled = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| "enabled: must be true or false".to_string())?,
                )
            },
            "daily_usd" => {
                update.daily_usd = Some(
                    BudgetCapUsd::from_json(value).map_err(|why| format!("daily_usd: {why}"))?,
                )
            },
            "total_usd" => {
                update.total_usd = Some(
                    BudgetCapUsd::from_json(value).map_err(|why| format!("total_usd: {why}"))?,
                )
            },
            "window" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| "window: must be a string".to_string())?;
                update.window =
                    Some(SpendEnvelopeWindow::parse(text).map_err(|why| format!("window: {why}"))?)
            },
            "reset_offset_minutes" => {
                let minutes = value
                    .as_i64()
                    .ok_or_else(|| "reset_offset_minutes: must be a whole number".to_string())?;
                if minutes.abs() >= 24 * 60 {
                    return Err(format!(
                        "reset_offset_minutes: must be within a day (-1439..=1439), got {minutes}"
                    ));
                }
                update.reset_offset_minutes = Some(minutes as i32);
            },
            "warn_at_pct" => {
                let pct = value
                    .as_u64()
                    .ok_or_else(|| "warn_at_pct: must be a whole number".to_string())?;
                if !(1..=100).contains(&pct) {
                    return Err(format!("warn_at_pct: must be between 1 and 100, got {pct}"));
                }
                update.warn_at_pct = Some(pct as u8);
            },
            "unpriced" => {
                let text = value
                    .as_str()
                    .ok_or_else(|| "unpriced: must be a string".to_string())?;
                update.unpriced =
                    Some(UnpricedPolicy::parse(text).map_err(|why| format!("unpriced: {why}"))?)
            },
            other => return Err(format!("unknown field `{other}`")),
        }
    }
    if update == EnvelopeUpdate::default() {
        return Err(
            "name at least one of enabled, daily_usd, total_usd, window, reset_offset_minutes, warn_at_pct, unpriced"
                .to_string(),
        );
    }
    Ok(update)
}

impl EnvelopeUpdate {
    fn apply(&self, settings: &mut SpendEnvelopeSettings) {
        if let Some(enabled) = self.enabled {
            settings.enabled = enabled;
        }
        if let Some(daily) = self.daily_usd {
            settings.daily_usd = daily;
        }
        if let Some(total) = self.total_usd {
            settings.total_usd = total;
        }
        if let Some(window) = self.window {
            settings.window = window;
        }
        if let Some(minutes) = self.reset_offset_minutes {
            settings.reset_offset_minutes = minutes;
        }
        if let Some(pct) = self.warn_at_pct {
            settings.warn_at_pct = pct;
        }
        if let Some(unpriced) = self.unpriced {
            settings.unpriced = unpriced;
        }
    }
}

/// Read-modify-write the live config under the shared config-file lock, then
/// publish the new settings (the running envelope picks them up through
/// `configure_execution_budgets`) before releasing it.
fn save_envelope(path: &Path, update: &EnvelopeUpdate) -> anyhow::Result<ExecutionBudgetSettings> {
    save_budgets(
        path,
        |settings| update.apply(&mut settings.spend_envelope),
        |settings| settings.spend_envelope.validate(),
    )
}

/// Remember which scope the owner's attention items belong to: the scope the
/// owner request resolved to.
fn note_owner_scope(req: &HttpRequest) {
    if let (Some(envelope), Ok(scope)) = (
        spend_envelope::envelope(),
        crate::scope::resolve_required_scope_ref(req.headers(), None),
    ) {
        envelope.note_owner_scope(scope.principal(), scope.workspace());
    }
}

/// `GET /api/magician/v2/budgets/envelope`
pub async fn envelope_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    note_owner_scope(&req);
    HttpResponse::Ok().json(envelope_body(&execution_budget_settings().spend_envelope))
}

/// `PUT /api/magician/v2/budgets/envelope` — partial body.
pub async fn envelope_put_handler(
    req: HttpRequest,
    body: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    note_owner_scope(&req);
    let update = match parse_envelope_update(&body.into_inner()) {
        Ok(update) => update,
        Err(message) => return bad_request("invalid_spend_envelope", message),
    };
    put_envelope_at(runtime_settings_paths().config_path, update).await
}

async fn put_envelope_at(path: PathBuf, update: EnvelopeUpdate) -> HttpResponse {
    match tokio::task::spawn_blocking(move || save_envelope(&path, &update)).await {
        Ok(Ok(settings)) => {
            // Let the attention items follow a raised, lowered or disabled
            // ceiling at once rather than at the next tick.
            if let Some(envelope) = spend_envelope::envelope() {
                envelope.reconcile_at(chrono::Utc::now().timestamp_millis());
            }
            HttpResponse::Ok().json(envelope_body(&settings.spend_envelope))
        },
        result => {
            tracing::error!(?result, "[BUDGETS] could not save the spend envelope");
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "spend_envelope_not_saved",
                "Could not save the spend envelope to magician-config.yaml.",
            )
        },
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeGrantRequest {
    add_cost_usd: f64,
    #[serde(default)]
    note: Option<String>,
}

/// `POST /api/magician/v2/budgets/envelope/grant` — `{ add_cost_usd, note? }`.
/// More money for the window the grant is made in; never unlimited.
pub async fn envelope_grant_handler(
    req: HttpRequest,
    body: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    note_owner_scope(&req);
    let request = match serde_json::from_value::<EnvelopeGrantRequest>(body.into_inner()) {
        Ok(request) => request,
        Err(why) => {
            return bad_request(
                "invalid_envelope_grant",
                format!("Send add_cost_usd (dollars) and, optionally, note ({why})."),
            )
        },
    };
    if let Err(message) = validate_grant_usd(request.add_cost_usd) {
        return bad_request("invalid_envelope_grant", message);
    }
    let Some(envelope) = spend_envelope::envelope() else {
        return error(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "spend_envelope_unavailable",
            "The spend envelope is not running in this process.",
        );
    };
    let Some(grants_path) = envelope.grants_path() else {
        return error(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "spend_envelope_unavailable",
            "The spend envelope has no runtime root to record grants in.",
        );
    };
    let now = chrono::Utc::now();
    let amount = request.add_cost_usd;
    let grant = EnvelopeGrant {
        granted_at: now,
        add_cost_usd: amount,
        source: Some(ENVELOPE_GRANT_SOURCE.to_string()),
        note: request
            .note
            .map(|note| note.trim().chars().take(200).collect::<String>())
            .filter(|note| !note.is_empty()),
    };
    match tokio::task::spawn_blocking(move || append_grant(&grants_path, grant)).await {
        Ok(Ok(_)) => {},
        result => {
            tracing::error!(?result, "[BUDGETS] spend envelope grant not recorded");
            return error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "envelope_grant_not_saved",
                "Could not record the spend envelope grant.",
            );
        },
    }
    let at_ms = now.timestamp_millis();
    envelope.record_grant(amount, at_ms, at_ms);
    // The ledger's model spend token gets the same top-up.
    magician::magician_v2::spend_envelope_ledger::credit_grant(amount).await;
    // Parked work resumes within minutes; the attention item goes now.
    envelope.reconcile_at(at_ms);
    HttpResponse::Ok().json(envelope_body(&execution_budget_settings().spend_envelope))
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
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
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
    let pause = pending.iter().find(|pending| {
        matches!(
            pending.pause_kind,
            Some(ExecutionPauseKind::Budget | ExecutionPauseKind::MaxIterations)
        )
    })?;
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
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
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
            ..ExecutionBudgetSettings::default()
        }
    }

    fn file_effective(contents: &str) -> ExecutionBudgetSettings {
        budgets_in(contents).unwrap().effective()
    }

    #[test]
    fn limits_are_written_to_resource_authority_without_touching_other_lines() {
        let before = budgets_in(LIVE_LIKE).unwrap();
        assert_eq!(before.model_spend, None, "a config from before the move");
        let next = write_budgets(
            LIVE_LIKE,
            &before,
            &settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(250.0)),
        )
        .unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&next).unwrap();
        let model = &parsed["resource_authority"]["model_spend"];
        assert_eq!(model["run_magician_usd"], serde_yaml::Value::from(15));
        assert_eq!(model["run_harness_usd"], serde_yaml::Value::from(250));
        assert_eq!(model["window_usd"], serde_yaml::Value::from(250));
        assert_eq!(model["lifetime_usd"], "unlimited");
        assert!(next.contains("# operator note above execution\n"));
        assert!(next.contains("  # the loop's engine\n"));
        assert_eq!(parsed["chat"]["harness_engine"], "magician");
        assert_eq!(parsed["execution"]["file_sandbox"]["allowed_roots"][0], ".");
        assert!(parsed["execution"].get("budgets").is_none(), "{next}");
        assert_eq!(
            file_effective(&next),
            settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(250.0))
        );
    }

    #[test]
    fn a_config_from_before_the_move_keeps_its_limits() {
        let legacy = "execution:\n  budgets:\n    native_max_cost_usd: 7.5\n    harness_max_cost_usd: unlimited\n    spend_envelope:\n      daily_usd: 40\n";
        let effective = file_effective(legacy);
        assert_eq!(effective.native_max_cost_usd, BudgetCapUsd::Usd(7.5));
        assert_eq!(effective.harness_max_cost_usd, BudgetCapUsd::Unlimited);
        assert_eq!(effective.spend_envelope.daily_usd, BudgetCapUsd::Usd(40.0));
        // Once resource authority holds them, it wins over the legacy keys.
        let moved = format!(
            "{legacy}resource_authority:\n  model_spend:\n    window_usd: 90\n    run_magician_usd: 3\n"
        );
        let effective = file_effective(&moved);
        assert_eq!(effective.native_max_cost_usd, BudgetCapUsd::Usd(3.0));
        assert_eq!(effective.spend_envelope.daily_usd, BudgetCapUsd::Usd(90.0));
        assert_eq!(
            effective.harness_max_cost_usd,
            BudgetCapUsd::Usd(100.0),
            "section default"
        );
    }

    #[test]
    fn a_limit_change_keeps_the_owners_comments_inside_the_section() {
        let contents = "resource_authority:\n  enabled: true\n  model_spend:\n    # agreed with finance\n    window_usd: 250  # a day\n    window: daily\n    lifetime_usd: unlimited\n    run_magician_usd: 15\n    run_harness_usd: 100\nchat:\n  x: 1\n";
        let before = budgets_in(contents).unwrap();
        let next = write_budgets(
            contents,
            &before,
            &settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Unlimited),
        )
        .unwrap();
        assert_eq!(
            next,
            contents.replace("run_harness_usd: 100", "run_harness_usd: unlimited")
        );
        let unchanged = write_budgets(
            contents,
            &before,
            &settings(BudgetCapUsd::Usd(15.0), BudgetCapUsd::Usd(100.0)),
        )
        .unwrap();
        assert_eq!(unchanged, contents);
    }

    fn rows_in(contents: &str) -> Vec<BudgetRow> {
        resource_authority_in(contents).unwrap().budgets
    }

    const LIMITS: &str = "resource_authority:\n  enabled: true\n  budgets:\n    # Email cap: edit with the skill doc\n    - scope: tool\n      id: agentmail-send\n      commodity: EMAIL_SENDS\n      ceiling: \"50\"   # per day\n      period: daily\n      carryover: { type: none }\n    # Rupee caps\n    - scope: tool\n      id: zepto-mcp\n      commodity: INR\n      ceiling: \"2000\"\n      period: daily\n      carryover: { type: none }\nchat:\n  x: 1\n";

    #[test]
    fn an_action_limit_edit_touches_only_its_row() {
        let before = rows_in(LIMITS);
        let mut after = before.clone();
        after[0].ceiling = rust_decimal::Decimal::from(80);
        let next = edit_limit_rows(LIMITS, &before, &after).expect("row edit");
        assert_eq!(
            next,
            LIMITS.replace("ceiling: \"50\"   # per day", "ceiling: '80'   # per day")
        );
        assert_eq!(
            serde_json::to_value(rows_in(&next)).unwrap(),
            serde_json::to_value(&after).unwrap()
        );

        // A removal drops the row's lines only; the comment above the next
        // row stays.
        let removed = vec![before[1].clone()];
        let next = edit_limit_rows(LIMITS, &before, &removed).expect("row removal");
        assert!(next.contains("    # Email cap: edit with the skill doc\n    # Rupee caps\n    - scope: tool\n      id: zepto-mcp"), "{next}");
        assert_eq!(rows_in(&next).len(), 1);

        // An addition is appended after the last row, comments intact.
        let mut added = before.clone();
        added.push(BudgetRow {
            scope: BudgetScope::Tool,
            id: "whatsapp-send".into(),
            commodity: "WHATSAPP_SENDS".into(),
            ceiling: rust_decimal::Decimal::from(25),
            period: CeilingPeriod::Daily,
            carryover: CarryoverPolicy::None,
            system_ceiling_id: None,
        });
        let next = edit_limit_rows(LIMITS, &before, &added).expect("row addition");
        assert!(next.contains("    # Rupee caps\n"), "{next}");
        assert!(next.ends_with("chat:\n  x: 1\n"), "{next}");
        assert_eq!(
            serde_json::to_value(rows_in(&next)).unwrap(),
            serde_json::to_value(&added).unwrap()
        );

        // A flow-style list is left to the whole-list writer.
        let flow = "resource_authority:\n  budgets: []\n";
        assert!(edit_limit_rows(flow, &[], &added[..1]).is_none());
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
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(file_effective(&text), saved);
        assert_eq!(
            execution_settings_in(&text).unwrap().harness_engine,
            "magician"
        );
        let body = defaults_body(&execution_budget_settings());
        assert_eq!(body["native_max_cost_usd"], json!("unlimited"));
        assert_eq!(body["harness_max_cost_usd"], json!(250.0));

        configure_execution_budgets(&previous);
    }

    #[test]
    fn envelope_updates_are_partial_and_validated() {
        assert_eq!(
            parse_envelope_update(&json!({"daily_usd": 40, "window": "Weekly"})),
            Ok(EnvelopeUpdate {
                daily_usd: Some(BudgetCapUsd::Usd(40.0)),
                window: Some(SpendEnvelopeWindow::Weekly),
                ..Default::default()
            })
        );
        assert_eq!(
            parse_envelope_update(&json!({
                "enabled": false,
                "total_usd": "unlimited",
                "reset_offset_minutes": -420,
                "warn_at_pct": 90,
                "unpriced": "block"
            })),
            Ok(EnvelopeUpdate {
                enabled: Some(false),
                total_usd: Some(BudgetCapUsd::Unlimited),
                reset_offset_minutes: Some(-420),
                warn_at_pct: Some(90),
                unpriced: Some(UnpricedPolicy::Block),
                ..Default::default()
            })
        );
        for bad in [
            json!({}),
            json!([1]),
            json!({"daily_usd": 0}),
            json!({"daily_usd": -5}),
            json!({"daily_usd": "lots"}),
            json!({"total_usd": null}),
            json!({"enabled": "yes"}),
            json!({"window": "yearly"}),
            json!({"window": 7}),
            json!({"reset_offset_minutes": 1440}),
            json!({"reset_offset_minutes": 1.5}),
            json!({"warn_at_pct": 0}),
            json!({"warn_at_pct": 101}),
            json!({"unpriced": "guess"}),
            json!({"daily_usd": 250, "bogus": 1}),
        ] {
            assert!(
                parse_envelope_update(&bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn a_partial_envelope_put_round_trips_through_the_config_file() {
        let _budgets = magician::config::execution_budget_test_lock();
        let previous = execution_budget_settings();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("magician-config.yaml");
        let original = "execution:\n  harness_engine: magician\n  # per-run valves\n  budgets:\n    native_max_cost_usd: 15\n    harness_max_cost_usd: 100\nchat:\n  x: 1\n";
        std::fs::write(&path, original).unwrap();

        // Live config says nothing about the envelope: defaults apply.
        let defaults = execution_settings_in(original)
            .unwrap()
            .budgets
            .spend_envelope;
        assert_eq!(defaults, SpendEnvelopeSettings::default());
        assert!(defaults.enabled);

        let saved = save_envelope(
            &path,
            &parse_envelope_update(
                &json!({"daily_usd": 40, "window": "weekly", "unpriced": "block"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(saved.spend_envelope.daily_usd, BudgetCapUsd::Usd(40.0));
        assert_eq!(saved.spend_envelope.window, SpendEnvelopeWindow::Weekly);
        assert_eq!(saved.spend_envelope.unpriced, UnpricedPolicy::Block);
        assert!(
            saved.spend_envelope.enabled,
            "untouched fields keep their value"
        );
        assert_eq!(saved.native_max_cost_usd, BudgetCapUsd::Usd(15.0));
        assert_eq!(saved.harness_max_cost_usd, BudgetCapUsd::Usd(100.0));
        assert_eq!(execution_budget_settings(), saved, "published live");

        // Limits went to resource authority, behaviour to execution.budgets;
        // the owner's lines survived.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("  # per-run valves\n"), "{text}");
        let parsed: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        let model = &parsed["resource_authority"]["model_spend"];
        assert_eq!(model["window_usd"], serde_yaml::Value::from(40), "{text}");
        assert_eq!(model["window"], "weekly", "{text}");
        assert_eq!(
            model["run_magician_usd"],
            serde_yaml::Value::from(15),
            "{text}"
        );
        assert_eq!(
            parsed["execution"]["budgets"]["spend_envelope"]["unpriced"], "block",
            "{text}"
        );
        assert!(!text.contains("warn_at_pct"), "{text}");
        assert!(text.contains("chat:\n  x: 1\n"), "{text}");

        // A second partial PUT keeps the first one's fields.
        let saved = save_envelope(
            &path,
            &parse_envelope_update(&json!({"enabled": false, "total_usd": 5000})).unwrap(),
        )
        .unwrap();
        assert!(!saved.spend_envelope.enabled);
        assert_eq!(saved.spend_envelope.total_usd, BudgetCapUsd::Usd(5000.0));
        assert_eq!(saved.spend_envelope.daily_usd, BudgetCapUsd::Usd(40.0));
        assert_eq!(saved.spend_envelope.window, SpendEnvelopeWindow::Weekly);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(file_effective(&text), saved);
        assert_eq!(
            execution_settings_in(&text).unwrap().harness_engine,
            "magician"
        );

        // GET reads what PUT wrote.
        let body = envelope_body(&execution_budget_settings().spend_envelope);
        assert_eq!(body["settings"]["enabled"], json!(false));
        assert_eq!(body["settings"]["daily_usd"], json!(40.0));
        assert_eq!(body["settings"]["total_usd"], json!(5000.0));
        assert_eq!(body["settings"]["window"], json!("weekly"));
        assert_eq!(body["settings"]["warn_at_pct"], json!(80));
        assert_eq!(
            body["installed"],
            json!(false),
            "no envelope in a unit test"
        );

        // A PUT that changes nothing leaves the file as it was.
        let before = std::fs::read_to_string(&path).unwrap();
        let again = save_envelope(
            &path,
            &parse_envelope_update(&json!({"window": "weekly"})).unwrap(),
        )
        .unwrap();
        assert_eq!(again, saved);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

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
        budget.pause_kind = Some(ExecutionPauseKind::MaxIterations);
        assert!(budget_pause_continue_request(&[budget]).is_some());
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
