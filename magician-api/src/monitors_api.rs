//! Recurring Monitors (Phase 1) — scoped `/api/magician/v3/monitors` CRUD.
//!
//! Plan: `docs/plans/2026-07-21-recurring-monitors-productization-design-implementation.md`
//! (§6.1 task specialization, §8 API surface). A Monitor IS an existing
//! persistent task + `Task.schedule` + a validated `manifest.monitor_spec` —
//! these routes are thin projections/mutations over the SAME
//! `ArtifactV2Service` paths the `/v3/tasks` endpoints use. No second
//! scheduler, store, or execution runtime:
//!
//! * create   → `create_task` + `update_task` (spec attach bumps the
//!   server-owned `monitor_revision` to 1)
//! * edit     → `update_task` (spec arm increments the revision)
//! * pause/resume → mutate `TaskSchedule.paused` through `update_task`
//! * run-now  → the exact `start_execution` path `execute_task_v3_handler` uses
//! * delete   → the exact `archive_task_with_options` path
//!   `delete_task_v3_handler` uses
//!
//! The reserved `system:monitor` tag is PROJECTED into detail reads for
//! filtering convenience only — it is never persisted and never the source
//! of truth (plan §6.1). List pages use the canonical cursor envelope
//! `{items, next_cursor, limit, total, offset}` pinned by
//! `magician/tests/fixtures/monitors/monitor_list_page_v1.json`.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::scope::resolve_required_scope_ref;
use crate::task_api_v3::{
    map_result, validate_task_agent_assignment, DeleteTaskV3Query, ExecuteTaskV3Request,
    ExecutionResponseV3, ScopeQuery, TaskApiV3,
};
use magician::magician_v2::artifact_v2::{
    models::{TaskLifecycle, TaskRecord, TaskTagRecord},
    ArtifactV2Error, ArtifactV2Service, ScopeRef, UpdateTaskInput, V3ReadApi,
};
use magician::magician_v2::execution::agent_resources::AgentResources;
pub use magician::magician_v2::monitor_support::{
    cadence_summary, create_monitor_task, monitor_state_label, monitor_title_from, parsed_schedule,
    record_monitor_updated_event, schedule_is_paused, DEFAULT_MONITOR_AGENT_ID,
};
use magician::magician_v2::monitors::{
    monitor_feedback::{MonitorFeedbackVerdict, MonitorUpdateFeedbackV1},
    monitor_run::{would_notify, MonitorRunResultV1, MonitorRunStatus, MonitorSourceOutcomeStatus},
    monitor_spec::{validate_and_normalize, MonitorSpecV1},
    monitor_updates::MonitorUpdateDetailV1,
};
use magician::magician_v2::storage::{MonitorPageQuery, TaskSchedule};

/// Cursors are emitted with this prefix (`cur_task_…`, matching the Phase 0
/// list fixture) and accepted with or without it.
const MONITOR_CURSOR_PREFIX: &str = "cur_";

/// Reserved read-time tag projected into monitor detail responses.
/// NEVER persisted — `manifest.monitor_spec` is the source of truth.
const MONITOR_PROJECTED_TAG: &str = "system:monitor";

const MONITOR_LIST_DEFAULT_LIMIT: usize = 50;
const MONITOR_LIST_MAX_LIMIT: usize = 200;
/// Phase 6 metrics window: rates are computed over the newest N accepted
/// run records PER MONITOR (read from the existing change ledger on every
/// request — no new store, plan §12 aggregate observability).
pub const MONITOR_METRICS_RUN_WINDOW: usize = 50;

#[derive(Debug, Deserialize)]
pub struct CreateMonitorV3Request {
    pub workspace: Option<String>,
    pub title: Option<String>,
    pub spec: MonitorSpecV1,
    pub schedule: Option<serde_json::Value>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMonitorV3Request {
    pub workspace: Option<String>,
    pub title: Option<String>,
    pub spec: Option<MonitorSpecV1>,
    pub schedule: Option<serde_json::Value>,
}

/// `POST /monitors/{task_id}/convert` body — the FIXED Phase 7 conversion
/// contract: `{"spec": MonitorSpecV1, "title"?: string}`. Conversion is
/// USER-EXPLICIT only (plan Phase 7: never infer monitors from title text —
/// no heuristics exist anywhere on this path).
#[derive(Debug, Deserialize)]
pub struct ConvertMonitorV3Request {
    pub workspace: Option<String>,
    pub title: Option<String>,
    pub spec: MonitorSpecV1,
}

#[derive(Debug, Deserialize)]
pub struct MonitorListQueryV3 {
    pub workspace: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    /// `active` (schedule not paused, incl. unscheduled) | `paused`.
    pub state: Option<String>,
}

/// One row of `GET /monitors` — shape pinned by
/// `tests/fixtures/monitors/monitor_list_page_v1.json` and mirrored in
/// `ui/unified-ui/src/lib/types/monitor.ts` (`MonitorListItemV1`).
/// `next_run_at` is intentionally absent in Phase 1: next-fire state is
/// in-memory scheduler state (Phase 2+ surfaces it). `Deserialize` exists
/// for the fixture contract check, not for any write path.
#[derive(Debug, Serialize, Deserialize)]
pub struct MonitorListItemV3 {
    pub task_id: String,
    pub title: String,
    pub objective: String,
    pub state: String,
    pub cadence_summary: String,
    pub monitor_revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    pub last_run_status: String,
    pub health: String,
}

/// Canonical cursor page envelope (plan §8 rule 1): `{items, next_cursor,
/// limit, total, offset}` — `next_cursor` serializes as `null` on the last
/// page.
///
/// `total` and `offset` are ADDITIVE. The cursor keeps its meaning exactly —
/// the previous page's last `task_id`, over `updated_at` desc then
/// `task_id` — and next/prev still walk by cursor. What the two new fields
/// buy is the one thing a cursor cannot express: how many pages there are,
/// and which one the reader is on. Without them a pager can only offer
/// "next", which is why this surface had a different control from the two
/// task tabs.
#[derive(Debug, Serialize, Deserialize)]
pub struct MonitorListPageV3 {
    pub items: Vec<MonitorListItemV3>,
    pub next_cursor: Option<String>,
    pub limit: usize,
    /// Monitors in the scope after the `state` filter and before this page —
    /// the size of the corpus being paged, never the size of the page. A
    /// total that shrank with each page would report one page of results
    /// however many there are.
    pub total: usize,
    /// Where this page starts in that corpus. It is the position the cursor
    /// RESOLVED to, not a number the caller sent: `/monitors` takes no
    /// `offset` parameter, and a stale cursor resolves to `total` — an empty
    /// last page rather than an error.
    pub offset: usize,
}

/// `map_result` never returns `Err`, but stay total for helper call-sites
/// that need a bare `HttpResponse`.
fn v3_error_response(error: ArtifactV2Error) -> HttpResponse {
    map_result(Err(error)).unwrap_or_else(|error| {
        HttpResponse::InternalServerError().json(json!({ "error": error.to_string() }))
    })
}

fn monitor_not_found(task_id: &str) -> HttpResponse {
    HttpResponse::NotFound().json(json!({
        "error": "monitor_not_found",
        "task_id": task_id
    }))
}

/// Load a task and require it to be a monitor (`manifest.monitor_spec`
/// present). Missing task → the canonical 404 `task_not_found`; a plain task
/// reached through a monitor route → 404 `monitor_not_found` so generic
/// tasks are never mutated or exposed here.
async fn load_monitor_task(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    task_id: &str,
) -> std::result::Result<TaskRecord, HttpResponse> {
    match service.get_task(scope, task_id).await {
        // An archived monitor is DELETED as far as monitor surfaces go: the
        // soft archive keeps the task dir (restorable, dedupe ledger intact)
        // but every /monitors route must treat it as gone. The generic task
        // list is feed-projection-backed and hides archived tasks that way;
        // the raw-directory monitor walk needs this explicit gate.
        Ok(task) if task.manifest.monitor_spec.is_some() && task.state.status != "archived" => {
            Ok(task)
        },
        Ok(_) => Err(monitor_not_found(task_id)),
        Err(error) => Err(v3_error_response(error)),
    }
}

/// Build one list row from the task record alone — deliberately CHEAP
/// (plan §6 principle 6: no per-row artifact/execution reads). Until the
/// Phase 2 run-result ledger exists, `last_run_*` derive from `TaskState`
/// only: a completed root execution reads as `unchanged` (no change ledger
/// yet), a failed task as `failed`, and `last_run_at` approximates the last
/// run with `state.updated_at`.
fn monitor_list_item(task: &TaskRecord) -> MonitorListItemV3 {
    let objective = task
        .manifest
        .monitor_spec
        .as_ref()
        .map(|spec| spec.objective.clone())
        .unwrap_or_default();
    let schedule = parsed_schedule(task.manifest.schedule.as_ref());
    let paused = schedule_is_paused(schedule.as_ref());
    let has_completed_run = task.state.last_completed_root_execution_id.is_some();
    let last_run_status = if task.state.status == "failed" {
        "failed"
    } else if has_completed_run {
        "unchanged"
    } else {
        "never_ran"
    };
    MonitorListItemV3 {
        task_id: task.manifest.task_id.clone(),
        title: task.manifest.title.clone(),
        objective,
        state: monitor_state_label(paused).to_string(),
        cadence_summary: cadence_summary(schedule.as_ref()),
        monitor_revision: task.manifest.monitor_revision,
        last_run_at: has_completed_run.then(|| task.state.updated_at.clone()),
        last_run_status: last_run_status.to_string(),
        health: if task.state.status == "failed" {
            "needs_attention".to_string()
        } else {
            "ok".to_string()
        },
    }
}

/// `POST /api/magician/v3/monitors` — validate + normalize the spec at
/// admission (400 with the stable reason on failure), mint a persistent task
/// through the canonical create path, then attach the spec through
/// `update_task` so the server-owned `monitor_revision` starts at 1.
pub async fn create_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    resources: Option<web::Data<Arc<AgentResources>>>,
    http_req: HttpRequest,
    req: web::Json<CreateMonitorV3Request>,
) -> Result<HttpResponse> {
    let request = req.into_inner();
    let scope = match resolve_required_scope_ref(http_req.headers(), request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let mut spec = request.spec;
    if let Err(reason) = validate_and_normalize(&mut spec) {
        return Ok(HttpResponse::BadRequest().json(json!({ "error": reason })));
    }

    // Same fallback create_task_v3_handler resolves to; the VibeDev
    // configured-lead override can never apply to a monitor.
    let agent_id = request
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MONITOR_AGENT_ID)
        .to_string();
    // Reuse the shared task-surface agent validation whenever the
    // `AgentResources` app-data is installed (always true in the server
    // binary; absent in lightweight handler tests).
    if let Some(resources) = resources.as_ref() {
        if let Err(response) = validate_task_agent_assignment(resources, &scope, &agent_id).await {
            return Ok(response);
        }
    }

    let service = api.service();
    let created = create_monitor_task(
        &service,
        &scope,
        request.title,
        spec,
        request.schedule,
        agent_id,
    )
    .await;
    map_result(created.map(|task| {
        HttpResponse::Created().json(json!({
            "task_id": task.manifest.task_id,
            "monitor_revision": task.manifest.monitor_revision,
        }))
    }))
}

/// §12 `monitor_created` metadata for the Phase 7 conversion seam — the
/// SAME trace the create path emits, plus `converted: true` so
/// observability distinguishes a converted existing task from a freshly
/// minted monitor. Pure so the flag is unit-testable (the funnel store's
/// observability read does not expose event metadata).
pub(crate) fn converted_monitor_event_payload(task: &TaskRecord) -> serde_json::Value {
    json!({
        "monitor_revision": task.manifest.monitor_revision,
        "has_schedule": task.manifest.schedule.is_some(),
        "agent_id": task.manifest.agent_id,
        "converted": true,
    })
}

/// `POST /api/magician/v3/monitors/{task_id}/convert` — explicit conversion
/// of an EXISTING task into a monitor (plan Phase 7). The task keeps its id,
/// schedule, fire history, executions, and outputs: conversion only
/// validates + attaches the spec through the SAME `update_task` spec arm the
/// create path uses (server-owned `monitor_revision` starts at 1), optionally
/// retitles, and emits `monitor_created` with `converted: true`. The task
/// then appears on `/monitors` like any other monitor.
///
/// Contract (all three clients build against exactly this):
/// * `200 {task_id, monitor_revision: 1, converted: true}`
/// * `404 task_not_found` — no such task (this route addresses a TASK, so
///   the canonical task 404, not `monitor_not_found`)
/// * `409 monitor_already_exists` — the task already carries a spec,
///   INCLUDING a soft-archived former monitor (archive keeps the manifest,
///   so its spec + dedupe ledger survive restore; re-converting would reset
///   the server-owned revision lineage)
/// * `409 task_not_eligible_for_monitor` — Internal-lifecycle tasks
///   (chat/runtime transients) and archived tasks. A MISSING schedule is
///   allowed: the converted monitor is simply run-on-demand.
/// * `400 monitor_*` — the standard Phase 1 admission reasons.
pub async fn convert_task_to_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    req: web::Json<ConvertMonitorV3Request>,
) -> Result<HttpResponse> {
    let request = req.into_inner();
    let scope = match resolve_required_scope_ref(http_req.headers(), request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    let task = match service.get_task(&scope, &task_id).await {
        Ok(task) => task,
        Err(error) => return map_result(Err(error)),
    };
    // Spec presence FIRST, unconditionally — an archived former monitor is
    // `monitor_already_exists`, never "eligible again".
    if task.manifest.monitor_spec.is_some() {
        return Ok(HttpResponse::Conflict().json(json!({
            "error": "monitor_already_exists",
            "task_id": task_id,
        })));
    }
    if task.manifest.lifecycle == TaskLifecycle::Internal || task.state.status == "archived" {
        return Ok(HttpResponse::Conflict().json(json!({
            "error": "task_not_eligible_for_monitor",
            "task_id": task_id,
        })));
    }
    let mut spec = request.spec;
    if let Err(reason) = validate_and_normalize(&mut spec) {
        return Ok(HttpResponse::BadRequest().json(json!({ "error": reason })));
    }
    // Optional retitle; blank/whitespace titles keep the existing one.
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let converted = service
        .update_task(
            &scope,
            &task_id,
            UpdateTaskInput {
                title,
                monitor_spec: Some(spec),
                ..Default::default()
            },
        )
        .await;
    if let Ok(task) = converted.as_ref() {
        // Deterministic seed = the task id (same as the create seam): a task
        // converts at most once (the spec gate above makes a second convert
        // 409), and the funnel row's INSERT OR IGNORE makes any replay of
        // the same action emit nothing new.
        service
            .record_monitor_lifecycle_event(
                &scope,
                "monitor_created",
                &task.manifest.task_id,
                &task.manifest.task_id,
                converted_monitor_event_payload(task),
            )
            .await;
    }
    map_result(converted.map(|task| {
        HttpResponse::Ok().json(json!({
            "task_id": task.manifest.task_id,
            "monitor_revision": task.manifest.monitor_revision,
            "converted": true,
        }))
    }))
}

/// `GET /api/magician/v3/monitors?limit=&cursor=&state=` — scoped,
/// server-paginated monitor list in the canonical `{items, next_cursor,
/// limit, total, offset}` envelope. Sorted `updated_at` desc then `task_id`;
/// the cursor is the previous page's last `task_id` (accepted with or
/// without the `cur_` prefix, always emitted with it). A stale cursor (row
/// deleted between pages) terminates pagination with an empty page rather
/// than erroring.
///
/// **Served from the storage index when one is ready**, as a `COUNT(*)` plus
/// a keyset seek on `list_keyset_id_asc`; otherwise from the walk below,
/// which loads every task in the scope and scans it for the cursor's row.
/// Both produce the same order, the same `total`/`offset`, and the same
/// empty page for a stale cursor — the order and the tiebreak are the
/// contract, not the mechanism.
pub async fn list_monitors_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<MonitorListQueryV3>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let limit = query
        .limit
        .unwrap_or(MONITOR_LIST_DEFAULT_LIMIT)
        .clamp(1, MONITOR_LIST_MAX_LIMIT);
    let paused_filter = match query
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => None,
        Some("active") => Some(false),
        Some("paused") => Some(true),
        Some(_) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "monitor_state_filter_invalid"
            })));
        },
    };

    let cursor_task_id = query
        .cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .strip_prefix(MONITOR_CURSOR_PREFIX)
                .unwrap_or(value)
                .to_string()
        });

    let service = api.service();
    // The index answers this exact question — same order, same tiebreak,
    // same stale-cursor behaviour — or it declines and the walk below runs.
    // Declining is always safe: the files are the source of truth and the
    // index is a cache that may be deleted at any moment.
    if let Some(response) = indexed_monitor_page(
        &service,
        &scope,
        paused_filter,
        cursor_task_id.as_deref(),
        limit,
    )
    .await
    {
        return Ok(response);
    }

    let listed = match service.list_tasks(&scope).await {
        Ok(listed) => listed,
        Err(error) => return map_result(Err(error)),
    };
    // The user-visible listing carries projections, not manifests, so the
    // monitor discriminator (`manifest.monitor_spec`) needs the task record.
    // Kept to record reads only — no per-row execution/artifact lookups.
    let mut monitors: Vec<TaskRecord> = Vec::new();
    for item in listed {
        let task = match service.get_task(&scope, &item.id).await {
            Ok(task) => task,
            // Deleted between the directory walk and this read — skip.
            Err(ArtifactV2Error::TaskNotFound(_)) => continue,
            Err(error) => return map_result(Err(error)),
        };
        if task.manifest.monitor_spec.is_none() {
            continue;
        }
        // Soft-deleted (archived) monitors are gone from monitor surfaces —
        // same gate as load_monitor_task.
        if task.state.status == "archived" {
            continue;
        }
        if let Some(paused_filter) = paused_filter {
            let schedule = parsed_schedule(task.manifest.schedule.as_ref());
            if schedule_is_paused(schedule.as_ref()) != paused_filter {
                continue;
            }
        }
        monitors.push(task);
    }
    // Deterministic total order for stable cursors: updated_at desc, then
    // task_id as the tiebreak.
    monitors.sort_by(|left, right| {
        right
            .state
            .updated_at
            .cmp(&left.state.updated_at)
            .then_with(|| left.manifest.task_id.cmp(&right.manifest.task_id))
    });

    let start = match cursor_task_id.as_deref() {
        None => 0,
        Some(cursor_task_id) => monitors
            .iter()
            .position(|task| task.manifest.task_id == cursor_task_id)
            .map(|position| position + 1)
            // Stale cursor → end of pagination, not an error.
            .unwrap_or(monitors.len()),
    };
    // The corpus this page is a window onto — after the `state` filter, and
    // never reduced by paging. Read before the slice so a later change to
    // the windowing cannot quietly turn it into the page size.
    let total = monitors.len();
    let offset = start.min(total);
    let items: Vec<MonitorListItemV3> = monitors[offset..]
        .iter()
        .take(limit)
        .map(monitor_list_item)
        .collect();
    let next_cursor = if offset + items.len() < total {
        items
            .last()
            .map(|item| format!("{MONITOR_CURSOR_PREFIX}{}", item.task_id))
    } else {
        None
    };

    Ok(HttpResponse::Ok().json(MonitorListPageV3 {
        items,
        next_cursor,
        limit,
        total,
        offset,
    }))
}

/// Serve one `/monitors` page out of the storage index, or decline.
///
/// `None` means "the index cannot answer this" — it was never wired in, it
/// is mid-rebuild, or a query failed — and the caller falls back to the walk.
///
/// The readiness check is the important one. `is_ready()` is false from the
/// moment a rebuild starts until it finishes, INCLUDING across a crash,
/// because the marker is on disk. Serving a half-built index would not error
/// or look empty; it would look like a complete index holding fewer
/// monitors, which is the one failure here a reader could not detect.
///
/// **Nothing about the wire contract moves.** Same order (`updated_at` desc,
/// `task_id` asc), same bare-`task_id` cursor, same stale-cursor behaviour,
/// same `{items, next_cursor, limit, total, offset}`. What changes is that
/// `total` and the page are a `COUNT(*)` and a keyset seek instead of a load
/// of every task in the scope followed by a linear scan for the cursor row.
async fn indexed_monitor_page(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    paused: Option<bool>,
    cursor_task_id: Option<&str>,
    limit: usize,
) -> Option<HttpResponse> {
    let index = service.list_index()?;
    if !index.is_ready().unwrap_or(false) {
        return None;
    }

    let page = match index.monitor_page(&MonitorPageQuery {
        scope: ArtifactV2Service::list_scope(scope),
        paused,
        cursor_task_id: cursor_task_id.map(ToOwned::to_owned),
        limit,
    }) {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "[LIST-INDEX] Monitor page query failed; falling back to the file walk"
            );
            return None;
        },
    };

    // The index holds ids; every field a reader sees is built from the record
    // on disk, so a stale cache can never put a value in front of them that
    // the file does not carry. Bounded by `limit` — this is the read the
    // walk did for the WHOLE scope.
    let mut items: Vec<MonitorListItemV3> = Vec::with_capacity(page.ids.len());
    for task_id in &page.ids {
        match service.get_task(scope, task_id).await {
            Ok(task) if task.manifest.monitor_spec.is_some() && task.state.status != "archived" => {
                items.push(monitor_list_item(&task));
            },
            // Deleted, archived, or no longer a monitor since its row was
            // written. Skipped, exactly as the walk skipped a task that
            // vanished between the directory read and the record read.
            Ok(_) => {},
            Err(ArtifactV2Error::TaskNotFound(_)) => {},
            Err(error) => {
                tracing::warn!(
                    task_id = %task_id,
                    error = %error,
                    "[LIST-INDEX] Loading an indexed monitor page's records failed; \
                     falling back to the file walk"
                );
                return None;
            },
        }
    }

    // Minted from the INDEX's last row, not from the last row that survived
    // the record read. A skipped row is still a position the page reached;
    // resuming before it would serve it again on the next page.
    let next_cursor = if page.offset + page.ids.len() < page.total {
        page.ids
            .last()
            .map(|task_id| format!("{MONITOR_CURSOR_PREFIX}{task_id}"))
    } else {
        None
    };

    Some(HttpResponse::Ok().json(MonitorListPageV3 {
        items,
        next_cursor,
        limit,
        total: page.total,
        offset: page.offset,
    }))
}

/// `GET /api/magician/v3/monitors/{task_id}` — monitor detail. 404 for a
/// missing task or a plain (non-monitor) task. The `tags` array carries the
/// PROJECTED reserved `system:monitor` tag alongside any persisted tag
/// names — the projection is read-time only (plan §6.1).
pub async fn get_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let task = match load_monitor_task(&api.service(), &scope, &task_id).await {
        Ok(task) => task,
        Err(response) => return Ok(response),
    };
    let mut tags: Vec<String> = task
        .manifest
        .tags
        .iter()
        .map(|tag: &TaskTagRecord| tag.name.clone())
        .collect();
    if !tags.iter().any(|tag| tag == MONITOR_PROJECTED_TAG) {
        tags.push(MONITOR_PROJECTED_TAG.to_string());
    }
    Ok(HttpResponse::Ok().json(json!({
        "task_id": task.manifest.task_id,
        "title": task.manifest.title,
        "spec": task.manifest.monitor_spec,
        "monitor_revision": task.manifest.monitor_revision,
        "schedule": task.manifest.schedule,
        "state": {
            "status": task.state.status,
            "schedule_fire_count": task.state.schedule_fire_count,
        },
        "created_at": task.manifest.created_at,
        "updated_at": task.manifest.updated_at,
        "tags": tags,
    })))
}

/// `PATCH /api/magician/v3/monitors/{task_id}` — edit title/spec/schedule.
/// A spec edit routes through the `update_task` monitor arm, which bumps the
/// server-owned `monitor_revision` (plan §6.1 rule 5).
pub async fn update_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    req: web::Json<UpdateMonitorV3Request>,
) -> Result<HttpResponse> {
    let request = req.into_inner();
    let scope = match resolve_required_scope_ref(http_req.headers(), request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let spec = match request.spec {
        Some(mut spec) => {
            if let Err(reason) = validate_and_normalize(&mut spec) {
                return Ok(HttpResponse::BadRequest().json(json!({ "error": reason })));
            }
            Some(spec)
        },
        None => None,
    };
    let updated = service
        .update_task(
            &scope,
            &task_id,
            UpdateTaskInput {
                title: request.title,
                // Set-only in Phase 1: a PATCH carrying `schedule` replaces
                // it; clearing a schedule is not a monitor operation.
                schedule: request.schedule.map(Some),
                monitor_spec: spec,
                ..Default::default()
            },
        )
        .await;
    if let Ok(task) = updated.as_ref() {
        record_monitor_updated_event(&service, &scope, task).await;
    }
    map_result(updated.map(|task| {
        HttpResponse::Ok().json(json!({
            "task_id": task.manifest.task_id,
            "monitor_revision": task.manifest.monitor_revision,
        }))
    }))
}

/// `DELETE /api/magician/v3/monitors/{task_id}` — 404 for non-monitors,
/// otherwise the exact archive path `delete_task_v3_handler` uses (same
/// query contract incl. `remove_files`, same response shape).
pub async fn delete_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<DeleteTaskV3Query>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let response = service
        .archive_task_with_options(&scope, &task_id, query.remove_files)
        .await;
    // The generic soft archive removes the FEED summary but leaves
    // `state.status` untouched (the feed-backed task list hides it; the
    // raw-directory monitor walk would not). Stamp the terminal status so
    // list_monitors/load_monitor_task treat the monitor as deleted while the
    // task dir stays restorable. Physical deletes have no record left to
    // stamp; a stamp failure after archive is best-effort surfaced as an
    // error since the monitor would otherwise remain visible.
    if response.is_ok() && !query.remove_files {
        if let Err(error) = service
            .update_task_status(&scope, &task_id, "archived")
            .await
        {
            return map_result(Err(error));
        }
    }
    map_result(response.map(|_| {
        HttpResponse::Ok().json(json!({
            "ok": true,
            "task_id": task_id,
            "files_removed": query.remove_files,
        }))
    }))
}

/// Shared pause/resume body: mutate `TaskSchedule.paused` through the
/// central `update_task` path (plan §8 rule 3) — the scheduler's hydration
/// sync honours the flag, so no scheduler-side surgery happens here.
async fn set_monitor_schedule_paused(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    paused: bool,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    let task = match load_monitor_task(&service, &scope, &task_id).await {
        Ok(task) => task,
        Err(response) => return Ok(response),
    };
    let Some(schedule_value) = task.manifest.schedule else {
        return Ok(HttpResponse::Conflict().json(json!({
            "error": "monitor_unscheduled",
            "task_id": task_id,
        })));
    };
    let mut schedule: TaskSchedule = match serde_json::from_value(schedule_value) {
        Ok(schedule) => schedule,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "monitor_schedule_invalid",
                "task_id": task_id,
            })));
        },
    };
    schedule.paused = Some(paused);
    let schedule_value = match serde_json::to_value(&schedule) {
        Ok(value) => value,
        Err(error) => return map_result(Err(ArtifactV2Error::Serde(error))),
    };
    let updated = service
        .update_task(
            &scope,
            &task_id,
            UpdateTaskInput {
                schedule: Some(Some(schedule_value)),
                ..Default::default()
            },
        )
        .await;
    // §12 `monitor_paused` / `monitor_resumed` — once per successful toggle
    // (the manifest timestamp in the seed distinguishes repeated toggles).
    if let Ok(task) = updated.as_ref() {
        service
            .record_monitor_lifecycle_event(
                &scope,
                if paused {
                    "monitor_paused"
                } else {
                    "monitor_resumed"
                },
                &task.manifest.task_id,
                &format!(
                    "{}\u{1f}{}\u{1f}{}",
                    task.manifest.task_id,
                    monitor_state_label(paused),
                    task.manifest.updated_at
                ),
                json!({ "state": monitor_state_label(paused) }),
            )
            .await;
    }
    map_result(updated.map(|task| {
        HttpResponse::Ok().json(json!({
            "task_id": task.manifest.task_id,
            "state": monitor_state_label(paused),
        }))
    }))
}

/// `POST /api/magician/v3/monitors/{task_id}/pause`
pub async fn pause_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    set_monitor_schedule_paused(api, http_req, path, query, true).await
}

/// `POST /api/magician/v3/monitors/{task_id}/resume`
pub async fn resume_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    set_monitor_schedule_paused(api, http_req, path, query, false).await
}

#[derive(Debug, Deserialize)]
pub struct MonitorRunsQueryV3 {
    pub workspace: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /api/magician/v3/monitors/{task_id}/runs?limit=` — accepted
/// (server-finalized) `MonitorRunResultV1` records, newest first, from the
/// Phase 2 change ledger (`ArtifactV2Service::get_monitor_runs` — the
/// existing execution index + persisted-artifact reads, no new store).
///
/// Phase 3+ read seam for Runs history. Reuses the canonical simple page
/// envelope `{items, next_cursor, limit}`; `next_cursor` is always `null`
/// for now — history depth is bounded by `limit` (clamped 1..=200,
/// default 50) and real cursoring arrives when a client needs to page past
/// it. 404 (`monitor_not_found`) for plain tasks, like every monitor route.
pub async fn get_monitor_runs_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MonitorRunsQueryV3>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let limit = query
        .limit
        .unwrap_or(MONITOR_LIST_DEFAULT_LIMIT)
        .clamp(1, MONITOR_LIST_MAX_LIMIT);
    let runs = service.get_monitor_runs(&scope, &task_id, limit).await;
    map_result(runs.map(|items: Vec<MonitorRunResultV1>| {
        HttpResponse::Ok().json(json!({
            "items": items,
            "next_cursor": serde_json::Value::Null,
            "limit": limit,
        }))
    }))
}

/// `GET /api/magician/v3/monitors/{task_id}/updates?limit=` — one monitor's
/// durable update records (Phase 3 notification ledger), newest first, in
/// the same simple `{items, next_cursor, limit}` envelope as the runs
/// route. Records are `MonitorUpdateDetailV1` — the wire shape pinned by
/// `tests/fixtures/monitors/monitor_update_detail_v1.json`. 404
/// (`monitor_not_found`) for plain tasks.
pub async fn get_monitor_updates_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MonitorRunsQueryV3>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let limit = query
        .limit
        .unwrap_or(MONITOR_LIST_DEFAULT_LIMIT)
        .clamp(1, MONITOR_LIST_MAX_LIMIT);
    let updates = service.list_monitor_updates(&scope, &task_id, limit).await;
    map_result(updates.map(|items: Vec<MonitorUpdateDetailV1>| {
        HttpResponse::Ok().json(json!({
            "items": items,
            "next_cursor": serde_json::Value::Null,
            "limit": limit,
        }))
    }))
}

#[derive(Debug, Deserialize)]
pub struct MonitorUpdatesQueryV3 {
    pub workspace: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /api/magician/v3/monitor-updates?limit=` — scope-wide monitor
/// updates, newest first across every monitor in the scope (plan §8). Same
/// envelope as the per-monitor route; scope isolation is inherited from the
/// scoped task walk.
pub async fn list_monitor_updates_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<MonitorUpdatesQueryV3>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let limit = query
        .limit
        .unwrap_or(MONITOR_LIST_DEFAULT_LIMIT)
        .clamp(1, MONITOR_LIST_MAX_LIMIT);
    let service = api.service();
    let updates = service.list_scope_monitor_updates(&scope, limit).await;
    map_result(updates.map(|items: Vec<MonitorUpdateDetailV1>| {
        HttpResponse::Ok().json(json!({
            "items": items,
            "next_cursor": serde_json::Value::Null,
            "limit": limit,
        }))
    }))
}

// ─── Phase 6 — feedback (§10) ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MonitorFeedbackRequestV3 {
    pub workspace: Option<String>,
    /// `useful` | `not_relevant` — anything else is the contract's 400
    /// `monitor_feedback_verdict_invalid`.
    pub verdict: String,
    /// Optional, ≤500 chars (longer input is truncated at admission — the
    /// contract defines no oversized-note error).
    pub note: Option<String>,
}

/// One item of `GET /monitors/{task_id}/feedback` — EXACTLY the contract's
/// item shape (`{feedback_id, update_id, verdict, note?, recorded_at}`);
/// stored evidence is deliberately NOT on this wire (it feeds later
/// rule-suggestion previews, not list rendering).
#[derive(Debug, Serialize)]
pub struct MonitorFeedbackListItemV3 {
    pub feedback_id: String,
    pub update_id: String,
    pub verdict: MonitorFeedbackVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub recorded_at: String,
}

impl From<MonitorUpdateFeedbackV1> for MonitorFeedbackListItemV3 {
    fn from(record: MonitorUpdateFeedbackV1) -> Self {
        Self {
            feedback_id: record.feedback_id,
            update_id: record.update_id,
            verdict: record.verdict,
            note: record.note,
            recorded_at: record.recorded_at,
        }
    }
}

/// `POST /api/magician/v3/monitors/{task_id}/updates/{update_id}/feedback`
/// (§10, fixed wire contract). Idempotent per `(update_id, verdict)`:
/// re-posting the current verdict returns the SAME deterministic
/// `feedback_id` with `recorded: false`; the other verdict replaces (latest
/// wins) and returns its own deterministic id. 404 `monitor_not_found` /
/// `update_not_found`; 400 `monitor_feedback_verdict_invalid`.
pub async fn post_monitor_feedback_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    req: web::Json<MonitorFeedbackRequestV3>,
) -> Result<HttpResponse> {
    let request = req.into_inner();
    let scope = match resolve_required_scope_ref(http_req.headers(), request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let (task_id, update_id) = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let Some(verdict) = MonitorFeedbackVerdict::parse(&request.verdict) else {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "monitor_feedback_verdict_invalid",
            "verdict": request.verdict,
        })));
    };

    match service
        .record_monitor_update_feedback(&scope, &task_id, &update_id, verdict, request.note)
        .await
    {
        Ok(outcome) => Ok(HttpResponse::Ok().json(json!({
            "task_id": task_id,
            "update_id": update_id,
            "verdict": verdict,
            "recorded": outcome.newly_recorded,
            "feedback_id": outcome.feedback.feedback_id,
        }))),
        Err(ArtifactV2Error::InvalidRequest(reason)) if reason == "monitor_update_not_found" => {
            Ok(HttpResponse::NotFound().json(json!({
                "error": "update_not_found",
                "task_id": task_id,
                "update_id": update_id,
            })))
        },
        Err(error) => map_result(Err(error)),
    }
}

/// `GET /api/magician/v3/monitors/{task_id}/feedback?limit=` — the CURRENT
/// verdict per update (latest wins), newest first, in the canonical simple
/// envelope `{items, next_cursor: null, limit}` (limit clamped 1..=200,
/// default 50, like the runs/updates routes). 404 `monitor_not_found` for
/// plain tasks.
pub async fn get_monitor_feedback_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MonitorRunsQueryV3>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let limit = query
        .limit
        .unwrap_or(MONITOR_LIST_DEFAULT_LIMIT)
        .clamp(1, MONITOR_LIST_MAX_LIMIT);
    let feedback = service
        .list_monitor_update_feedback(&scope, &task_id, limit)
        .await;
    map_result(feedback.map(|records: Vec<MonitorUpdateFeedbackV1>| {
        let items: Vec<MonitorFeedbackListItemV3> = records.into_iter().map(Into::into).collect();
        HttpResponse::Ok().json(json!({
            "items": items,
            "next_cursor": serde_json::Value::Null,
            "limit": limit,
        }))
    }))
}

// ─── Phase 6 — aggregate metrics (§12) ───────────────────────────────────

/// One monitor's metrics input: pause state + the CURRENT spec (policy for
/// the suppression recompute) + its newest accepted runs (bounded window).
pub(crate) struct MonitorMetricsInput {
    pub paused: bool,
    pub spec: MonitorSpecV1,
    /// Newest first (the order `get_monitor_runs` serves).
    pub runs: Vec<MonitorRunResultV1>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct MonitorsMetricsCountsV3 {
    pub active: usize,
    pub paused: usize,
    /// Monitors whose NEWEST accepted run is `degraded`.
    pub degraded_last_run: usize,
    /// Monitors whose NEWEST accepted run is `failed`.
    pub failed_last_run: usize,
}

/// Aggregate monitor observability (plan §12: active monitors,
/// degraded counts, material-change rate, notification suppression rate,
/// source failure rate). Computed on read from the existing change ledger —
/// no new store.
#[derive(Debug, PartialEq, Serialize)]
pub struct MonitorsMetricsV3 {
    pub monitors: usize,
    pub counts: MonitorsMetricsCountsV3,
    /// Fraction of considered runs with status `changed`.
    pub material_change_rate: f64,
    /// Fraction of considered runs that notified nothing (the CURRENT
    /// spec's policy recomputed deterministically per run — historical
    /// policy changes are not replayed; documented in the component doc).
    pub suppression_rate: f64,
    /// Fraction of considered runs with any failing source (a non-ok or
    /// incomplete source outcome, an access problem, or a failed run).
    pub source_failure_rate: f64,
    pub window: MonitorsMetricsWindowV3,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct MonitorsMetricsWindowV3 {
    /// The per-monitor run window (newest N accepted runs).
    pub runs_per_monitor: usize,
    /// Total runs the rates were computed over, across all monitors.
    pub runs_considered: usize,
}

/// True when a run observed any source failing (§12 source failure rate):
/// a failed-run marker, an explicit access problem, or any source outcome
/// that is not ok+complete.
fn run_had_source_failure(run: &MonitorRunResultV1) -> bool {
    run.status == MonitorRunStatus::Failed
        || run.access_problem.is_some()
        || run
            .source_outcomes
            .iter()
            .any(|outcome| outcome.status != MonitorSourceOutcomeStatus::Ok || !outcome.complete)
}

/// Pure §12 aggregation over per-monitor ledger windows. Deterministic —
/// unit-tested directly; the handler only assembles inputs.
pub(crate) fn aggregate_monitor_metrics(inputs: &[MonitorMetricsInput]) -> MonitorsMetricsV3 {
    let mut counts = MonitorsMetricsCountsV3 {
        active: 0,
        paused: 0,
        degraded_last_run: 0,
        failed_last_run: 0,
    };
    let mut runs_considered = 0usize;
    let mut material = 0usize;
    let mut suppressed = 0usize;
    let mut source_failures = 0usize;
    for input in inputs {
        if input.paused {
            counts.paused += 1;
        } else {
            counts.active += 1;
        }
        match input.runs.first().map(|run| run.status) {
            Some(MonitorRunStatus::Degraded) => counts.degraded_last_run += 1,
            Some(MonitorRunStatus::Failed) => counts.failed_last_run += 1,
            _ => {},
        }
        for run in input.runs.iter().take(MONITOR_METRICS_RUN_WINDOW) {
            runs_considered += 1;
            let run_material = run.status == MonitorRunStatus::Changed;
            if run_material {
                material += 1;
            }
            if !would_notify(&input.spec, run.status, run_material) {
                suppressed += 1;
            }
            if run_had_source_failure(run) {
                source_failures += 1;
            }
        }
    }
    let rate = |count: usize| {
        if runs_considered == 0 {
            0.0
        } else {
            count as f64 / runs_considered as f64
        }
    };
    MonitorsMetricsV3 {
        monitors: inputs.len(),
        counts,
        material_change_rate: rate(material),
        suppression_rate: rate(suppressed),
        source_failure_rate: rate(source_failures),
        window: MonitorsMetricsWindowV3 {
            runs_per_monitor: MONITOR_METRICS_RUN_WINDOW,
            runs_considered,
        },
    }
}

/// `GET /api/magician/v3/monitors-metrics` — cheap aggregate diagnostics
/// (plan §12): monitor counts by state plus material-change / suppression /
/// source-failure rates over the newest [`MONITOR_METRICS_RUN_WINDOW`] run
/// records per monitor. Everything is computed on read from the existing
/// task records + change ledger (`get_monitor_runs`) — no new store.
///
/// Read cost: the same full-scope record walk as `GET /monitors` PLUS one
/// bounded run-ledger read (execution index + up to N artifact-index reads)
/// per monitor — heavier than the list, fine at personal scale; documented
/// in the component doc's Known limitations.
pub async fn get_monitors_metrics_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let service = api.service();
    let listed = match service.list_tasks(&scope).await {
        Ok(listed) => listed,
        Err(error) => return map_result(Err(error)),
    };
    let mut inputs: Vec<MonitorMetricsInput> = Vec::new();
    for item in listed {
        let task = match service.get_task(&scope, &item.id).await {
            Ok(task) => task,
            Err(ArtifactV2Error::TaskNotFound(_)) => continue,
            Err(error) => return map_result(Err(error)),
        };
        let Some(spec) = task.manifest.monitor_spec.clone() else {
            continue;
        };
        if task.state.status == "archived" {
            continue;
        }
        let schedule = parsed_schedule(task.manifest.schedule.as_ref());
        let runs = match service
            .get_monitor_runs(&scope, &task.manifest.task_id, MONITOR_METRICS_RUN_WINDOW)
            .await
        {
            Ok(runs) => runs,
            Err(error) => return map_result(Err(error)),
        };
        inputs.push(MonitorMetricsInput {
            paused: schedule_is_paused(schedule.as_ref()),
            spec,
            runs,
        });
    }
    Ok(HttpResponse::Ok().json(aggregate_monitor_metrics(&inputs)))
}

/// `POST /api/magician/v3/monitors/{task_id}/run` — run-now. 404 for
/// non-monitors, then EXACTLY the `execute_task_v3_handler` path: the same
/// `start_execution` service call and the same 202 `{task, execution}`
/// response shape (plan §8 rule 4).
pub async fn run_monitor_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    req: Option<web::Json<ExecuteTaskV3Request>>,
) -> Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = path.into_inner();
    let service = api.service();
    if let Err(response) = load_monitor_task(&service, &scope, &task_id).await {
        return Ok(response);
    }
    let refinement = req
        .as_ref()
        .and_then(|value| value.refinement.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let execution = service
        .start_execution_with_llm_routing_overrides(
            scope,
            task_id,
            refinement,
            req.as_ref()
                .and_then(|value| value.overwrite)
                .unwrap_or(false),
            req.as_ref()
                .and_then(|value| value.client_llm_routing_overrides()),
        )
        .await;
    map_result(execution.map(|(task, execution)| {
        HttpResponse::Accepted().json(ExecutionResponseV3 { task, execution })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Was reached through `crate::*` before the crate split moved these tests
    // out of `web_api`; the type itself never moved.
    use actix_web::{http::StatusCode, test, App};
    use magician::magician_v2::artifact_v2::CreateTaskInput;
    use magician::magician_v2::storage::ListKind;
    use magician::magician_v2::test_support::{
        build_test_artifact_v2_service, wire_test_list_index, wire_unready_test_list_index,
    };
    use serde_json::Value;
    use tempfile::TempDir;

    const TEST_PRINCIPAL: &str = "anonymous";
    const TEST_WORKSPACE: &str = "default";

    fn api() -> (TempDir, web::Data<TaskApiV3>) {
        let tmp = TempDir::new().expect("tempdir");
        let service = build_test_artifact_v2_service(tmp.path());
        (tmp, web::Data::new(TaskApiV3::from_service(service)))
    }

    /// Mount exactly the routes `bin/magician.rs` registers under the
    /// `/api/magician/v3` scope for monitors.
    macro_rules! build_app {
        ($api:expr) => {
            test::init_service(
                App::new()
                    .app_data($api.clone())
                    .route("/monitors", web::post().to(create_monitor_v3_handler))
                    .route("/monitors", web::get().to(list_monitors_v3_handler))
                    .route("/monitors/{task_id}", web::get().to(get_monitor_v3_handler))
                    .route(
                        "/monitors/{task_id}",
                        web::patch().to(update_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}",
                        web::delete().to(delete_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/pause",
                        web::post().to(pause_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/resume",
                        web::post().to(resume_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/run",
                        web::post().to(run_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/convert",
                        web::post().to(convert_task_to_monitor_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/runs",
                        web::get().to(get_monitor_runs_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/updates",
                        web::get().to(get_monitor_updates_v3_handler),
                    )
                    .route(
                        "/monitor-updates",
                        web::get().to(list_monitor_updates_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/updates/{update_id}/feedback",
                        web::post().to(post_monitor_feedback_v3_handler),
                    )
                    .route(
                        "/monitors/{task_id}/feedback",
                        web::get().to(get_monitor_feedback_v3_handler),
                    )
                    .route(
                        "/monitors-metrics",
                        web::get().to(get_monitors_metrics_v3_handler),
                    ),
            )
            .await
        };
    }

    fn fixture_spec_json() -> Value {
        serde_json::from_str(include_str!(
            "../../magician/tests/fixtures/monitors/monitor_spec_v1.json"
        ))
        .expect("canonical monitor spec fixture parses")
    }

    fn interval_schedule_json() -> Value {
        serde_json::json!({
            "kind": { "Interval": { "seconds": 3600, "jitter_seconds": null } },
            "timezone": null,
            "missed_fire_policy": "skip",
            "concurrent_execution_policy": "skip"
        })
    }

    fn scoped(request: test::TestRequest, principal: &str) -> test::TestRequest {
        request
            .insert_header(("X-Principal", principal.to_string()))
            .insert_header(("X-Workspace", TEST_WORKSPACE))
    }

    fn scoped_get(uri: &str) -> test::TestRequest {
        scoped(test::TestRequest::get().uri(uri), TEST_PRINCIPAL)
    }

    fn scoped_post(uri: &str, body: Value) -> test::TestRequest {
        scoped(test::TestRequest::post().uri(uri), TEST_PRINCIPAL).set_json(body)
    }

    /// POST /monitors with the given body and assert 201, returning the
    /// created `{task_id, monitor_revision}` JSON.
    macro_rules! create_monitor {
        ($service:expr, $body:expr) => {{
            let req = scoped_post("/monitors", $body).to_request();
            let resp = test::call_service(&$service, req).await;
            assert_eq!(
                resp.status(),
                StatusCode::CREATED,
                "monitor create should be 201"
            );
            let created: Value = test::read_body_json(resp).await;
            created
        }};
    }

    /// GET a monitor list URI, assert 200, and return the page JSON.
    macro_rules! list_page {
        ($service:expr, $uri:expr) => {{
            let resp = test::call_service(&$service, scoped_get($uri).to_request()).await;
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "list should be 200: {}",
                $uri
            );
            let page: Value = test::read_body_json(resp).await;
            page
        }};
    }

    /// Run-now is the third v3 starter a client reaches with
    /// `llm_routing_overrides`; it admits them through the same strip the
    /// execute route uses, so a wire-supplied `parent_engine` never reaches
    /// the service from here either.
    #[actix_web::test]
    async fn run_now_admits_client_routing_overrides_through_the_execute_strip() {
        let source = include_str!("monitors_api.rs");
        let handler = source
            .split("pub async fn run_monitor_v3_handler")
            .nth(1)
            .and_then(|tail| tail.split("\n}\n").next())
            .expect("run-now handler body");
        assert!(
            handler.contains("value.client_llm_routing_overrides()"),
            "run-now must admit a client's overrides through the v3 execute strip"
        );
        assert!(
            !handler.contains("llm_routing_overrides.clone()"),
            "run-now must not hand the raw wire overrides to the service"
        );
        let request: ExecuteTaskV3Request = serde_json::from_value(serde_json::json!({
            "llm_routing_overrides": {"parent_engine": "grok"}
        }))
        .expect("valid run-now request");
        assert_eq!(
            request
                .client_llm_routing_overrides()
                .and_then(|overrides| overrides.parent_engine),
            None
        );
    }

    #[actix_web::test]
    async fn canonical_list_fixture_decodes_with_the_page_types() {
        // Phase 0 contract: the shared list fixture MUST decode with the
        // Phase 1 wire types (unknown Phase 2+ keys like `next_run_at` are
        // tolerated, never required).
        let page: MonitorListPageV3 = serde_json::from_str(include_str!(
            "../../magician/tests/fixtures/monitors/monitor_list_page_v1.json"
        ))
        .expect("canonical list fixture decodes as MonitorListPageV3");
        assert_eq!(page.limit, 50);
        assert_eq!(
            page.next_cursor.as_deref(),
            Some("cur_task_monitor_fixture_002")
        );
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].task_id, "task_monitor_fixture_001");
        assert_eq!(page.items[0].state, "active");
        assert_eq!(page.items[0].monitor_revision, 2);
        assert_eq!(page.items[1].health, "needs_attention");
        assert_eq!(page.items[1].last_run_status, "degraded");
        // Additive: the fixture grew `total`/`offset` without changing a
        // byte of what was already in it. It models a MID-corpus page, so
        // the two must agree with the non-null cursor above.
        assert_eq!(page.total, 3);
        assert_eq!(page.offset, 0);
        assert!(
            page.offset + page.items.len() < page.total,
            "a set next_cursor means there is corpus left after this page"
        );
    }

    /// I6 — the canonical list fixture's `cadence_summary` strings must be
    /// EXACTLY what `cadence_summary()` emits for the corresponding
    /// schedules (weekly-Monday-06:00 cron and daily-07:00 cron, both in
    /// America/Los_Angeles). Web `specForm.ts cadenceSummary` mirrors the
    /// same format; its vitest pins the same strings.
    #[actix_web::test]
    async fn canonical_list_fixture_cadence_strings_match_the_implementation() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../magician/tests/fixtures/monitors/monitor_list_page_v1.json"
        ))
        .expect("list fixture parses");

        let schedule_for = |expression: &str| -> TaskSchedule {
            serde_json::from_value(serde_json::json!({
                "kind": { "Cron": {
                    "expression": expression,
                    "timezone": "America/Los_Angeles"
                } }
            }))
            .expect("schedule parses")
        };

        assert_eq!(
            fixture["items"][0]["cadence_summary"],
            cadence_summary(Some(&schedule_for("0 6 * * 1"))).as_str(),
            "fixture item 0 must carry the implementation's weekly-cron format"
        );
        assert_eq!(
            fixture["items"][1]["cadence_summary"],
            cadence_summary(Some(&schedule_for("0 7 * * *"))).as_str(),
            "fixture item 1 must carry the implementation's daily-cron format"
        );
    }

    #[actix_web::test]
    async fn create_get_list_round_trip_with_projected_tag() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        let created = create_monitor!(
            service,
            serde_json::json!({
                "spec": fixture_spec_json(),
                "schedule": interval_schedule_json()
            })
        );
        assert_eq!(created["monitor_revision"], 1, "first spec write is rev 1");
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        // Detail: spec round-trips, the reserved tag is PROJECTED.
        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{task_id}")).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let detail: Value = test::read_body_json(resp).await;
        assert_eq!(detail["spec"], fixture_spec_json());
        assert_eq!(detail["monitor_revision"], 1);
        assert_eq!(detail["state"]["status"], "pending");
        assert_eq!(detail["state"]["schedule_fire_count"], 0);
        assert!(detail["tags"]
            .as_array()
            .expect("tags array")
            .iter()
            .any(|tag| tag == "system:monitor"));

        // The projection is never persisted: the raw manifest has no tags.
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        let raw = api
            .service()
            .get_task(&scope, &task_id)
            .await
            .expect("raw task loads");
        assert!(
            raw.manifest.tags.is_empty(),
            "system:monitor must never be persisted"
        );

        // List: one active row with the fixture's item keys.
        let page = list_page!(service, "/monitors");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["task_id"], task_id.as_str());
        assert_eq!(items[0]["state"], "active");
        assert_eq!(items[0]["cadence_summary"], "Every 3600s");
        assert_eq!(items[0]["last_run_status"], "never_ran");
        assert_eq!(items[0]["health"], "ok");
        assert_eq!(items[0]["monitor_revision"], 1);
        assert_eq!(page["next_cursor"], Value::Null);
        assert_eq!(page["limit"], 50);
    }

    #[actix_web::test]
    async fn patch_spec_bumps_server_owned_revision() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        let mut edited = fixture_spec_json();
        edited["objective"] = Value::String("Watch the Acme pricing page (weekly)".to_string());
        let req = scoped(
            test::TestRequest::patch().uri(&format!("/monitors/{task_id}")),
            TEST_PRINCIPAL,
        )
        .set_json(serde_json::json!({ "spec": edited }))
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let patched: Value = test::read_body_json(resp).await;
        assert_eq!(patched["monitor_revision"], 2, "spec edit bumps revision");

        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{task_id}")).to_request(),
        )
        .await;
        let detail: Value = test::read_body_json(resp).await;
        assert_eq!(
            detail["spec"]["objective"],
            "Watch the Acme pricing page (weekly)"
        );
        assert_eq!(detail["monitor_revision"], 2);
    }

    #[actix_web::test]
    async fn create_rejects_unknown_schema_version_with_stable_reason() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let mut spec = fixture_spec_json();
        spec["schema_version"] = serde_json::json!(2);
        let req = scoped_post("/monitors", serde_json::json!({ "spec": spec })).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "monitor_schema_version_unsupported");

        // Nothing was created.
        let page = list_page!(service, "/monitors");
        assert!(page["items"].as_array().expect("items").is_empty());
    }

    #[actix_web::test]
    async fn scope_isolation_hides_other_scopes_monitors() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        // A second principal sees an empty list and 404 detail.
        let resp = test::call_service(
            &service,
            scoped(test::TestRequest::get().uri("/monitors"), "someone-else").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let page: Value = test::read_body_json(resp).await;
        assert!(page["items"].as_array().expect("items").is_empty());

        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::get().uri(&format!("/monitors/{task_id}")),
                "someone-else",
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn pagination_uses_the_fixture_envelope_and_cursor() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        for index in 0..3 {
            let mut spec = fixture_spec_json();
            spec["objective"] = Value::String(format!("Watch source number {index}"));
            create_monitor!(service, serde_json::json!({ "spec": spec }));
        }

        let page_one = list_page!(service, "/monitors?limit=2");
        // Envelope keys are EXACTLY the canonical list fixture's.
        let fixture: Value = serde_json::from_str(include_str!(
            "../../magician/tests/fixtures/monitors/monitor_list_page_v1.json"
        ))
        .expect("list fixture parses");
        let mut envelope_keys: Vec<&str> = page_one
            .as_object()
            .expect("page object")
            .keys()
            .map(String::as_str)
            .collect();
        let mut fixture_keys: Vec<&str> = fixture
            .as_object()
            .expect("fixture object")
            .keys()
            .map(String::as_str)
            .collect();
        envelope_keys.sort_unstable();
        fixture_keys.sort_unstable();
        assert_eq!(envelope_keys, fixture_keys);

        let first_items = page_one["items"].as_array().expect("items");
        assert_eq!(first_items.len(), 2);
        assert_eq!(page_one["limit"], 2);
        // `total` is the CORPUS, not the page: it is 3 on a page of 2, and
        // it stays 3 on the next page. A total that tracked the page would
        // report one page of results however many there are.
        assert_eq!(page_one["total"], 3);
        assert_eq!(page_one["offset"], 0);
        let cursor = page_one["next_cursor"].as_str().expect("next_cursor");
        assert!(cursor.starts_with("cur_"), "cursor is prefixed: {cursor}");

        let page_two = list_page!(service, &format!("/monitors?limit=2&cursor={cursor}"));
        let second_items = page_two["items"].as_array().expect("items");
        assert_eq!(second_items.len(), 1);
        assert_eq!(page_two["next_cursor"], Value::Null);
        assert_eq!(page_two["total"], 3, "the corpus did not shrink by paging");
        assert_eq!(
            page_two["offset"], 2,
            "offset is the position the CURSOR resolved to — the caller \
             sent no offset at all, which is the whole reason a monitors \
             pager could not show pages before"
        );

        // No overlap between pages.
        let first_ids: Vec<&str> = first_items
            .iter()
            .map(|item| item["task_id"].as_str().expect("task_id"))
            .collect();
        let second_id = second_items[0]["task_id"].as_str().expect("task_id");
        assert!(!first_ids.contains(&second_id));

        // The bare cursor (no `cur_` prefix) is accepted too.
        let bare = cursor.trim_start_matches("cur_");
        let page_two_bare = list_page!(service, &format!("/monitors?limit=2&cursor={bare}"));
        assert_eq!(page_two_bare["items"], page_two["items"]);
    }

    // ── The storage index's read path ─────────────────────────────────
    //
    // The index reaches a handler through a `OnceLock` only `bin/magician.rs`
    // fills in production, so every test above this line has no index wired
    // in and takes the fallback walk. That makes them a complete proof the
    // WALK still answers every case, and simultaneously the reason nothing
    // exercised a handler CHOOSING the index. These do.
    //
    // Each one turns on the same discriminator: make the index deliberately
    // disagree with the disk (`ListIndex::remove` drops one row), then ask
    // which answer comes back. An assertion that only compared two correct
    // sources would pass whichever one served it.

    /// Create N monitors through the ordinary POST path and return their
    /// task ids. A macro rather than a function for the same reason
    /// `create_monitor!` is one: the actix test service's type is an
    /// unnameable `impl Service<…>`.
    macro_rules! seed_monitors {
        ($service:expr, $count:expr) => {{
            let mut ids: Vec<String> = Vec::new();
            for index in 0..$count {
                let mut spec = fixture_spec_json();
                spec["objective"] = Value::String(format!("Watch source number {index}"));
                let created = create_monitor!($service, serde_json::json!({ "spec": spec }));
                ids.push(created["task_id"].as_str().expect("task_id").to_string());
            }
            ids
        }};
    }

    #[actix_web::test]
    async fn an_indexed_monitor_page_is_the_page_the_walk_serves() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        seed_monitors!(service, 3);

        // Recorded BEFORE any index exists, so the walk's answer cannot have
        // been influenced by the thing it is about to be compared against.
        // `set_list_index` is a OnceLock, so walk-then-index is the only
        // direction available — and it is the useful one.
        let walked_first = list_page!(service, "/monitors?limit=2");
        let walked_cursor = walked_first["next_cursor"]
            .as_str()
            .expect("a next cursor")
            .to_string();
        let walked_second = list_page!(
            service,
            &format!("/monitors?limit=2&cursor={walked_cursor}")
        );

        let _index = wire_test_list_index(&api.service());

        let indexed_first = list_page!(service, "/monitors?limit=2");
        assert_eq!(
            indexed_first, walked_first,
            "the index must serve the identical page — items, order, total, \
             offset and cursor — or the reader's list changes when a cache warms"
        );
        let indexed_second = list_page!(
            service,
            &format!("/monitors?limit=2&cursor={walked_cursor}")
        );
        assert_eq!(
            indexed_second, walked_second,
            "a cursor minted by the walk must resolve to the same place in the index"
        );
    }

    #[actix_web::test]
    async fn the_handler_really_reads_the_index_and_not_the_walk() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let ids = seed_monitors!(service, 3);
        let index = wire_test_list_index(&api.service());

        assert_eq!(list_page!(service, "/monitors")["total"], 3);

        // Drop ONE monitor row from the index while leaving the record on
        // disk untouched. Only a handler reading the index can notice.
        assert!(
            index
                .remove(ListKind::Monitor, &ids[0])
                .expect("removing a monitor row"),
            "the rebuild must have written a monitor row to remove"
        );

        let page = list_page!(service, "/monitors");
        let listed: Vec<&str> = page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["task_id"].as_str().expect("task_id"))
            .collect();
        assert_eq!(
            page["total"], 2,
            "the walk would still have found three monitors on disk; a total of \
             two is proof the index answered this request"
        );
        assert!(
            !listed.contains(&ids[0].as_str()),
            "the row removed from the index must be the one missing from the page"
        );
        assert_eq!(listed.len(), 2);

        // The record itself is untouched, which is what makes the assertion
        // above about the READ path rather than about a deletion.
        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{}", ids[0])).to_request(),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "detail reads the record, not the index, so it still resolves"
        );
    }

    #[actix_web::test]
    async fn an_unready_index_is_never_read() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        // Wired BEFORE the monitors exist and never rebuilt, so it is unready
        // for the whole test — the state a reader meets during a rebuild, and
        // still meets after a crash partway through one.
        let index = wire_unready_test_list_index(&api.service());
        let ids = seed_monitors!(service, 3);
        assert!(!index.is_ready().expect("readiness"));

        // The write hook kept it current anyway, so make it disagree: a
        // handler that consulted it would drop this monitor.
        assert!(
            index
                .remove(ListKind::Monitor, &ids[0])
                .expect("removing a monitor row"),
            "the write hook must have written a monitor row, or an unready index \
             holding nothing would agree with the walk by accident"
        );

        let page = list_page!(service, "/monitors");
        assert_eq!(
            page["total"], 3,
            "a half-built index looks exactly like a complete one holding fewer \
             monitors — which is why `is_ready()` gates the read at all"
        );
        let listed: Vec<&str> = page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["task_id"].as_str().expect("task_id"))
            .collect();
        assert!(listed.contains(&ids[0].as_str()));
    }

    #[actix_web::test]
    async fn the_indexed_state_filter_tracks_a_live_pause() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        // Scheduled at create, because pausing an unscheduled monitor is a
        // 409 — there is nothing to pause.
        let mut ids: Vec<String> = Vec::new();
        for index in 0..2 {
            let mut spec = fixture_spec_json();
            spec["objective"] = Value::String(format!("Watch source number {index}"));
            let created = create_monitor!(
                service,
                serde_json::json!({ "spec": spec, "schedule": interval_schedule_json() })
            );
            ids.push(created["task_id"].as_str().expect("task_id").to_string());
        }
        let index = wire_test_list_index(&api.service());

        assert_eq!(list_page!(service, "/monitors?state=active")["total"], 2);
        assert_eq!(list_page!(service, "/monitors?state=paused")["total"], 0);

        // Pausing writes the record, and the write hook must move `paused` on
        // the monitor row with it — otherwise `state=` would answer from a
        // column that stopped tracking the schedule.
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{}/pause", ids[0]),
                serde_json::json!({}),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(index.is_ready().expect("readiness"));

        let active = list_page!(service, "/monitors?state=active");
        let paused = list_page!(service, "/monitors?state=paused");
        assert_eq!(active["total"], 1);
        assert_eq!(paused["total"], 1);
        assert_eq!(
            paused["items"][0]["task_id"], ids[0],
            "the paused list must hold the monitor that was actually paused"
        );
        assert_eq!(paused["items"][0]["state"], "paused");
        assert_eq!(active["items"][0]["task_id"], ids[1]);
        assert_eq!(active["items"][0]["state"], "active");
    }

    #[actix_web::test]
    async fn malformed_cursor_terminates_pagination_gracefully() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));

        // Garbage cursors (never a valid task id, prefixed or not, even
        // URL-encoded binary noise) must read as "past the end": 200 with
        // an empty page and a null next_cursor — never a 5xx or a panic.
        for cursor in [
            "cur_definitely_not_a_task",
            "definitely_not_a_task",
            "cur_",
            "%F0%9F%92%A9garbage",
            "cur_task_%2e%2e%2fescape",
        ] {
            let page = list_page!(service, &format!("/monitors?cursor={cursor}"));
            assert!(
                page["items"].as_array().expect("items").is_empty(),
                "stale/garbage cursor must yield an empty page: {cursor}"
            );
            assert_eq!(page["next_cursor"], Value::Null, "cursor: {cursor}");
            // The corpus is still reported even when the cursor found
            // nothing in it — a pager that lost its place must still be
            // able to render "page N of M" and offer a jump back.
            assert_eq!(page["total"], 1, "cursor: {cursor}");
            assert_eq!(
                page["offset"], 1,
                "a cursor past the end resolves to the end, not to zero: {cursor}"
            );
        }

        // A whitespace-only cursor is treated as absent → first page.
        let page = list_page!(service, "/monitors?cursor=%20%20");
        assert_eq!(page["items"].as_array().expect("items").len(), 1);
        assert_eq!(page["total"], 1);
        assert_eq!(page["offset"], 0);
    }

    #[actix_web::test]
    async fn patch_schedule_only_does_not_bump_monitor_revision() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        // Schedule-only PATCH: the spec is untouched, so the server-owned
        // monitor_revision must stay at 1 (revision counts SPEC contracts,
        // not cadence changes).
        let req = scoped(
            test::TestRequest::patch().uri(&format!("/monitors/{task_id}")),
            TEST_PRINCIPAL,
        )
        .set_json(serde_json::json!({ "schedule": interval_schedule_json() }))
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let patched: Value = test::read_body_json(resp).await;
        assert_eq!(
            patched["monitor_revision"], 1,
            "schedule-only edits must not bump the spec revision"
        );

        // Title-only PATCH: same rule.
        let req = scoped(
            test::TestRequest::patch().uri(&format!("/monitors/{task_id}")),
            TEST_PRINCIPAL,
        )
        .set_json(serde_json::json!({ "title": "Renamed watch" }))
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let patched: Value = test::read_body_json(resp).await;
        assert_eq!(patched["monitor_revision"], 1);

        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{task_id}")).to_request(),
        )
        .await;
        let detail: Value = test::read_body_json(resp).await;
        assert_eq!(detail["monitor_revision"], 1);
        assert_eq!(detail["title"], "Renamed watch");
        assert!(detail["schedule"].is_object(), "schedule persisted");
    }

    #[actix_web::test]
    async fn delete_removes_monitor_owned_feed_and_attention_items() {
        use magician::magician_v2::{
            feed::{FeedItemStatus, FeedItemType, FeedQuery, FeedStore},
            monitors::monitor_run::{
                MonitorAccessProblemV1, MonitorSourceFailureEntry, MonitorSourceOutcomeStatus,
            },
            monitors::monitor_updates::{
                monitor_access_problem_feed_item, monitor_access_problem_item_id,
            },
            realtime_events::RuntimeTransportBroadcaster,
        };

        let (tmp, api) = api();
        let feed_store = FeedStore::open(&tmp.path().join("feed_store")).expect("feed store opens");
        api.service().set_feed_projection(
            feed_store.clone(),
            Arc::new(RuntimeTransportBroadcaster::new(8)),
        );
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        // Materialize a monitor-owned Needs-You escalation the way the
        // Phase 3 attention projection does (same builder, same store).
        let source = "https://dash.example/reports";
        let problem = MonitorAccessProblemV1 {
            source: source.to_string(),
            kind: MonitorSourceOutcomeStatus::AuthFailed,
            message: "Session expired.".to_string(),
            since: "2026-07-22T06:00:00Z".to_string(),
        };
        let entry = MonitorSourceFailureEntry {
            source: source.to_string(),
            consecutive_failures: 2,
            last_status: MonitorSourceOutcomeStatus::AuthFailed,
            since: "2026-07-22T06:00:00Z".to_string(),
        };
        let item = monitor_access_problem_feed_item(
            TEST_PRINCIPAL,
            TEST_WORKSPACE,
            &task_id,
            "Dashboard monitor",
            &problem,
            &entry,
            "exec_del_1",
            None,
            1_800_000_000_000,
        );
        assert_eq!(item.id, monitor_access_problem_item_id(&task_id, source));
        feed_store.upsert_item(item).await.expect("item upserts");

        let escalations = || {
            let feed_store = feed_store.clone();
            async move {
                feed_store
                    .list_items(FeedQuery {
                        principal: TEST_PRINCIPAL.to_string(),
                        workspace: TEST_WORKSPACE.to_string(),
                        limit: 10,
                        item_type: Some(FeedItemType::Escalation),
                        status: Some(FeedItemStatus::NeedsAction),
                        ..Default::default()
                    })
                    .await
                    .expect("escalation query")
            }
        };
        assert_eq!(escalations().await.len(), 1, "item exists before delete");

        // DELETE /monitors/{id} (default soft delete) must cascade the
        // monitor-owned feed/attention rows away with the task.
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::delete().uri(&format!("/monitors/{task_id}")),
                TEST_PRINCIPAL,
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            escalations().await.is_empty(),
            "deleting the monitor must remove its feed/attention items"
        );
    }

    #[actix_web::test]
    async fn pause_resume_flip_schedule_state_and_list_filters() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(
            service,
            serde_json::json!({
                "spec": fixture_spec_json(),
                "schedule": interval_schedule_json()
            })
        );
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        let resp = test::call_service(
            &service,
            scoped_post(&format!("/monitors/{task_id}/pause"), serde_json::json!({})).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let paused: Value = test::read_body_json(resp).await;
        assert_eq!(paused["state"], "paused");

        let page = list_page!(service, "/monitors?state=paused");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["state"], "paused");
        assert_eq!(page["total"], 1);
        let page = list_page!(service, "/monitors?state=active");
        assert!(page["items"].as_array().expect("items").is_empty());
        // `total` counts the FILTERED corpus. A total taken before the
        // `state` filter would tell a pager there is a page of active
        // monitors to fetch when there is not one.
        assert_eq!(page["total"], 0);
        assert_eq!(page["offset"], 0);

        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/resume"),
                serde_json::json!({}),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resumed: Value = test::read_body_json(resp).await;
        assert_eq!(resumed["state"], "active");
        let page = list_page!(service, "/monitors?state=active");
        assert_eq!(page["items"].as_array().expect("items").len(), 1);
    }

    #[actix_web::test]
    async fn pausing_an_unscheduled_monitor_conflicts() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        let resp = test::call_service(
            &service,
            scoped_post(&format!("/monitors/{task_id}/pause"), serde_json::json!({})).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "monitor_unscheduled");

        // An unscheduled monitor lists as active/unscheduled.
        let page = list_page!(service, "/monitors");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items[0]["cadence_summary"], "unscheduled");
        assert_eq!(items[0]["state"], "active");
    }

    #[actix_web::test]
    async fn delete_removes_the_monitor_from_list_and_detail() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();

        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::delete().uri(&format!("/monitors/{task_id}")),
                TEST_PRINCIPAL,
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["task_id"], task_id.as_str());

        let page = list_page!(service, "/monitors");
        assert!(page["items"].as_array().expect("items").is_empty());
        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{task_id}")).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn runs_endpoint_returns_accepted_ledger_results_newest_first() {
        use magician::magician_v2::artifact_v2::models::{
            ExecutionRecord, ExecutionRefs, ExecutionState, TaskOutputMode,
        };
        use magician::magician_v2::monitors::monitor_run::{
            MonitorCountsV1, MonitorFindingClassification, MonitorFindingV1, MonitorRunResultV1,
            MonitorRunStatus, MonitorSourceOutcomeStatus, MonitorSourceOutcomeV1,
        };

        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );

        // Empty ledger: the canonical simple envelope with zero items.
        let page = list_page!(service, &format!("/monitors/{task_id}/runs"));
        assert!(page["items"].as_array().expect("items").is_empty());
        assert_eq!(page["next_cursor"], Value::Null);
        assert_eq!(page["limit"], 50);

        // Register one execution the way pipeline discovery does, then
        // accept a baseline run through the Phase 2 service path. Through the
        // API's own service, so the write reaches the same reconciler the
        // endpoint below reads through; a reducer built here over a second
        // workspace handle would carry a second `TaskWriteReconciler`, which
        // is the stale-index bug in test form.
        api.service()
            .reducer()
            .reduce_execution_discovered(
                &scope,
                &ExecutionRecord {
                    state: ExecutionState {
                        execution_id: "exec_run_1".to_string(),
                        task_id: task_id.clone(),
                        root_execution_id: Some("exec_run_1".to_string()),
                        parent_execution_id: None,
                        agent_id: "personal-assistant".to_string(),
                        relationship_type: "root".to_string(),
                        status: "completed".to_string(),
                        completion_kind: None,
                        open_items: Vec::new(),
                        plan_id: None,
                        primary_execution_output_id: None,
                        active_child_execution_ids: Vec::new(),
                        started_at: "2026-07-22T06:00:00Z".to_string(),
                        completed_at: Some("2026-07-22T06:01:00Z".to_string()),
                        updated_at: "2026-07-22T06:01:00Z".to_string(),
                        completed_step_ids: Vec::new(),
                        failed_step_ids: Vec::new(),
                        current_step_id: None,
                        task_output_mode: TaskOutputMode::default(),
                        refinement: None,
                        synthesis_pending: false,
                        synthesis_failed: None,
                    },
                    refs: ExecutionRefs {
                        execution_id: "exec_run_1".to_string(),
                        ..Default::default()
                    },
                },
            )
            .await
            .expect("execution discovers");

        let mut finding = MonitorFindingV1 {
            stable_key: "native:item-1".to_string(),
            title: "Item 1 present".to_string(),
            canonical_url: None,
            source: "https://acme.example/pricing".to_string(),
            observed_at: "2026-07-22T06:00:30Z".to_string(),
            published_at: None,
            summary: "Item 1 observed.".to_string(),
            why_it_matters: "In contract.".to_string(),
            entities: Vec::new(),
            evidence: Vec::new(),
            content_fingerprint: String::new(),
            // A coherent modest claim — the backend recomputes the real
            // classification (baseline → new) at acceptance.
            classification: MonitorFindingClassification::Unchanged,
        };
        finding.content_fingerprint =
            magician::magician_v2::monitors::monitor_run::content_fingerprint(&finding);
        let accepted = api
            .service()
            .accept_monitor_run(
                &scope,
                &task_id,
                "exec_run_1",
                MonitorRunResultV1 {
                    monitor_task_id: task_id.clone(),
                    execution_id: "exec_run_1".to_string(),
                    monitor_revision: 1,
                    started_at: "2026-07-22T06:00:00Z".to_string(),
                    completed_at: "2026-07-22T06:01:00Z".to_string(),
                    status: MonitorRunStatus::Unchanged,
                    complete_scan: true,
                    source_outcomes: vec![MonitorSourceOutcomeV1 {
                        source: "https://acme.example/pricing".to_string(),
                        status: MonitorSourceOutcomeStatus::Ok,
                        complete: true,
                        items_scanned: 1,
                        note: None,
                    }],
                    counts: MonitorCountsV1 {
                        scanned: 1,
                        new: 0,
                        updated: 0,
                        unchanged: 1,
                        possibly_removed: 0,
                    },
                    findings: vec![finding],
                    run_fingerprint: "rf_0000000000000000".to_string(),
                    change_fingerprint: None,
                    access_problem: None,
                },
            )
            .await
            .expect("run accepts");
        assert_eq!(accepted.result.status, MonitorRunStatus::Baseline);

        // The endpoint serves the FINALIZED (server-owned) record.
        let page = list_page!(service, &format!("/monitors/{task_id}/runs?limit=10"));
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["execution_id"], "exec_run_1");
        assert_eq!(items[0]["status"], "baseline");
        assert_eq!(items[0]["findings"][0]["classification"], "new");
        assert_eq!(page["limit"], 10);
        assert_eq!(page["next_cursor"], Value::Null);

        // Plain tasks and other scopes still 404 on the runs route.
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::get().uri(&format!("/monitors/{task_id}/runs")),
                "someone-else",
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn updates_endpoints_serve_the_deduped_ledger_newest_first() {
        use magician::magician_v2::artifact_v2::models::{
            ExecutionRecord, ExecutionRefs, ExecutionState, TaskOutputMode,
        };
        use magician::magician_v2::monitors::monitor_run::{
            content_fingerprint, MonitorCountsV1, MonitorFindingClassification, MonitorFindingV1,
            MonitorRunResultV1, MonitorRunStatus, MonitorSourceOutcomeStatus,
            MonitorSourceOutcomeV1,
        };

        let (_tmp, api) = api();
        let service = build_app!(api);
        let created = create_monitor!(service, serde_json::json!({ "spec": fixture_spec_json() }));
        let task_id = created["task_id"].as_str().expect("task_id").to_string();
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );

        // Empty ledger: the canonical simple envelope, both routes.
        let page = list_page!(service, &format!("/monitors/{task_id}/updates"));
        assert!(page["items"].as_array().expect("items").is_empty());
        assert_eq!(page["next_cursor"], Value::Null);
        assert_eq!(page["limit"], 50);
        let page = list_page!(service, "/monitor-updates");
        assert!(page["items"].as_array().expect("items").is_empty());

        // Register two executions and drive the Phase 2+3 service seam the
        // way the terminal hook does: accept, then project. Through the API's
        // own service, so the write reaches the same reconciler the endpoints
        // read through; a reducer built here over a second workspace handle
        // would carry a second `TaskWriteReconciler`, which is the
        // stale-index bug in test form.
        let artifact_service = api.service();
        for (execution_id, started_at) in [
            ("exec_upd_1", "2026-07-22T06:00:00Z"),
            ("exec_upd_2", "2026-07-23T06:00:00Z"),
        ] {
            artifact_service
                .reducer()
                .reduce_execution_discovered(
                    &scope,
                    &ExecutionRecord {
                        state: ExecutionState {
                            execution_id: execution_id.to_string(),
                            task_id: task_id.clone(),
                            root_execution_id: Some(execution_id.to_string()),
                            parent_execution_id: None,
                            agent_id: "personal-assistant".to_string(),
                            relationship_type: "root".to_string(),
                            status: "completed".to_string(),
                            completion_kind: None,
                            open_items: Vec::new(),
                            plan_id: None,
                            primary_execution_output_id: None,
                            active_child_execution_ids: Vec::new(),
                            started_at: started_at.to_string(),
                            completed_at: Some(started_at.to_string()),
                            updated_at: started_at.to_string(),
                            completed_step_ids: Vec::new(),
                            failed_step_ids: Vec::new(),
                            current_step_id: None,
                            task_output_mode: TaskOutputMode::default(),
                            refinement: None,
                            synthesis_pending: false,
                            synthesis_failed: None,
                        },
                        refs: ExecutionRefs {
                            execution_id: execution_id.to_string(),
                            ..Default::default()
                        },
                    },
                )
                .await
                .expect("execution discovers");
        }

        let make_finding = |title: &str| {
            let mut finding = MonitorFindingV1 {
                stable_key: "native:item-1".to_string(),
                title: title.to_string(),
                canonical_url: None,
                source: "https://acme.example/pricing".to_string(),
                observed_at: "2026-07-22T06:00:30Z".to_string(),
                published_at: None,
                summary: format!("{title}."),
                why_it_matters: "In contract.".to_string(),
                entities: Vec::new(),
                evidence: Vec::new(),
                content_fingerprint: String::new(),
                classification: MonitorFindingClassification::Unchanged,
            };
            finding.content_fingerprint = content_fingerprint(&finding);
            finding
        };
        let make_result = |execution_id: &str, started_at: &str, title: &str| MonitorRunResultV1 {
            monitor_task_id: task_id.clone(),
            execution_id: execution_id.to_string(),
            monitor_revision: 1,
            started_at: started_at.to_string(),
            completed_at: started_at.to_string(),
            status: MonitorRunStatus::Unchanged,
            complete_scan: true,
            source_outcomes: vec![MonitorSourceOutcomeV1 {
                source: "https://acme.example/pricing".to_string(),
                status: MonitorSourceOutcomeStatus::Ok,
                complete: true,
                items_scanned: 1,
                note: None,
            }],
            counts: MonitorCountsV1 {
                scanned: 1,
                new: 0,
                updated: 0,
                unchanged: 1,
                possibly_removed: 0,
            },
            findings: vec![make_finding(title)],
            run_fingerprint: "rf_0000000000000000".to_string(),
            change_fingerprint: None,
            access_problem: None,
        };

        // Baseline (quiet under the fixture's material_changes policy) then
        // a material update on the same stable key.
        let baseline = api
            .service()
            .accept_monitor_run(
                &scope,
                &task_id,
                "exec_upd_1",
                make_result("exec_upd_1", "2026-07-22T06:00:00Z", "Pro plan $49"),
            )
            .await
            .expect("baseline accepts");
        api.service()
            .project_monitor_run_outcome(&scope, &baseline)
            .await
            .expect("baseline projects");
        let changed = api
            .service()
            .accept_monitor_run(
                &scope,
                &task_id,
                "exec_upd_2",
                make_result("exec_upd_2", "2026-07-23T06:00:00Z", "Pro plan $59"),
            )
            .await
            .expect("material run accepts");
        assert!(changed.material && changed.would_notify);
        api.service()
            .project_monitor_run_outcome(&scope, &changed)
            .await
            .expect("material run projects");

        // Newest first: the material update leads, then the baseline record.
        let page = list_page!(service, &format!("/monitors/{task_id}/updates"));
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["execution_id"], "exec_upd_2");
        assert_eq!(items[0]["status"], "changed");
        assert_eq!(items[0]["notification"]["emitted"], true);
        assert_eq!(
            items[0]["notification"]["dedupe_key"],
            format!(
                "anonymous/default:{task_id}:1:{}:today_changed",
                changed
                    .result
                    .change_fingerprint
                    .as_deref()
                    .expect("change fingerprint")
            ),
            "§7.4: EXACTLY scope:task_id:revision:change_fingerprint:channel"
        );
        assert_eq!(items[1]["execution_id"], "exec_upd_1");
        assert_eq!(items[1]["status"], "baseline");
        assert_eq!(items[1]["notification"]["emitted"], false);

        // Pagination: limit clamps and serves the newest record only.
        let page = list_page!(service, &format!("/monitors/{task_id}/updates?limit=1"));
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["execution_id"], "exec_upd_2");
        assert_eq!(page["limit"], 1);

        // Replayed acceptance (retry) re-projects without double-recording.
        let replay = api
            .service()
            .accept_monitor_run(
                &scope,
                &task_id,
                "exec_upd_2",
                make_result("exec_upd_2", "2026-07-23T06:00:00Z", "Pro plan $59"),
            )
            .await
            .expect("replay accepts idempotently");
        assert!(!replay.newly_accepted);
        api.service()
            .project_monitor_run_outcome(&scope, &replay)
            .await
            .expect("replay projects");
        let page = list_page!(service, &format!("/monitors/{task_id}/updates"));
        assert_eq!(page["items"].as_array().expect("items").len(), 2);

        // Scope-wide route: same records, newest first; foreign scopes and
        // plain tasks stay isolated.
        let page = list_page!(service, "/monitor-updates?limit=10");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["execution_id"], "exec_upd_2");
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::get().uri("/monitor-updates"),
                "someone-else",
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let page: Value = test::read_body_json(resp).await;
        assert!(page["items"].as_array().expect("items").is_empty());
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::get().uri(&format!("/monitors/{task_id}/updates")),
                "someone-else",
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ─── Phase 6 test support ────────────────────────────────────────────

    /// Seed one monitor with two accepted+projected runs (quiet baseline,
    /// then a material change on the same stable key) through the REAL
    /// Phase 2/3 service seams. Returns the task id and its update records,
    /// newest first (`[changed, baseline]`).
    async fn seed_monitor_with_two_updates(
        api: &web::Data<TaskApiV3>,
    ) -> (String, Vec<MonitorUpdateDetailV1>) {
        use magician::magician_v2::artifact_v2::models::{
            ExecutionRecord, ExecutionRefs, ExecutionState, TaskOutputMode,
        };
        use magician::magician_v2::monitors::monitor_run::{
            content_fingerprint, MonitorCountsV1, MonitorFindingClassification, MonitorFindingV1,
            MonitorRunResultV1 as RunResult, MonitorRunStatus, MonitorSourceOutcomeStatus,
            MonitorSourceOutcomeV1,
        };

        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        let mut spec: MonitorSpecV1 =
            serde_json::from_value(fixture_spec_json()).expect("fixture spec decodes");
        validate_and_normalize(&mut spec).expect("fixture spec is valid");
        let task = create_monitor_task(
            &api.service(),
            &scope,
            None,
            spec,
            None,
            DEFAULT_MONITOR_AGENT_ID.to_string(),
        )
        .await
        .expect("monitor creates");
        let task_id = task.manifest.task_id.clone();

        // The API's own reducer, not one built here over a second workspace
        // handle: a separately built reducer carries a separately built
        // `TaskWriteReconciler`, so its writes would reconcile a different
        // index than the endpoints under test read through — the stale-index
        // bug in test form.
        let artifact_service = api.service();
        for (execution_id, started_at) in [
            ("exec_fb_1", "2026-07-22T06:00:00Z"),
            ("exec_fb_2", "2026-07-23T06:00:00Z"),
        ] {
            artifact_service
                .reducer()
                .reduce_execution_discovered(
                    &scope,
                    &ExecutionRecord {
                        state: ExecutionState {
                            execution_id: execution_id.to_string(),
                            task_id: task_id.clone(),
                            root_execution_id: Some(execution_id.to_string()),
                            parent_execution_id: None,
                            agent_id: DEFAULT_MONITOR_AGENT_ID.to_string(),
                            relationship_type: "root".to_string(),
                            status: "completed".to_string(),
                            completion_kind: None,
                            open_items: Vec::new(),
                            plan_id: None,
                            primary_execution_output_id: None,
                            active_child_execution_ids: Vec::new(),
                            started_at: started_at.to_string(),
                            completed_at: Some(started_at.to_string()),
                            updated_at: started_at.to_string(),
                            completed_step_ids: Vec::new(),
                            failed_step_ids: Vec::new(),
                            current_step_id: None,
                            task_output_mode: TaskOutputMode::default(),
                            refinement: None,
                            synthesis_pending: false,
                            synthesis_failed: None,
                        },
                        refs: ExecutionRefs {
                            execution_id: execution_id.to_string(),
                            ..Default::default()
                        },
                    },
                )
                .await
                .expect("execution discovers");
        }

        let make_result = |execution_id: &str, started_at: &str, title: &str| {
            let mut finding = MonitorFindingV1 {
                stable_key: "native:item-1".to_string(),
                title: title.to_string(),
                canonical_url: None,
                source: "https://acme.example/pricing".to_string(),
                observed_at: started_at.to_string(),
                published_at: None,
                summary: format!("{title}."),
                why_it_matters: "In contract.".to_string(),
                entities: Vec::new(),
                evidence: Vec::new(),
                content_fingerprint: String::new(),
                classification: MonitorFindingClassification::Unchanged,
            };
            finding.content_fingerprint = content_fingerprint(&finding);
            RunResult {
                monitor_task_id: task_id.clone(),
                execution_id: execution_id.to_string(),
                monitor_revision: 1,
                started_at: started_at.to_string(),
                completed_at: started_at.to_string(),
                status: MonitorRunStatus::Unchanged,
                complete_scan: true,
                source_outcomes: vec![MonitorSourceOutcomeV1 {
                    source: "https://acme.example/pricing".to_string(),
                    status: MonitorSourceOutcomeStatus::Ok,
                    complete: true,
                    items_scanned: 1,
                    note: None,
                }],
                counts: MonitorCountsV1 {
                    scanned: 1,
                    new: 0,
                    updated: 0,
                    unchanged: 1,
                    possibly_removed: 0,
                },
                findings: vec![finding],
                run_fingerprint: "rf_0000000000000000".to_string(),
                change_fingerprint: None,
                access_problem: None,
            }
        };
        let scope_ref = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        for (execution_id, started_at, title) in [
            ("exec_fb_1", "2026-07-22T06:00:00Z", "Pro plan $49"),
            ("exec_fb_2", "2026-07-23T06:00:00Z", "Pro plan $59"),
        ] {
            let accepted = api
                .service()
                .accept_monitor_run(
                    &scope_ref,
                    &task_id,
                    execution_id,
                    make_result(execution_id, started_at, title),
                )
                .await
                .expect("run accepts");
            api.service()
                .project_monitor_run_outcome(&scope_ref, &accepted)
                .await
                .expect("run projects");
        }
        let updates = api
            .service()
            .list_monitor_updates(&scope_ref, &task_id, 10)
            .await
            .expect("updates list");
        assert_eq!(updates.len(), 2, "seed produced baseline + changed");
        (task_id, updates)
    }

    /// Count funnel events for one §12 trace name (`monitor-obs:{trace}:`
    /// prefixed deterministic ids — see `append_monitor_trace_event`).
    async fn count_trace_events(
        funnel: &magician::magician_v2::attention_funnel_store::AttentionFunnelStore,
        trace: &str,
    ) -> usize {
        funnel
            .observability(TEST_PRINCIPAL, TEST_WORKSPACE, None, 100)
            .await
            .expect("observability")
            .recent_events
            .iter()
            .filter(|event| event.event_id.starts_with(&format!("monitor-obs:{trace}:")))
            .count()
    }

    // ─── Phase 6 — feedback contract (§10, fixed wire contract) ─────────

    #[actix_web::test]
    async fn feedback_record_replay_flip_and_errors_follow_the_wire_contract() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let (task_id, updates) = seed_monitor_with_two_updates(&api).await;
        let changed = &updates[0];
        assert_eq!(changed.status, MonitorRunStatus::Changed);
        let update_id = changed.update_id.clone();

        // 1) Record `useful`.
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({ "verdict": "useful" }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let first: Value = test::read_body_json(resp).await;
        assert_eq!(first["task_id"], task_id.as_str());
        assert_eq!(first["update_id"], update_id.as_str());
        assert_eq!(first["verdict"], "useful");
        assert_eq!(first["recorded"], true);
        let useful_id = first["feedback_id"]
            .as_str()
            .expect("feedback_id")
            .to_string();
        assert!(useful_id.starts_with("mf_"), "{useful_id}");
        assert_eq!(useful_id.len(), "mf_".len() + 16);

        // 2) Idempotent replay: same verdict → SAME id, recorded:false.
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({ "verdict": "useful" }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let replay: Value = test::read_body_json(resp).await;
        assert_eq!(replay["recorded"], false);
        assert_eq!(replay["feedback_id"], useful_id.as_str());

        // 3) Flip: the OTHER verdict replaces (latest wins) with a new id.
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({ "verdict": "not_relevant", "note": "wrong SKU" }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let flipped: Value = test::read_body_json(resp).await;
        assert_eq!(flipped["recorded"], true);
        let not_relevant_id = flipped["feedback_id"].as_str().expect("id").to_string();
        assert_ne!(not_relevant_id, useful_id);

        // GET serves the CURRENT verdict only (latest wins), exact contract
        // item keys, canonical envelope.
        let page = list_page!(service, &format!("/monitors/{task_id}/feedback"));
        assert_eq!(page["limit"], 50);
        assert_eq!(page["next_cursor"], Value::Null);
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["feedback_id"], not_relevant_id.as_str());
        assert_eq!(items[0]["update_id"], update_id.as_str());
        assert_eq!(items[0]["verdict"], "not_relevant");
        assert_eq!(items[0]["note"], "wrong SKU");
        assert!(items[0]["recorded_at"].is_string());
        let mut keys: Vec<&str> = items[0]
            .as_object()
            .expect("item object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["feedback_id", "note", "recorded_at", "update_id", "verdict"],
            "GET items carry EXACTLY the contract fields (no stored evidence)"
        );

        // 4) §10 evidence: the not_relevant record retained the update's
        // finding identities, copied at write time (service read — evidence
        // deliberately stays off the list wire).
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        let records = api
            .service()
            .list_monitor_update_feedback(&scope, &task_id, 10)
            .await
            .expect("feedback records");
        assert_eq!(records.len(), 1);
        let evidence = records[0].evidence.as_ref().expect("evidence retained");
        assert_eq!(evidence.execution_id, changed.execution_id);
        assert_eq!(evidence.change_fingerprint, changed.change_fingerprint);
        assert_eq!(
            evidence.stable_keys,
            changed
                .findings
                .iter()
                .map(|f| f.stable_key.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            evidence.content_fingerprints,
            changed
                .findings
                .iter()
                .map(|f| f.content_fingerprint.clone())
                .collect::<Vec<_>>()
        );

        // 5) Flip BACK: deterministic ids → the original useful id returns,
        // as a real (re-)recording.
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({ "verdict": "useful" }),
            )
            .to_request(),
        )
        .await;
        let back: Value = test::read_body_json(resp).await;
        assert_eq!(back["recorded"], true);
        assert_eq!(back["feedback_id"], useful_id.as_str());

        // 6) Errors: invalid verdict → 400; unknown update → 404
        // update_not_found; plain/missing monitor → 404 monitor_not_found.
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({ "verdict": "meh" }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "monitor_feedback_verdict_invalid");

        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/mu_does_not_exist/feedback"),
                serde_json::json!({ "verdict": "useful" }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "update_not_found");

        let plain = api
            .service()
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "Plain".to_string(),
                description: "not a monitor".to_string(),
                agent_id: DEFAULT_MONITOR_AGENT_ID.to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: magician::magician_v2::artifact_v2::models::TaskOutputMode::default(),
                chat_session_id: None,
                lifecycle: magician::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
                sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("plain task creates");
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!(
                    "/monitors/{}/updates/{update_id}/feedback",
                    plain.manifest.task_id
                ),
                serde_json::json!({ "verdict": "useful" }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "monitor_not_found");

        // Scope isolation: another principal 404s on both feedback routes.
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::get().uri(&format!("/monitors/{task_id}/feedback")),
                "someone-else",
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn feedback_note_is_truncated_and_limit_clamps() {
        use magician::magician_v2::monitors::monitor_feedback::MONITOR_FEEDBACK_NOTE_MAX_CHARS;

        let (_tmp, api) = api();
        let service = build_app!(api);
        let (task_id, updates) = seed_monitor_with_two_updates(&api).await;
        let update_id = updates[0].update_id.clone();

        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({
                    "verdict": "not_relevant",
                    "note": "n".repeat(MONITOR_FEEDBACK_NOTE_MAX_CHARS + 100),
                }),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let page = list_page!(service, &format!("/monitors/{task_id}/feedback?limit=1"));
        assert_eq!(page["limit"], 1);
        let note = page["items"][0]["note"].as_str().expect("note kept");
        assert_eq!(note.chars().count(), MONITOR_FEEDBACK_NOTE_MAX_CHARS);

        // Limit clamps to the shared 1..=200 range.
        let page = list_page!(service, &format!("/monitors/{task_id}/feedback?limit=0"));
        assert_eq!(page["limit"], 1);
        let page = list_page!(service, &format!("/monitors/{task_id}/feedback?limit=9999"));
        assert_eq!(page["limit"], 200);
    }

    // ─── Phase 6 — §12 lifecycle + feedback observability events ────────

    #[actix_web::test]
    async fn lifecycle_seams_emit_exactly_once_per_action() {
        use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;

        let (_tmp, api) = api();
        let funnel = AttentionFunnelStore::open_in_temp();
        api.service().set_attention_funnel_store(funnel.clone());
        let service = build_app!(api);

        // Created (once per monitor).
        let created = create_monitor!(
            service,
            serde_json::json!({
                "spec": fixture_spec_json(),
                "schedule": interval_schedule_json()
            })
        );
        let task_id = created["task_id"].as_str().expect("task_id").to_string();
        assert_eq!(count_trace_events(&funnel, "monitor_created").await, 1);

        // Updated (once per edit action; a second edit is a second action).
        for objective in ["Watch A", "Watch B"] {
            let mut spec = fixture_spec_json();
            spec["objective"] = Value::String(objective.to_string());
            let req = scoped(
                test::TestRequest::patch().uri(&format!("/monitors/{task_id}")),
                TEST_PRINCIPAL,
            )
            .set_json(serde_json::json!({ "spec": spec }))
            .to_request();
            let resp = test::call_service(&service, req).await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
        assert_eq!(count_trace_events(&funnel, "monitor_updated").await, 2);

        // Paused / resumed (once per toggle).
        let resp = test::call_service(
            &service,
            scoped_post(&format!("/monitors/{task_id}/pause"), serde_json::json!({})).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{task_id}/resume"),
                serde_json::json!({}),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(count_trace_events(&funnel, "monitor_paused").await, 1);
        assert_eq!(count_trace_events(&funnel, "monitor_resumed").await, 1);
        // No cross-contamination.
        assert_eq!(count_trace_events(&funnel, "monitor_created").await, 1);
    }

    #[actix_web::test]
    async fn feedback_event_emits_once_and_replay_emits_nothing() {
        use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;

        let (_tmp, api) = api();
        let funnel = AttentionFunnelStore::open_in_temp();
        api.service().set_attention_funnel_store(funnel.clone());
        let service = build_app!(api);
        let (task_id, updates) = seed_monitor_with_two_updates(&api).await;
        let update_id = updates[0].update_id.clone();

        let post = |verdict: &'static str| {
            scoped_post(
                &format!("/monitors/{task_id}/updates/{update_id}/feedback"),
                serde_json::json!({ "verdict": verdict }),
            )
            .to_request()
        };
        let resp = test::call_service(&service, post("useful")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            count_trace_events(&funnel, "monitor_feedback_recorded").await,
            1
        );
        // Idempotent replay of the SAME verdict emits NOTHING new.
        let resp = test::call_service(&service, post("useful")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            count_trace_events(&funnel, "monitor_feedback_recorded").await,
            1
        );
        // A flip IS a new action → a second event.
        let resp = test::call_service(&service, post("not_relevant")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            count_trace_events(&funnel, "monitor_feedback_recorded").await,
            2
        );
    }

    // ─── Phase 6 — metrics (§12) ─────────────────────────────────────────

    /// Pure-math coverage for `aggregate_monitor_metrics` (the handler only
    /// assembles inputs).
    #[actix_web::test]
    async fn monitor_metrics_math_over_synthetic_windows() {
        use magician::magician_v2::monitors::monitor_run::{
            MonitorAccessProblemV1, MonitorCountsV1, MonitorSourceOutcomeV1,
        };

        let spec = |policy: &str| -> MonitorSpecV1 {
            let mut spec: MonitorSpecV1 =
                serde_json::from_value(fixture_spec_json()).expect("spec decodes");
            spec.notification_policy =
                serde_json::from_value(serde_json::json!(policy)).expect("policy decodes");
            spec
        };
        let run = |status: MonitorRunStatus, source_ok: bool| -> MonitorRunResultV1 {
            MonitorRunResultV1 {
                monitor_task_id: "task_m".to_string(),
                execution_id: format!("exec_{}", uuid::Uuid::new_v4()),
                monitor_revision: 1,
                started_at: "2026-07-22T06:00:00Z".to_string(),
                completed_at: "2026-07-22T06:01:00Z".to_string(),
                status,
                complete_scan: source_ok,
                source_outcomes: vec![MonitorSourceOutcomeV1 {
                    source: "https://acme.example".to_string(),
                    status: if source_ok {
                        MonitorSourceOutcomeStatus::Ok
                    } else {
                        MonitorSourceOutcomeStatus::AuthFailed
                    },
                    complete: source_ok,
                    items_scanned: 0,
                    note: None,
                }],
                counts: MonitorCountsV1 {
                    scanned: 0,
                    new: 0,
                    updated: 0,
                    unchanged: 0,
                    possibly_removed: 0,
                },
                findings: Vec::new(),
                run_fingerprint: "rf_0000000000000000".to_string(),
                change_fingerprint: (status == MonitorRunStatus::Changed)
                    .then(|| "chg_0000000000000000".to_string()),
                access_problem: (!source_ok).then(|| MonitorAccessProblemV1 {
                    source: "https://acme.example".to_string(),
                    kind: MonitorSourceOutcomeStatus::AuthFailed,
                    message: "login".to_string(),
                    since: "2026-07-22T06:00:00Z".to_string(),
                }),
            }
        };

        // Empty scope: everything zero, never NaN.
        let empty = aggregate_monitor_metrics(&[]);
        assert_eq!(empty.monitors, 0);
        assert_eq!(empty.counts.active, 0);
        assert_eq!(empty.material_change_rate, 0.0);
        assert_eq!(empty.suppression_rate, 0.0);
        assert_eq!(empty.source_failure_rate, 0.0);
        assert_eq!(empty.window.runs_considered, 0);

        // Monitor A (active, material_changes): newest run degraded, then
        // changed, unchanged, failed → 4 runs: 1 material; suppression =
        // 3/4 (only changed notifies); source failures = degraded + failed
        // = 2/4. Monitor B (paused, every_run): one unchanged run — every
        // run notifies → 0 suppressed there.
        let inputs = vec![
            MonitorMetricsInput {
                paused: false,
                spec: spec("material_changes"),
                runs: vec![
                    run(MonitorRunStatus::Degraded, false),
                    run(MonitorRunStatus::Changed, true),
                    run(MonitorRunStatus::Unchanged, true),
                    run(MonitorRunStatus::Failed, true),
                ],
            },
            MonitorMetricsInput {
                paused: true,
                spec: spec("every_run"),
                runs: vec![run(MonitorRunStatus::Unchanged, true)],
            },
        ];
        let metrics = aggregate_monitor_metrics(&inputs);
        assert_eq!(metrics.monitors, 2);
        assert_eq!(metrics.counts.active, 1);
        assert_eq!(metrics.counts.paused, 1);
        assert_eq!(metrics.counts.degraded_last_run, 1, "newest run of A");
        assert_eq!(metrics.counts.failed_last_run, 0, "failed is not newest");
        assert_eq!(metrics.window.runs_considered, 5);
        assert_eq!(metrics.window.runs_per_monitor, MONITOR_METRICS_RUN_WINDOW);
        assert!((metrics.material_change_rate - 1.0 / 5.0).abs() < 1e-9);
        // Suppressed: A's unchanged + degraded + failed = 3; B notifies.
        assert!((metrics.suppression_rate - 3.0 / 5.0).abs() < 1e-9);
        // Failures: A's degraded (auth outcome) + A's failed marker = 2.
        assert!((metrics.source_failure_rate - 2.0 / 5.0).abs() < 1e-9);

        // Window bound: more than N runs only counts the newest N.
        let many: Vec<MonitorRunResultV1> = (0..MONITOR_METRICS_RUN_WINDOW + 10)
            .map(|_| run(MonitorRunStatus::Unchanged, true))
            .collect();
        let metrics = aggregate_monitor_metrics(&[MonitorMetricsInput {
            paused: false,
            spec: spec("material_changes"),
            runs: many,
        }]);
        assert_eq!(metrics.window.runs_considered, MONITOR_METRICS_RUN_WINDOW);
    }

    #[actix_web::test]
    async fn monitors_metrics_endpoint_reads_the_real_ledger() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        // Empty scope first.
        let resp = test::call_service(&service, scoped_get("/monitors-metrics").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let empty: Value = test::read_body_json(resp).await;
        assert_eq!(empty["monitors"], 0);
        assert_eq!(empty["counts"]["active"], 0);
        assert_eq!(empty["window"]["runs_considered"], 0);

        // Seeded monitor: baseline (quiet) + changed (notifies) — both
        // complete-ok scans.
        let (_task_id, _updates) = seed_monitor_with_two_updates(&api).await;
        let resp = test::call_service(&service, scoped_get("/monitors-metrics").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let metrics: Value = test::read_body_json(resp).await;
        assert_eq!(metrics["monitors"], 1);
        assert_eq!(metrics["counts"]["active"], 1);
        assert_eq!(metrics["counts"]["paused"], 0);
        assert_eq!(metrics["counts"]["degraded_last_run"], 0);
        assert_eq!(metrics["counts"]["failed_last_run"], 0);
        assert_eq!(metrics["window"]["runs_considered"], 2);
        assert_eq!(metrics["window"]["runs_per_monitor"], 50);
        // 1 of 2 runs material; the quiet baseline is the 1 suppressed run;
        // no source failures anywhere.
        assert_eq!(metrics["material_change_rate"].as_f64(), Some(0.5));
        assert_eq!(metrics["suppression_rate"].as_f64(), Some(0.5));
        assert_eq!(metrics["source_failure_rate"].as_f64(), Some(0.0));

        // Scope isolation: a foreign principal sees an empty aggregate.
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::get().uri("/monitors-metrics"),
                "someone-else",
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let foreign: Value = test::read_body_json(resp).await;
        assert_eq!(foreign["monitors"], 0);
    }

    #[actix_web::test]
    async fn plain_tasks_are_never_monitors() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        // A generic task minted through the ordinary create path…
        let plain = api
            .service()
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "Plain task".to_string(),
                description: "Not a monitor".to_string(),
                agent_id: "personal-assistant".to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: magician::magician_v2::artifact_v2::models::TaskOutputMode::default(),
                chat_session_id: None,
                lifecycle: magician::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
                sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("plain task creates");
        let plain_id = plain.manifest.task_id;

        // …never appears in /monitors and 404s on every monitor route.
        let page = list_page!(service, "/monitors");
        assert!(page["items"].as_array().expect("items").is_empty());
        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{plain_id}")).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "monitor_not_found");
        let resp = test::call_service(
            &service,
            scoped_post(
                &format!("/monitors/{plain_id}/pause"),
                serde_json::json!({}),
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ─── Phase 7 — explicit conversion (POST /monitors/{task_id}/convert) ──

    /// Create a plain task through the ordinary create path (the state every
    /// pre-monitor recurring task is in).
    async fn create_plain_task(
        api: &web::Data<TaskApiV3>,
        title: &str,
        schedule: Option<Value>,
        lifecycle: TaskLifecycle,
    ) -> TaskRecord {
        api.service()
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: title.to_string(),
                description: "Check the Acme pricing page for plan changes".to_string(),
                agent_id: "personal-assistant".to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule,
                output_mode: magician::magician_v2::artifact_v2::models::TaskOutputMode::default(),
                chat_session_id: None,
                lifecycle,
                sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("plain task creates")
    }

    fn convert_body(title: Option<&str>) -> Value {
        let mut body = serde_json::json!({ "spec": fixture_spec_json() });
        if let Some(title) = title {
            body["title"] = Value::String(title.to_string());
        }
        body
    }

    /// POST /monitors/{task_id}/convert and return `(status, body)`.
    macro_rules! post_convert {
        ($service:expr, $task_id:expr, $body:expr) => {{
            let resp = test::call_service(
                &$service,
                scoped_post(&format!("/monitors/{}/convert", $task_id), $body).to_request(),
            )
            .await;
            let status = resp.status();
            let body: Value = test::read_body_json(resp).await;
            (status, body)
        }};
    }

    /// Unit-pin the §12 payload builder: the funnel observability read does
    /// not expose event metadata, so the `converted: true` flag is asserted
    /// at the builder the convert seam records through.
    #[actix_web::test]
    async fn converted_event_payload_carries_the_converted_flag() {
        let (_tmp, api) = api();
        let task = create_plain_task(
            &api,
            "Pricing watch",
            Some(interval_schedule_json()),
            TaskLifecycle::Persistent,
        )
        .await;
        let payload = converted_monitor_event_payload(&task);
        assert_eq!(payload["converted"], true);
        assert_eq!(payload["has_schedule"], true);
        assert_eq!(payload["agent_id"], "personal-assistant");
    }

    #[actix_web::test]
    async fn convert_attaches_spec_and_keeps_identity_schedule_and_history() {
        use magician::magician_v2::artifact_v2::models::{
            ExecutionRecord, ExecutionRefs, ExecutionState, TaskOutputMode,
        };

        let (_tmp, api) = api();
        let service = build_app!(api);
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        let plain = create_plain_task(
            &api,
            "Acme pricing check",
            Some(interval_schedule_json()),
            TaskLifecycle::Persistent,
        )
        .await;
        let task_id = plain.manifest.task_id.clone();
        let original_created_at = plain.manifest.created_at.clone();

        // Real pre-conversion run history: one completed root execution
        // registered the way pipeline discovery does — through the API's own
        // service, so the write reaches the same reconciler the conversion
        // and its assertions read through. A reducer built here over a second
        // workspace handle would carry a second `TaskWriteReconciler`, which
        // is the stale-index bug in test form.
        api.service()
            .reducer()
            .reduce_execution_discovered(
                &scope,
                &ExecutionRecord {
                    state: ExecutionState {
                        execution_id: "exec_before_convert".to_string(),
                        task_id: task_id.clone(),
                        root_execution_id: Some("exec_before_convert".to_string()),
                        parent_execution_id: None,
                        agent_id: "personal-assistant".to_string(),
                        relationship_type: "root".to_string(),
                        status: "completed".to_string(),
                        completion_kind: None,
                        open_items: Vec::new(),
                        plan_id: None,
                        primary_execution_output_id: None,
                        active_child_execution_ids: Vec::new(),
                        started_at: "2026-07-22T06:00:00Z".to_string(),
                        completed_at: Some("2026-07-22T06:01:00Z".to_string()),
                        updated_at: "2026-07-22T06:01:00Z".to_string(),
                        completed_step_ids: Vec::new(),
                        failed_step_ids: Vec::new(),
                        current_step_id: None,
                        task_output_mode: TaskOutputMode::default(),
                        refinement: None,
                        synthesis_pending: false,
                        synthesis_failed: None,
                    },
                    refs: ExecutionRefs {
                        execution_id: "exec_before_convert".to_string(),
                        ..Default::default()
                    },
                },
            )
            .await
            .expect("execution discovers");

        // Convert with an explicit retitle — the FIXED contract response.
        let (status, body) =
            post_convert!(service, task_id, convert_body(Some("Acme pricing monitor")));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["task_id"], task_id.as_str());
        assert_eq!(body["monitor_revision"], 1, "first spec write is rev 1");
        assert_eq!(body["converted"], true);

        // Same task id, same manifest lineage: spec attached, schedule and
        // created_at untouched, description untouched, title replaced.
        let raw = api
            .service()
            .get_task(&scope, &task_id)
            .await
            .expect("task loads");
        assert_eq!(raw.manifest.monitor_revision, 1);
        assert!(raw.manifest.monitor_spec.is_some());
        assert_eq!(raw.manifest.title, "Acme pricing monitor");
        assert_eq!(
            raw.manifest.description,
            "Check the Acme pricing page for plan changes"
        );
        assert_eq!(raw.manifest.created_at, original_created_at);
        assert_eq!(
            raw.manifest.schedule.as_ref().expect("schedule kept"),
            &interval_schedule_json()
        );

        // Pre-conversion execution history survives.
        let executions = api
            .service()
            .list_executions(&scope, &task_id)
            .await
            .expect("executions list");
        assert!(
            executions
                .iter()
                .any(|execution| execution.execution_id == "exec_before_convert"),
            "conversion must not touch execution history"
        );

        // The converted task now appears on /monitors with the ORIGINAL
        // schedule's cadence, and the detail serves that schedule.
        let page = list_page!(service, "/monitors");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["task_id"], task_id.as_str());
        assert_eq!(items[0]["cadence_summary"], "Every 3600s");
        assert_eq!(items[0]["monitor_revision"], 1);
        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{task_id}")).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let detail: Value = test::read_body_json(resp).await;
        assert_eq!(detail["schedule"], interval_schedule_json());
        assert_eq!(detail["title"], "Acme pricing monitor");
    }

    #[actix_web::test]
    async fn convert_without_schedule_or_title_is_run_on_demand() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let plain =
            create_plain_task(&api, "Unscheduled check", None, TaskLifecycle::Persistent).await;
        let task_id = plain.manifest.task_id.clone();

        // A missing schedule is ALLOWED — the monitor is run-on-demand.
        let (status, body) = post_convert!(service, task_id, convert_body(None));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["monitor_revision"], 1);
        assert_eq!(body["converted"], true);

        let page = list_page!(service, "/monitors");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["cadence_summary"], "unscheduled");
        // No title in the body → the task keeps its own.
        assert_eq!(items[0]["title"], "Unscheduled check");
    }

    #[actix_web::test]
    async fn convert_conflicts_for_existing_and_archived_monitors() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        // Already a monitor (freshly created through POST /monitors).
        let created = create_monitor!(
            service,
            serde_json::json!({
                "spec": fixture_spec_json(),
                "schedule": interval_schedule_json()
            })
        );
        let monitor_id = created["task_id"].as_str().expect("task_id").to_string();
        let (status, body) = post_convert!(service, monitor_id, convert_body(None));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "monitor_already_exists");

        // Soft-archived former monitor: still 409 monitor_already_exists —
        // the archive keeps the manifest (spec + dedupe ledger survive
        // restore), so it is never "eligible again".
        let resp = test::call_service(
            &service,
            scoped(
                test::TestRequest::delete().uri(&format!("/monitors/{monitor_id}")),
                TEST_PRINCIPAL,
            )
            .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let (status, body) = post_convert!(service, monitor_id, convert_body(None));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "monitor_already_exists");
    }

    #[actix_web::test]
    async fn convert_refuses_internal_archived_and_missing_tasks() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );

        // Internal-lifecycle (chat/runtime transient) → not eligible.
        let internal =
            create_plain_task(&api, "Internal dispatch", None, TaskLifecycle::Internal).await;
        let (status, body) = post_convert!(service, internal.manifest.task_id, convert_body(None));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "task_not_eligible_for_monitor");

        // Archived plain task (never a monitor) → not eligible.
        let archived =
            create_plain_task(&api, "Archived task", None, TaskLifecycle::Persistent).await;
        api.service()
            .update_task_status(&scope, &archived.manifest.task_id, "archived")
            .await
            .expect("archives");
        let (status, body) = post_convert!(service, archived.manifest.task_id, convert_body(None));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "task_not_eligible_for_monitor");

        // Missing task → the canonical task 404 (this route addresses a
        // TASK, not a monitor).
        let (status, body) = post_convert!(service, "task_missing", convert_body(None));
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "task_not_found");
    }

    #[actix_web::test]
    async fn convert_rejects_invalid_specs_and_leaves_the_task_plain() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        let plain = create_plain_task(
            &api,
            "Still plain",
            Some(interval_schedule_json()),
            TaskLifecycle::Persistent,
        )
        .await;
        let task_id = plain.manifest.task_id.clone();

        // Unknown schema version → the standard Phase 1 admission reason.
        let mut versioned = fixture_spec_json();
        versioned["schema_version"] = serde_json::json!(2);
        let (status, body) =
            post_convert!(service, task_id, serde_json::json!({ "spec": versioned }));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "monitor_schema_version_unsupported");

        // No surviving sources → monitor_sources_required.
        let mut sourceless = fixture_spec_json();
        sourceless["sources"] = serde_json::json!({
            "urls": [], "domains": [], "authenticated_sources": []
        });
        sourceless["query_seeds"] = serde_json::json!([]);
        let (status, body) =
            post_convert!(service, task_id, serde_json::json!({ "spec": sourceless }));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "monitor_sources_required");

        // Refusal leaves the task exactly as it was: plain, revision 0,
        // absent from every monitor surface.
        let raw = api
            .service()
            .get_task(&scope, &task_id)
            .await
            .expect("task loads");
        assert!(raw.manifest.monitor_spec.is_none());
        assert_eq!(raw.manifest.monitor_revision, 0);
        let page = list_page!(service, "/monitors");
        assert!(page["items"].as_array().expect("items").is_empty());
        let resp = test::call_service(
            &service,
            scoped_get(&format!("/monitors/{task_id}")).to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn convert_emits_monitor_created_once_and_generic_tasks_stay_untouched() {
        use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;

        let (_tmp, api) = api();
        let funnel = AttentionFunnelStore::open_in_temp();
        api.service().set_attention_funnel_store(funnel.clone());
        let service = build_app!(api);
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );

        let convertee = create_plain_task(
            &api,
            "Watched task",
            Some(interval_schedule_json()),
            TaskLifecycle::Persistent,
        )
        .await;
        let bystander =
            create_plain_task(&api, "Bystander task", None, TaskLifecycle::Persistent).await;

        // Creating plain tasks emits no monitor lifecycle events.
        assert_eq!(count_trace_events(&funnel, "monitor_created").await, 0);

        let (status, _body) =
            post_convert!(service, convertee.manifest.task_id, convert_body(None));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(count_trace_events(&funnel, "monitor_created").await, 1);

        // Replay safety: a second convert is a 409 and emits nothing new;
        // the durable funnel row (deterministic id over the task id) would
        // also swallow any same-action replay via INSERT OR IGNORE.
        let (status, _body) =
            post_convert!(service, convertee.manifest.task_id, convert_body(None));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(count_trace_events(&funnel, "monitor_created").await, 1);

        // Zero regression for the untouched generic task: still plain,
        // revision 0, absent from /monitors, present on the generic listing
        // with NO monitor_revision key on the wire (serde skip while 0).
        let raw = api
            .service()
            .get_task(&scope, &bystander.manifest.task_id)
            .await
            .expect("bystander loads");
        assert!(raw.manifest.monitor_spec.is_none());
        assert_eq!(raw.manifest.monitor_revision, 0);
        let page = list_page!(service, "/monitors");
        let items = page["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["task_id"], convertee.manifest.task_id.as_str());
        let listed = api.service().list_tasks(&scope).await.expect("tasks list");
        let bystander_row = listed
            .iter()
            .find(|item| item.id == bystander.manifest.task_id)
            .expect("bystander listed");
        assert_eq!(bystander_row.monitor_revision, 0);
        let wire = serde_json::to_value(bystander_row).expect("row serializes");
        assert!(
            wire.get("monitor_revision").is_none(),
            "plain-task rows stay byte-identical (monitor_revision omitted while 0)"
        );
        let convertee_row = listed
            .iter()
            .find(|item| item.id == convertee.manifest.task_id)
            .expect("convertee listed");
        assert_eq!(
            convertee_row.monitor_revision, 1,
            "task-list rows carry the discriminator clients derive eligibility from"
        );
    }
}
