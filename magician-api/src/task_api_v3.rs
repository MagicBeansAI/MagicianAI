use std::{sync::Arc, time::Instant};

use actix_web::{http::header::HeaderMap, web, HttpRequest, HttpResponse, Result};
pub use magician::magician_v2::agents::task_ownership::resolve_created_task_owner_agent_id;
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope_ref;
use crate::task_lanes::{lane_counts, LaneTask, TaskLane};
use magician::magician_v2::agents::{
    disabled_agent_hierarchy, AgentInvocationPolicy, InvocationSurface, TrustPolicyEnforcer,
};
use magician::magician_v2::artifact_v2::{
    models::{
        PublishSurfaceInput, PublishedSurfacePlacement, PublishedSurfaceProjectionFilter,
        RepublishSurfaceInput, TaskListItemV3, TaskOutputMode, TaskPlanRecord, TaskPlanStatus,
    },
    service::APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED,
    ArtifactV2Error, ArtifactV2Service, CreateTaskInput, ScopeRef, UpdateTaskInput, V3ReadApi,
};
use magician::magician_v2::execution::agent_resources::AgentResources;
use magician::magician_v2::hitl::{HitlOpenIdentifiers, HitlOpenScope, HitlOpenTarget};
use magician::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides;
use magician::magician_v2::storage::{
    epoch_millis_from_rfc3339, row_follows_cursor, ListCursor, ListKind, ListPageQuery,
};

#[derive(Debug, Deserialize)]
pub struct CreateTaskV3Request {
    pub workspace: Option<String>,
    pub ui_thread_id: Option<String>,
    pub title: Option<String>,
    pub description: String,
    pub agent_id: Option<String>,
    pub priority: Option<String>,
    pub due_date: Option<String>,
    pub tags: Option<Vec<magician::magician_v2::artifact_v2::models::TaskTagRecord>>,
    pub created_by: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub reference_task_ids: Option<Vec<String>>,
    pub approved: Option<bool>,
    pub schedule: Option<serde_json::Value>,
    pub output_mode: Option<TaskOutputMode>,
    /// Intent flag for VibeDev runs: when `true`, the run is saved as a
    /// user-visible (`Persistent`) task; when absent/`false`, a VibeDev run is
    /// created `Internal` (off the `/tasks` feed — a cockpit-coupled execution,
    /// not a tracked deliverable). Ignored for non-VibeDev creates, which keep
    /// the `Persistent` default.
    pub save_as_task: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskV3Request {
    pub workspace: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub agent_id: Option<String>,
    pub ui_thread_id: Option<String>,
    pub priority: Option<Option<String>>,
    pub due_date: Option<Option<String>>,
    pub tags: Option<Vec<magician::magician_v2::artifact_v2::models::TaskTagRecord>>,
    pub created_by: Option<String>,
    pub depends_on: Option<Vec<String>>,
    pub approved: Option<bool>,
    pub schedule: Option<Option<serde_json::Value>>,
    pub output_mode: Option<TaskOutputMode>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskStatusV3Request {
    pub status: String,
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ExecuteTaskV3Request {
    #[serde(default)]
    pub refinement: Option<String>,
    #[serde(default)]
    pub overwrite: Option<bool>,
    /// Optional execution-local model routing. This is intentionally attached
    /// to the execution request rather than the durable task manifest, so eval
    /// and diagnostic runs can choose a cheaper profile without changing the
    /// production agent definition used by later runs of the same task.
    #[serde(default)]
    pub llm_routing_overrides: Option<OperationRoutingOverrides>,
    /// Optional explicit first owner transition. The server validates and
    /// dispatches this through the normal delegation policy; it does not ask
    /// the root model to infer a route the caller has already selected.
    #[serde(default)]
    pub delegate_to_agent: Option<String>,
}

impl ExecuteTaskV3Request {
    /// The routing overrides a client may hand a run: its endpoints only.
    /// The flow's parent engine is named by the runtime from the run's own
    /// engine, never by the request, so a `parent_engine` on the wire is
    /// dropped before the overrides are stored or sealed.
    pub(crate) fn client_llm_routing_overrides(&self) -> Option<OperationRoutingOverrides> {
        self.llm_routing_overrides
            .clone()
            .map(OperationRoutingOverrides::without_parent_engine)
    }
}

#[cfg(test)]
mod execute_request_tests {
    use super::*;

    #[test]
    fn execution_local_profile_override_deserializes() {
        let request: ExecuteTaskV3Request = serde_json::from_value(serde_json::json!({
            "llm_routing_overrides": {
                "operations": {
                    "agentic_decision": {"profile": "gpt6luna-responses-toolsany"}
                }
            }
        }))
        .expect("valid execute request");

        assert_eq!(
            request
                .llm_routing_overrides
                .as_ref()
                .and_then(|overrides| overrides.operations.get("agentic_decision"))
                .and_then(|endpoint| endpoint.profile_name()),
            Some("gpt6luna-responses-toolsany")
        );
    }

    #[test]
    fn explicit_delegation_route_deserializes() {
        let request: ExecuteTaskV3Request = serde_json::from_value(serde_json::json!({
            "delegate_to_agent": "web-researcher"
        }))
        .expect("valid execute request");

        assert_eq!(request.delegate_to_agent.as_deref(), Some("web-researcher"));
    }

    #[test]
    fn a_client_never_names_the_parent_engine() {
        let request: ExecuteTaskV3Request = serde_json::from_value(serde_json::json!({
            "llm_routing_overrides": {
                "parent_engine": "grok",
                "operations": {
                    "agentic_decision": {"profile": "gpt6luna-responses-toolsany"}
                }
            }
        }))
        .expect("valid execute request");
        assert_eq!(
            request
                .llm_routing_overrides
                .as_ref()
                .and_then(|overrides| overrides.parent_engine.as_deref()),
            Some("grok"),
            "the field deserialises; the strip is the ingress's, not serde's"
        );

        let admitted = request
            .client_llm_routing_overrides()
            .expect("the endpoint override is kept");
        assert_eq!(admitted.parent_engine, None);
        assert_eq!(
            admitted
                .operations
                .get("agentic_decision")
                .and_then(|endpoint| endpoint.profile_name()),
            Some("gpt6luna-responses-toolsany")
        );

        let parent_only: ExecuteTaskV3Request = serde_json::from_value(serde_json::json!({
            "llm_routing_overrides": {"parent_engine": "grok"}
        }))
        .expect("valid execute request");
        assert_eq!(
            parent_only
                .client_llm_routing_overrides()
                .and_then(OperationRoutingOverrides::normalized),
            None,
            "a parent alone leaves the run with no overrides at all"
        );
    }
}

#[derive(Debug, Deserialize)]
pub struct SubmitTaskPlanClarificationV3Request {
    pub workspace: Option<String>,
    pub response_text: String,
}

#[derive(Debug, Deserialize)]
pub struct ResumeTaskPlanClarificationsV3Request {
    pub workspace: Option<String>,
    #[serde(default)]
    pub slots: Option<Vec<magician::magician_v2::ask_loop::api::ManualResumeSlot>>,
    #[serde(default)]
    pub updated_confidence: Option<f64>,
    #[serde(default)]
    pub handled_question: Option<String>,
}

impl From<ResumeTaskPlanClarificationsV3Request>
    for magician::magician_v2::ask_loop::api::ManualResumeRequest
{
    fn from(request: ResumeTaskPlanClarificationsV3Request) -> Self {
        Self {
            slots: request.slots,
            updated_confidence: request.updated_confidence,
            handled_question: request.handled_question,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskPlanV3Request {
    pub plan: magician::magician_v2::strategy::plan::PlanGraph,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TaskExecutionPageQuery {
    pub workspace: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    pub workspace: Option<String>,
    /// Exact immutable plan revision for approve/reject compare-and-set.
    pub plan_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadTaskResultRequest {
    pub result_ref: String,
    #[serde(default)]
    pub execution_id: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub field_paths: Vec<String>,
    #[serde(default = "default_task_result_page_records")]
    pub max_records: usize,
}

fn default_task_result_page_records() -> usize {
    20
}

struct TaskResultReadPolicyGuard {
    level: magician::magician_v2::agents::TrustLevel,
    enforcer: TrustPolicyEnforcer,
}

impl magician::magician_v2::tool_result_materialization::ResultReadPolicyGuard
    for TaskResultReadPolicyGuard
{
    fn permits(&self, tool: &str, action: &str) -> bool {
        self.enforcer.is_allowed(&self.level, tool, action)
    }
}

fn task_result_error_status(
    error: &magician::magician_v2::tool_result_materialization::CanonicalResultError,
) -> (actix_web::http::StatusCode, &'static str) {
    use magician::magician_v2::tool_result_materialization::CanonicalResultError;

    match error {
        CanonicalResultError::NotFound => {
            (actix_web::http::StatusCode::NOT_FOUND, "result_not_found")
        },
        CanonicalResultError::Revoked => (actix_web::http::StatusCode::FORBIDDEN, "result_revoked"),
        CanonicalResultError::Expired | CanonicalResultError::CursorExpired => {
            (actix_web::http::StatusCode::GONE, "result_expired")
        },
        CanonicalResultError::RecordTooLarge => (
            actix_web::http::StatusCode::PAYLOAD_TOO_LARGE,
            "result_record_too_large",
        ),
        CanonicalResultError::InvalidCursor | CanonicalResultError::InvalidRequest { .. } => (
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_result_request",
        ),
        CanonicalResultError::Corrupt | CanonicalResultError::IdentityConflict => {
            (actix_web::http::StatusCode::CONFLICT, "result_corrupt")
        },
        CanonicalResultError::AuthorityUnavailable | CanonicalResultError::StorageUnavailable => (
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "result_temporarily_unavailable",
        ),
    }
}

#[cfg(test)]
mod task_result_api_contract_tests {
    use super::*;

    use magician::magician_v2::tool_result_materialization::CanonicalResultError;

    #[test]
    fn complete_result_failures_have_stable_non_disclosing_http_statuses() {
        for (error, expected_status, expected_code) in [
            (
                CanonicalResultError::NotFound,
                actix_web::http::StatusCode::NOT_FOUND,
                "result_not_found",
            ),
            (
                CanonicalResultError::Revoked,
                actix_web::http::StatusCode::FORBIDDEN,
                "result_revoked",
            ),
            (
                CanonicalResultError::CursorExpired,
                actix_web::http::StatusCode::GONE,
                "result_expired",
            ),
            (
                CanonicalResultError::RecordTooLarge,
                actix_web::http::StatusCode::PAYLOAD_TOO_LARGE,
                "result_record_too_large",
            ),
            (
                CanonicalResultError::InvalidRequest { code: "fixture" },
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_result_request",
            ),
            (
                CanonicalResultError::StorageUnavailable,
                actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                "result_temporarily_unavailable",
            ),
        ] {
            assert_eq!(
                task_result_error_status(&error),
                (expected_status, expected_code)
            );
        }
    }
}

/// Query params for `GET /api/magician/v3/tasks`.
///
/// Pagination is OPT-IN and fully backward compatible: with no `limit`/
/// `offset` the endpoint behaves exactly as before (the whole pool, plain
/// `{tasks}` envelope — taskStore, iOS and every existing consumer keep
/// working unchanged). Passing `limit` (and optionally `offset`, `status`,
/// `sort`, `order`, `query`) filters/sorts/pages server-side and ADDS a
/// `pagination` object beside `tasks` — additive, so even paginated
/// responses still parse for a `{tasks}`-only reader.
#[derive(Debug, Deserialize)]
pub struct TasksListQueryV3 {
    pub workspace: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub status: Option<String>,
    pub query: Option<String>,
    /// One of the six lane names. Absent means no lane filter — the
    /// pre-existing behaviour, so every current caller keeps working.
    pub view: Option<String>,
    /// `YYYY-MM-DD` in the reader's timezone. Required only by
    /// `today`/`overdue`, and required for `counts` to be reported at all
    /// (two of the six lanes are date lanes the server cannot compute
    /// without it).
    pub today: Option<String>,
    /// A keyset position from a previous page's `pagination.next_cursor`.
    /// Supersedes `offset` when both are given — seeking and counting to the
    /// same place at once would page twice.
    ///
    /// New in this phase, which is why the legacy unpaginated branch tests
    /// for it too: a pre-pagination consumer sends none of the three and
    /// keeps its bare `{tasks}` body.
    pub cursor: Option<String>,
}

/// Narrow a stored task down to the slice a lane is allowed to read.
///
/// The adaptation lives here, at the boundary, rather than widening
/// `LaneTask` to carry the storage model's tag records: a lane only ever
/// asks whether a task is tagged, not what the tags are.
fn lane_task_slice(task: &magician::magician_v2::artifact_v2::models::TaskListItemV3) -> LaneTask {
    LaneTask {
        status: task.status.clone(),
        tags: task.tags.iter().map(|tag| tag.name.clone()).collect(),
        due_date: task.due_date.clone(),
    }
}

#[derive(Debug, Deserialize)]
pub struct DeleteTaskV3Query {
    pub workspace: Option<String>,
    #[serde(default)]
    pub remove_files: bool,
}

#[derive(Debug, Deserialize)]
pub struct TaskPlanVersionsQueryV3 {
    pub workspace: Option<String>,
    pub before: Option<i64>,
    pub limit: Option<usize>,
}

/// Query params for `GET /api/magician/v3/tasks/internal`. Powers the
/// server-side pagination / sort / filter on the `/internal-tasks` data
/// table. All params optional except scope. `sort` accepts
/// `updated_at | created_at | title | agent_id | status` (default
/// `updated_at`). `order` accepts `asc | desc` (default `desc`).
#[derive(Debug, Deserialize)]
pub struct InternalTasksQueryV3 {
    pub workspace: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub agent_id: Option<String>,
    pub status: Option<String>,
    pub query: Option<String>,
    /// Restrict to one `ui_thread_id` — lets a surface (e.g. the VibeDev cockpit,
    /// `ui_thread_id = "vibedev"`) pull only its OWN internal runs instead of the
    /// whole internal pool.
    pub ui_thread_id: Option<String>,
    /// A keyset position from a previous page's `pagination.next_cursor`.
    /// Supersedes `offset` when both are given.
    pub cursor: Option<String>,
}

/// The sort the tasks/internal handlers will ACTUALLY apply.
///
/// Spelled out because both handlers fall through to `updated_at` for
/// anything they do not recognise. The index fast path has to agree with
/// that fall-through, or a mistyped `sort=` would be served in one order by
/// the index and another by the walk — the same list, ordered two ways,
/// neither of them visibly wrong.
fn effective_task_sort(sort: Option<&str>) -> &'static str {
    match sort.unwrap_or("updated_at") {
        "title" => "title",
        "status" => "status",
        "created_at" => "created_at",
        "agent_id" => "agent_id",
        _ => "updated_at",
    }
}

/// Whether the request asks for the keyset order every cursor — and the
/// index — is defined against: `updated_at`, descending.
///
/// Both handlers treat any `order` that is not `asc` as `desc`, so this does
/// too.
fn task_order_is_keyset(sort: Option<&str>, order: Option<&str>) -> bool {
    effective_task_sort(sort) == "updated_at" && order.unwrap_or("desc") != "asc"
}

/// The offset the walk must start at to honour a keyset cursor.
///
/// The walk sorts on the same key the index does, so a cursor means the same
/// thing to both: the first row that sorts strictly after it. A cursor whose
/// row was deleted between pages therefore lands on the next surviving row
/// rather than erroring, and a cursor past the end yields an empty page.
fn keyset_start_offset(tasks: &[TaskListItemV3], cursor: &ListCursor) -> usize {
    tasks
        .iter()
        .position(|task| {
            let updated_at = epoch_millis_from_rfc3339(&task.updated_at).unwrap_or(0);
            row_follows_cursor(cursor, updated_at, &task.id)
        })
        .unwrap_or(tasks.len())
}

/// The cursor a client sends back for the next page, in the one wire form
/// `ListCursor` defines.
///
/// `None` when there is no next page, and `None` when the request asked for
/// an order that is not the keyset order: a cursor minted against a
/// title-sorted page would seek by timestamp on the way back and silently
/// return a different slice of the corpus.
fn next_keyset_cursor(
    page: &[TaskListItemV3],
    has_more: bool,
    order_is_keyset: bool,
) -> Option<String> {
    if !has_more || !order_is_keyset {
        return None;
    }
    page.last().and_then(|task| {
        epoch_millis_from_rfc3339(&task.updated_at)
            .map(|updated_at| ListCursor::new(updated_at, task.id.clone()).encode())
    })
}

/// The one paginated task-list envelope, so the index path and the walk
/// cannot drift into two shapes a client would have to tell apart.
fn task_list_page_body(
    tasks: serde_json::Value,
    total: usize,
    limit: usize,
    offset: usize,
    has_more: bool,
    next_cursor: Option<String>,
    counts: Option<std::collections::BTreeMap<&'static str, usize>>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tasks": tasks,
        "pagination": {
            "total": total,
            "limit": limit,
            "offset": offset,
            "has_more": has_more,
            // Always present, `null` on the last page — the same convention
            // the monitors envelope uses, so a client never has to tell
            // "no more pages" from "this server does not do cursors".
            "next_cursor": next_cursor,
        }
    });
    if let Some(counts) = counts {
        body["counts"] = serde_json::json!(counts);
    }
    body
}

#[derive(Debug, Deserialize)]
pub struct PublishSurfaceV3Request {
    pub workspace: Option<String>,
    pub task_id: String,
    pub source_output_id: Option<String>,
    pub materialize_as: Option<String>,
    pub logical_surface_id: Option<String>,
    pub surface_kind: Option<String>,
    pub route: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub placement_kind: Option<String>,
    pub placement_id: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RepublishSurfaceV3Request {
    pub workspace: Option<String>,
    pub source_output_id: Option<String>,
    pub materialize_as: Option<String>,
    pub logical_surface_id: Option<String>,
    pub surface_kind: Option<String>,
    pub route: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub placement_kind: Option<String>,
    pub placement_id: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PublishedSurfaceProjectionQueryV3 {
    pub workspace: Option<String>,
    pub route: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub ui_thread_id: Option<String>,
    pub placement_kind: Option<String>,
    pub status: Option<String>,
    pub pinned_only: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PublishedSurfaceTopFeedQueryV3 {
    pub workspace: Option<String>,
    pub route: Option<String>,
    pub ui_thread_id: Option<String>,
    pub per_section: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TaskResponseV3 {
    pub task: magician::magician_v2::artifact_v2::models::TaskRecord,
    /// Durable verification projection (§5.1 of the verification-controller
    /// plan).
    ///
    /// Deliberately **derived here rather than persisted on `TaskState`**.
    /// The gate journal is already the durable source of truth; a
    /// denormalised copy on the task record would be a second source that can
    /// drift from it, and drift in this particular field means reporting code
    /// as checked when it was not.
    ///
    /// Missing state reads as `unknown`, never `verified` — automation that
    /// requires working code must check `task.state.status == "completed"`
    /// **and** `verification_state == "verified"`.
    pub verification_state: magician::magician_v2::execution::verification::VerificationState,
}

impl TaskResponseV3 {
    /// Build a response, resolving the verification projection for the task.
    ///
    /// The scope comes from the task's own manifest, so the projection can
    /// never be resolved against a different principal or workspace than the
    /// task belongs to.
    pub fn new(
        service: &magician::magician_v2::artifact_v2::service::ArtifactV2Service,
        task: magician::magician_v2::artifact_v2::models::TaskRecord,
    ) -> Self {
        let scope =
            magician::magician_v2::artifact_v2::service::ScopeRef::system_internal_unauthenticated(
                &task.manifest.principal.clone(),
                &task.manifest.workspace.clone(),
            );
        let verification_state =
            service.verification_state_for_task(&scope, &task.manifest.task_id);
        Self {
            task,
            verification_state,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TaskListResponseV3 {
    pub tasks: Vec<magician::magician_v2::artifact_v2::models::TaskListItemV3>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionResponseV3 {
    pub task: magician::magician_v2::artifact_v2::models::TaskRecord,
    pub execution: magician::magician_v2::artifact_v2::models::ExecutionRecord,
}

#[derive(Debug, Serialize)]
pub struct ExecutionListResponseV3 {
    pub executions: Vec<magician::magician_v2::artifact_v2::models::ExecutionIndexEntry>,
}

#[derive(Debug, Serialize)]
pub struct CancelExecutionResponseV3 {
    pub task: magician::magician_v2::artifact_v2::models::TaskRecord,
    pub execution: magician::magician_v2::artifact_v2::models::ExecutionRecord,
    pub cancelled: bool,
}

#[derive(Debug, Serialize)]
pub struct TaskRefsResponseV3 {
    pub refs: magician::magician_v2::artifact_v2::models::TaskRefs,
}

#[derive(Debug, Serialize)]
pub struct TaskOutputsResponseV3 {
    pub outputs: magician::magician_v2::artifact_v2::models::TaskOutputsRecord,
}

#[derive(Debug, Serialize)]
pub struct ExecutionRefsResponseV3 {
    pub refs: magician::magician_v2::artifact_v2::models::ExecutionRefs,
}

#[derive(Debug, Serialize)]
pub struct ExecutionOutputsResponseV3 {
    pub outputs: magician::magician_v2::artifact_v2::models::ExecutionOutputsRecord,
}

#[derive(Debug, Serialize)]
pub struct ExecutionDelegationsResponseV3 {
    pub delegations: magician::magician_v2::artifact_v2::models::DelegationReadinessRecord,
}

#[derive(Debug, Serialize)]
pub struct ExecutionScheduleResponseV3 {
    pub schedule: magician::magician_v2::artifact_v2::models::ExecutionScheduleReadinessRecord,
}

#[derive(Debug, Serialize)]
pub struct ExecutionTreeResponseV3 {
    pub tree: magician::magician_v2::artifact_v2::models::ExecutionTreeRecord,
}

#[derive(Debug, Serialize)]
pub struct TaskProgressResponseV3 {
    pub progress: magician::magician_v2::artifact_v2::progress::V3ProgressProjectionRecord,
}

/// Wire envelope for `GET /tasks/{id}/plan` and the family of
/// `*_task_plan_*` endpoints.
///
/// Despite the name, this is **PlanGraph storage per task**, not a
/// separate "task plan" concept. The legacy `TaskPlan*` naming
/// (struct, status enum, route segment) is a fossil from the
/// pre-2026-04-29 era when a runtime-mutable `taskplan_live.md`
/// existed. That subsystem was retired; what survived is just the
/// per-task envelope for a `PlanGraph` built by Plan mode
/// (`process_with_strategy`).
///
/// See the doc comment block on `TaskPlanStatus` in
/// `artifact_v2/models.rs` for the full story.
#[derive(Debug, Serialize)]
pub struct TaskPlanResponseV3 {
    pub plan: TaskPlanRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hitl_request: Option<HitlOpenTarget>,
}

impl TaskPlanResponseV3 {
    fn new(plan: TaskPlanRecord, scope: &ScopeRef) -> Self {
        let hitl_request = (plan.status == TaskPlanStatus::Draft).then(|| HitlOpenTarget {
            id: plan.plan_id.clone(),
            source: "plan_approval".to_string(),
            input_type: "confirmation".to_string(),
            prompt: "This task plan is ready for review.".to_string(),
            hint: Some(
                "Approve to execute, or reject to send the plan back for revision.".to_string(),
            ),
            input_schema: serde_json::json!({
                "type": "confirmation",
                "confirm_label": "Approve",
                "deny_label": "Reject",
                "plan_id": plan.plan_id,
                "task_id": plan.task_id,
            }),
            identifiers: HitlOpenIdentifiers {
                correlation_id: Some(plan.plan_id.clone()),
                ..Default::default()
            },
            scope: HitlOpenScope {
                principal: Some(scope.principal().to_string()),
                workspace: Some(scope.workspace().to_string()),
                workflow_id: Some(plan.task_id.clone()),
                task_id: Some(plan.task_id.clone()),
                execution_id: plan
                    .planning_execution_id
                    .clone()
                    .or_else(|| Some(plan.task_id.clone())),
                agent_id: Some(plan.agent_id.clone()),
                thread_id: None,
            },
            at: None,
        });
        Self { plan, hitl_request }
    }
}

#[cfg(test)]
mod task_plan_hitl_response_tests {
    use super::*;

    fn scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(
            &"principal-a".to_string(),
            &"workspace-a".to_string(),
        )
    }

    fn plan(status: TaskPlanStatus) -> TaskPlanRecord {
        TaskPlanRecord {
            plan_id: "plan-7".to_string(),
            task_id: "task-4".to_string(),
            agent_id: "personal-assistant".to_string(),
            status,
            planning_execution_id: None,
            planning_recovery: None,
            plan_graph: None,
            query_analysis: None,
            slot_graph_snapshot: None,
            strategy_attempts: Vec::new(),
            clarification_history: Vec::new(),
            pending_questions: Vec::new(),
            error: None,
            created_at: "2026-07-11T00:00:00Z".to_string(),
            updated_at: "2026-07-11T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn draft_plan_response_carries_real_direct_open_correlation() {
        let response = TaskPlanResponseV3::new(plan(TaskPlanStatus::Draft), &scope());
        let target = response
            .hitl_request
            .expect("draft plans require an approval target");

        assert_eq!(target.id, "plan-7");
        assert_eq!(target.source, "plan_approval");
        assert_eq!(target.input_type, "confirmation");
        assert_eq!(target.identifiers.correlation_id.as_deref(), Some("plan-7"));
        assert_eq!(target.scope.principal.as_deref(), Some("principal-a"));
        assert_eq!(target.scope.workspace.as_deref(), Some("workspace-a"));
        assert_eq!(target.scope.workflow_id.as_deref(), Some("task-4"));
        assert_eq!(target.scope.task_id.as_deref(), Some("task-4"));
        assert_eq!(target.scope.execution_id.as_deref(), Some("task-4"));
    }

    #[test]
    fn draft_plan_response_keeps_workflow_and_durable_execution_distinct() {
        let mut draft = plan(TaskPlanStatus::Draft);
        draft.planning_execution_id = Some("planexec-4".to_string());

        let target = TaskPlanResponseV3::new(draft, &scope())
            .hitl_request
            .expect("draft plans require an approval target");

        assert_eq!(target.scope.workflow_id.as_deref(), Some("task-4"));
        assert_eq!(target.scope.task_id.as_deref(), Some("task-4"));
        assert_eq!(target.scope.execution_id.as_deref(), Some("planexec-4"));
    }

    #[test]
    fn terminal_plan_response_does_not_reopen_approval() {
        let response = TaskPlanResponseV3::new(plan(TaskPlanStatus::Approved), &scope());
        assert!(response.hitl_request.is_none());
    }

    #[test]
    fn plan_decisions_require_a_nonempty_revision_id() {
        assert_eq!(required_plan_id(Some(" plan-7 ")), Some("plan-7"));
        assert_eq!(required_plan_id(None), None);
        assert_eq!(required_plan_id(Some("")), None);
        assert_eq!(required_plan_id(Some("   ")), None);
    }

    #[test]
    fn typed_task_plan_not_found_maps_to_http_404() {
        let response = map_result(Err(ArtifactV2Error::TaskPlanNotFound("task-4".to_string())))
            .expect("mapping succeeds");
        assert_eq!(response.status(), actix_web::http::StatusCode::NOT_FOUND);
    }
}

#[derive(Debug, Serialize)]
pub struct TaskPlanVersionSummaryV3 {
    pub epoch_ms: i64,
    pub label: Option<String>,
    pub step_count: usize,
    pub confidence: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanVersionsResponseV3 {
    pub versions: Vec<TaskPlanVersionSummaryV3>,
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanVersionResponseV3 {
    pub version: magician::magician_v2::artifact_v2::models::TaskPlanVersionRecord,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanAnalysisResponseV3 {
    pub query_analysis: Option<magician::magician_v2::query_analysis::UnifiedQueryAnalysis>,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanSlotsResponseV3 {
    pub slot_graph_snapshot:
        Option<magician::magician_v2::orchestrator::v2_orchestrator::SlotGraphSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanAttemptsResponseV3 {
    pub strategy_attempts: Vec<magician::magician_v2::storage::StrategyAttempt>,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanClarificationsResponseV3 {
    pub clarification_history:
        Vec<magician::magician_v2::storage::models::ClarificationHistoryEntry>,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanPendingQuestionsResponseV3 {
    pub pending_questions:
        Vec<magician::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion>,
}

#[derive(Debug, Serialize)]
pub struct TaskPlanResumeResponseV3 {
    pub plan: magician::magician_v2::artifact_v2::models::TaskPlanRecord,
    pub workflow_resumed: bool,
}

#[derive(Debug, Serialize)]
pub struct PendingTaskPlanClarificationV3 {
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub task_title: String,
    pub plan_id: String,
    pub plan_status: magician::magician_v2::artifact_v2::models::TaskPlanStatus,
    pub question: magician::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion,
}

#[derive(Debug, Serialize)]
pub struct PendingTaskPlanClarificationsResponseV3 {
    pub pending: Vec<PendingTaskPlanClarificationV3>,
}

#[derive(Debug, Serialize)]
pub struct TaskAnalysisResponseV3 {
    pub task_id: String,
    pub query: String,
    pub analysis: magician::magician_v2::query_analysis::UnifiedQueryAnalysis,
    pub metadata: magician::magician_v2::orchestrator::v2_orchestrator::AnalysisMetadata,
}

#[derive(Debug, Serialize)]
pub struct PublishedSurfaceListResponseV3 {
    pub surfaces: Vec<magician::magician_v2::artifact_v2::models::PublishedSurfaceRecord>,
}

#[derive(Debug, Serialize)]
pub struct PublishedSurfaceResponseV3 {
    pub surface: magician::magician_v2::artifact_v2::models::PublishedSurfaceRecord,
}

#[derive(Debug, Serialize)]
pub struct PublishedSurfaceRenderResponseV3 {
    pub render: magician::magician_v2::artifact_v2::models::PublishedSurfaceRenderRecord,
}

#[derive(Debug, Serialize)]
pub struct PublishedSurfaceProjectionListResponseV3 {
    pub surfaces: Vec<magician::magician_v2::artifact_v2::models::PublishedSurfaceProjectionRecord>,
}

#[derive(Debug, Serialize)]
pub struct PublishedSurfaceTopFeedResponseV3 {
    pub top_feed: magician::magician_v2::artifact_v2::models::PublishedSurfaceTopFeedRecord,
}

pub struct TaskApiV3 {
    service: Arc<ArtifactV2Service>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskAgentAssignmentViolation {
    Missing,
    Disabled,
    SurfaceIneligible,
}

fn task_agent_assignment_violation(
    definition: Option<(bool, &AgentInvocationPolicy)>,
) -> Option<TaskAgentAssignmentViolation> {
    let Some((disabled, invocation_policy)) = definition else {
        return Some(TaskAgentAssignmentViolation::Missing);
    };
    if disabled {
        return Some(TaskAgentAssignmentViolation::Disabled);
    }
    if !invocation_policy.permits_direct_surface(InvocationSurface::Task) {
        return Some(TaskAgentAssignmentViolation::SurfaceIneligible);
    }
    None
}

pub(crate) async fn validate_task_agent_assignment(
    resources: &AgentResources,
    scope: &ScopeRef,
    agent_id: &str,
) -> std::result::Result<(), HttpResponse> {
    let scoped_definition_store = resources
        .agent_definition_store
        .for_scope(&scope.principal(), &scope.workspace());
    let definitions = match scoped_definition_store.list_definitions().await {
        Ok(records) => records,
        Err(error) => {
            return Err(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "agent_definition_unavailable",
                "message": "The requested task agent could not be validated",
                "agent_id": agent_id,
                "detail": error.to_string(),
            })));
        },
    };
    let disabled_agent_ids =
        disabled_agent_hierarchy(definitions.iter().map(|record| &record.definition));
    let agent_definition = definitions
        .into_iter()
        .find(|record| record.definition.agent_id == agent_id)
        .map(|record| record.definition);
    let violation = task_agent_assignment_violation(agent_definition.as_ref().map(|definition| {
        (
            disabled_agent_ids.contains(agent_id),
            &definition.invocation_policy,
        )
    }));
    match violation {
        None => Ok(()),
        Some(TaskAgentAssignmentViolation::Missing) => Err(HttpResponse::UnprocessableEntity()
            .json(serde_json::json!({
                "error": "agent_definition_missing",
                "message": "The requested task agent is not available in this scope",
                "agent_id": agent_id,
            }))),
        Some(TaskAgentAssignmentViolation::Disabled) => Err(HttpResponse::UnprocessableEntity()
            .json(serde_json::json!({
                "error": "agent_definition_disabled",
                "message": "The requested task agent is disabled in this scope",
                "agent_id": agent_id,
            }))),
        Some(TaskAgentAssignmentViolation::SurfaceIneligible) => {
            Err(HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "agent_not_surface_eligible",
                "message": "This agent cannot be assigned from the generic task surface",
                "agent_id": agent_id,
                "surface": InvocationSurface::Task.as_str(),
            })))
        },
    }
}

#[cfg(test)]
mod task_agent_assignment_tests {
    use super::*;

    use magician::magician_v2::agents::{
        AgentDelegationPolicy, AgentDiscoverability, AgentInvocationPolicy,
    };

    #[test]
    fn assignment_requires_an_existing_enabled_task_surface_agent() {
        let ambient = AgentInvocationPolicy::default();
        assert_eq!(
            task_agent_assignment_violation(None),
            Some(TaskAgentAssignmentViolation::Missing)
        );
        assert_eq!(
            task_agent_assignment_violation(Some((true, &ambient))),
            Some(TaskAgentAssignmentViolation::Disabled)
        );

        let thinking_map_only = AgentInvocationPolicy {
            discoverability: AgentDiscoverability::SurfaceOnly,
            delegation: AgentDelegationPolicy::None,
            allowed_direct_surfaces: vec![InvocationSurface::ThinkingMap],
        };
        assert_eq!(
            task_agent_assignment_violation(Some((false, &thinking_map_only))),
            Some(TaskAgentAssignmentViolation::SurfaceIneligible)
        );
        assert_eq!(
            task_agent_assignment_violation(Some((false, &ambient))),
            None
        );
    }

    #[test]
    fn surface_only_agent_is_assignable_only_when_task_is_explicitly_allowed() {
        let task_only = AgentInvocationPolicy {
            discoverability: AgentDiscoverability::SurfaceOnly,
            delegation: AgentDelegationPolicy::None,
            allowed_direct_surfaces: vec![InvocationSurface::Task],
        };
        assert_eq!(
            task_agent_assignment_violation(Some((false, &task_only))),
            None
        );
    }
}

#[cfg(test)]
mod task_list_pagination_tests {
    use super::*;

    use magician::magician_v2::artifact_v2::models::{TaskLifecycle, TaskSyncMode};

    fn item(id: &str, updated_at: &str) -> TaskListItemV3 {
        TaskListItemV3 {
            id: id.to_string(),
            title: format!("title {id}"),
            description: String::new(),
            status: "pending".to_string(),
            agent_id: "personal-assistant".to_string(),
            ui_thread_id: "general".to_string(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "user".to_string(),
            depends_on: Vec::new(),
            approved: true,
            is_blocked: false,
            schedule: None,
            output_mode: TaskOutputMode::Accumulate,
            active_root_execution_id: None,
            latest_root_execution_id: None,
            last_completed_root_execution_id: None,
            current_step_title: None,
            current_substep_title: None,
            completion_summary: None,
            completion_outcome: None,
            completion_artifact_names: Vec::new(),
            has_plan: false,
            latest_plan_id: None,
            approved_plan_id: None,
            plan_updated_at: None,
            plan_status: None,
            pending_question: None,
            pending_questions: Vec::new(),
            chat_session_id: None,
            lifecycle: TaskLifecycle::default(),
            sync_mode: TaskSyncMode::default(),
            synthesis_pending: false,
            synthesis_failed_execution_id: None,
            monitor_revision: 0,
            awaiting_diff_approval: false,
            last_progress_at: None,
            created_at: "2026-07-30T09:00:00+00:00".to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    /// The corpus in the keyset order both paths must produce:
    /// `updated_at` descending, then id descending inside a tie.
    fn corpus() -> Vec<TaskListItemV3> {
        vec![
            item("task-e", "2026-07-30T11:00:00+00:00"),
            item("task-c", "2026-07-30T10:00:00+00:00"),
            item("task-b", "2026-07-30T10:00:00+00:00"),
            item("task-a", "2026-07-30T10:00:00+00:00"),
            item("task-d", "2026-07-30T09:00:00+00:00"),
        ]
    }

    fn millis(timestamp: &str) -> i64 {
        epoch_millis_from_rfc3339(timestamp).expect("fixture timestamp")
    }

    #[test]
    fn the_effective_sort_is_the_one_the_handler_actually_applies() {
        assert_eq!(effective_task_sort(None), "updated_at");
        assert_eq!(effective_task_sort(Some("updated_at")), "updated_at");
        assert_eq!(effective_task_sort(Some("title")), "title");
        assert_eq!(effective_task_sort(Some("status")), "status");
        assert_eq!(effective_task_sort(Some("created_at")), "created_at");
        assert_eq!(effective_task_sort(Some("agent_id")), "agent_id");
        // The handlers' `match` falls through for anything else, so a
        // mistyped sort is served in `updated_at` order. The index path has
        // to agree, or the same typo would be ordered two different ways.
        for junk in ["", " ", "Title", "updated", "due_date"] {
            assert_eq!(
                effective_task_sort(Some(junk)),
                "updated_at",
                "{junk} must fall through exactly as the handler's match does"
            );
        }
    }

    #[test]
    fn only_updated_at_descending_is_the_keyset_order() {
        assert!(task_order_is_keyset(None, None));
        assert!(task_order_is_keyset(Some("updated_at"), Some("desc")));
        // Anything that is not `asc` is descending, in both handlers.
        assert!(task_order_is_keyset(None, Some("DESC")));
        assert!(task_order_is_keyset(Some("nonsense"), None));

        assert!(!task_order_is_keyset(None, Some("asc")));
        assert!(!task_order_is_keyset(Some("title"), None));
        assert!(!task_order_is_keyset(Some("created_at"), Some("desc")));
        assert!(!task_order_is_keyset(Some("agent_id"), None));
    }

    #[test]
    fn a_cursor_resumes_at_the_row_after_it_including_inside_a_tie() {
        let tasks = corpus();
        let after = |id: &str, timestamp: &str| {
            keyset_start_offset(&tasks, &ListCursor::new(millis(timestamp), id))
        };

        assert_eq!(after("task-e", "2026-07-30T11:00:00+00:00"), 1);
        // Inside the three same-millisecond rows the tiebreak, not the
        // timestamp, is what carries the walk forward one row at a time.
        assert_eq!(after("task-c", "2026-07-30T10:00:00+00:00"), 2);
        assert_eq!(after("task-b", "2026-07-30T10:00:00+00:00"), 3);
        assert_eq!(after("task-a", "2026-07-30T10:00:00+00:00"), 4);
        assert_eq!(after("task-d", "2026-07-30T09:00:00+00:00"), 5);
    }

    #[test]
    fn a_cursor_whose_row_was_deleted_lands_on_the_next_survivor() {
        let tasks = corpus();

        // `task-bb` never existed; it sorts between task-c and task-b.
        assert_eq!(
            keyset_start_offset(
                &tasks,
                &ListCursor::new(millis("2026-07-30T10:00:00+00:00"), "task-bb")
            ),
            2,
            "a vanished cursor must resume at the next surviving row, not restart"
        );
        // Past the end of the corpus: an empty page, never an error.
        assert_eq!(
            keyset_start_offset(
                &tasks,
                &ListCursor::new(millis("2026-07-30T08:00:00+00:00"), "task-z")
            ),
            tasks.len()
        );
        // Before the newest row: the whole corpus follows.
        assert_eq!(
            keyset_start_offset(
                &tasks,
                &ListCursor::new(millis("2026-07-30T12:00:00+00:00"), "task-z")
            ),
            0
        );
    }

    #[test]
    fn the_next_cursor_is_the_last_row_of_the_page_and_nothing_else() {
        let tasks = corpus();
        let page = &tasks[..2];

        assert_eq!(
            next_keyset_cursor(page, true, true).as_deref(),
            Some("1785405600000:task-c"),
            "the cursor is the index's wire form, built from the page's LAST row"
        );
        assert_eq!(
            ListCursor::decode("1785405600000:task-c").expect("decoding"),
            ListCursor::new(millis("2026-07-30T10:00:00+00:00"), "task-c"),
            "and it decodes back to the row it was minted from"
        );
        assert_eq!(
            next_keyset_cursor(page, false, true),
            None,
            "the last page carries no cursor"
        );
        assert_eq!(
            next_keyset_cursor(page, true, false),
            None,
            "a title-sorted page must not mint a timestamp cursor: sending it \
             back would seek by updated_at and return a different slice"
        );
        assert_eq!(next_keyset_cursor(&[], true, true), None);
    }

    #[test]
    fn the_paginated_envelope_carries_the_same_five_keys_on_every_path() {
        let body = task_list_page_body(
            serde_json::json!([{ "id": "task-a" }]),
            42,
            10,
            20,
            true,
            Some("1785405600000:task-c".to_string()),
            None,
        );

        assert_eq!(body["tasks"][0]["id"], "task-a");
        assert_eq!(body["pagination"]["total"], 42);
        assert_eq!(body["pagination"]["limit"], 10);
        assert_eq!(body["pagination"]["offset"], 20);
        assert_eq!(body["pagination"]["has_more"], true);
        assert_eq!(body["pagination"]["next_cursor"], "1785405600000:task-c");
        assert!(
            body.get("counts").is_none(),
            "counts are reported only when the reader supplied their date"
        );

        let last = task_list_page_body(serde_json::json!([]), 0, 10, 0, false, None, None);
        assert!(
            last["pagination"]["next_cursor"].is_null(),
            "next_cursor is present and null on the last page, never absent — a \
             client must not have to tell `no more pages` from `no cursors here`"
        );

        let counted = task_list_page_body(
            serde_json::json!([]),
            0,
            10,
            0,
            false,
            None,
            Some(lane_counts(&[], "2026-07-30")),
        );
        assert_eq!(counted["counts"]["all"], 0);
        assert_eq!(
            counted["counts"].as_object().expect("counts object").len(),
            6,
            "every lane is reported even at zero"
        );
    }
}

impl TaskApiV3 {
    pub fn from_service(service: Arc<ArtifactV2Service>) -> Self {
        Self { service }
    }

    pub fn service(&self) -> Arc<ArtifactV2Service> {
        Arc::clone(&self.service)
    }

    pub async fn create_task(
        &self,
        http_req: &HttpRequest,
        req: web::Json<CreateTaskV3Request>,
        resources: &Arc<AgentResources>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let description = req.description.trim();
        if description.is_empty() {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "task_description_required",
                "message": "description is required and must not be empty"
            })));
        }

        let title = req.title.clone().unwrap_or_else(|| {
            description
                .lines()
                .next()
                .unwrap_or("Untitled task")
                .chars()
                .take(120)
                .collect()
        });
        let linked_task_ids = if let Some(reference_task_ids) = req.reference_task_ids.clone() {
            let mut normalized_reference_task_ids = Vec::new();
            for reference_task_id in reference_task_ids {
                let reference_task_id = reference_task_id.trim();
                if !is_plain_task_reference_id(reference_task_id) {
                    return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "invalid_reference_task_id",
                        "message": "reference_task_ids must contain plain task ids such as `task_...`"
                    })));
                }
                match self.service.get_task(&scope, reference_task_id).await {
                    Ok(task) if task.state.status == "completed" => {},
                    Ok(task) => {
                        return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                            "error": "reference_task_not_completed",
                            "message": format!(
                                "reference_task_ids must refer to completed tasks; {reference_task_id} is currently {}",
                                task.state.status
                            )
                        })));
                    },
                    Err(error) => {
                        return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                            "error": "reference_task_not_found",
                            "message": format!(
                                "reference_task_ids contains a task that is not available in this scope: {reference_task_id}: {error}"
                            )
                        })));
                    },
                }
                if !normalized_reference_task_ids
                    .iter()
                    .any(|existing| existing == reference_task_id)
                {
                    normalized_reference_task_ids.push(reference_task_id.to_string());
                }
            }
            normalized_reference_task_ids
        } else {
            req.depends_on.clone().unwrap_or_default()
        };
        // The resolved ui_thread_id feeds both the owner rule below and the task input.
        let resolved_ui_thread_id =
            resolve_ui_thread_id(http_req.headers(), req.ui_thread_id.clone());
        let agent_id = resolve_created_task_owner_agent_id(
            resources,
            &resolved_ui_thread_id,
            req.tags.as_deref().unwrap_or(&[]),
            req.agent_id.as_deref(),
        );
        if let Err(response) = validate_task_agent_assignment(resources, &scope, &agent_id).await {
            return Ok(response);
        }
        // Intent-gated visibility: a VibeDev cockpit run (Build OR Discuss) is a
        // cockpit-coupled execution side-effect (same shape as chat-pack / "Do It"),
        // so default it to `Internal` (off the user `/tasks` feed) and promote to the
        // user-visible `Persistent` lifecycle ONLY when the user explicitly asks to
        // save the run as a task (`save_as_task`). Note this uses the BROADER
        // `is_vibedev_cockpit_run`, not the build-only `is_vibedev_coding_build_run`
        // the owner rule above gates on, so a Discuss/plan run is
        // internal-by-default too. Non-VibeDev creates keep the
        // `Persistent` default. The cockpit's own run history reads the internal feed
        // (`/v3/tasks/internal?ui_thread_id=vibedev`), so internal runs still show there.
        //
        // EXCEPTION — a SCHEDULED run (cron) must be `Persistent`: the cron scheduler
        // enumerates only the `tasks/` root, never `internal_tasks/`, so an Internal
        // scheduled task would SILENTLY never fire. So a nightly/Autopilot VibeDev
        // build is auto-promoted to user-visible regardless of `save_as_task` — a
        // scheduled deliverable is exactly the kind of run the user tracks in `/tasks`.
        let is_vibedev_cockpit_run =
            magician::magician_v2::artifact_v2::models::is_vibedev_cockpit_run(
                &resolved_ui_thread_id,
                req.tags.as_deref().unwrap_or(&[]),
            );
        let lifecycle = if is_vibedev_cockpit_run
            && !req.save_as_task.unwrap_or(false)
            && req.schedule.is_none()
        {
            magician::magician_v2::artifact_v2::models::TaskLifecycle::Internal
        } else {
            magician::magician_v2::artifact_v2::models::TaskLifecycle::default()
        };
        let task = self
            .service
            .create_task(CreateTaskInput {
                principal: scope.principal().to_string(),
                workspace: scope.workspace().to_string(),
                title,
                description: description.to_string(),
                agent_id,
                goal_id: None,
                ui_thread_id: resolved_ui_thread_id,
                priority: req.priority.clone(),
                due_date: req.due_date.clone(),
                tags: req.tags.clone().unwrap_or_default(),
                created_by: req.created_by.clone().unwrap_or_else(|| "user".to_string()),
                depends_on: linked_task_ids,
                approved: req.approved.unwrap_or(true),
                schedule: req.schedule.clone(),
                output_mode: req.output_mode.clone().unwrap_or_default(),
                chat_session_id: None,
                lifecycle,
                sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await;
        map_result(
            task.map(|task| HttpResponse::Created().json(TaskResponseV3::new(&self.service, task))),
        )
    }

    pub async fn get_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task = self.service.get_task(&scope, &path.into_inner()).await;
        map_result(
            task.map(|task| HttpResponse::Ok().json(TaskResponseV3::new(&self.service, task))),
        )
    }

    pub async fn read_task_result(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
        body: web::Json<ReadTaskResultRequest>,
        resources: &Arc<AgentResources>,
    ) -> Result<HttpResponse> {
        use magician::magician_v2::tool_result_materialization::{
            CanonicalRawResultStore, RawResultOwner, RawResultReadContext, RawResultReadRequest,
            ScopedResultReadAuthority, ScopedResultRef, DEFAULT_RESULT_PAGE_BYTES,
        };

        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task_id = path.into_inner();
        let task = match self.service.get_task(&scope, &task_id).await {
            Ok(task) => task,
            Err(ArtifactV2Error::TaskNotFound(_)) => {
                return Ok(HttpResponse::NotFound().json(serde_json::json!({
                    "error": "result_not_found"
                })));
            },
            Err(_) => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "result_temporarily_unavailable"
                })));
            },
        };
        let agent_id = task.manifest.agent_id;
        let scoped_store = resources
            .agent_definition_store
            .for_scope(&scope.principal(), &scope.workspace());
        let definition = match scoped_store.get_enabled_definition(&agent_id).await {
            Ok(Some(record)) => record.definition,
            Ok(None) => {
                return Ok(HttpResponse::Forbidden().json(serde_json::json!({
                    "error": "result_revoked"
                })));
            },
            Err(_) => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "result_temporarily_unavailable"
                })));
            },
        };
        let current_definitions = match scoped_store.list_definitions().await {
            Ok(records) => records,
            Err(_) => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "result_temporarily_unavailable"
                })));
            },
        };
        let disabled_agent_ids =
            disabled_agent_hierarchy(current_definitions.iter().map(|record| &record.definition));
        if disabled_agent_ids.contains(&agent_id)
            || !definition
                .invocation_policy
                .permits_direct_surface(InvocationSurface::Task)
        {
            return Ok(HttpResponse::Forbidden().json(serde_json::json!({
                "error": "result_revoked"
            })));
        }
        let current_delegation_targets =
            magician::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
                &definition,
                current_definitions.iter().map(|record| &record.definition),
                &disabled_agent_ids,
                InvocationSurface::Delegation,
            );
        let current_handover_targets =
            magician::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
                &definition,
                current_definitions.iter().map(|record| &record.definition),
                &disabled_agent_ids,
                InvocationSurface::Handover,
            );
        let Some(tool_index) = resources.tool_index.get() else {
            return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "result_temporarily_unavailable"
            })));
        };
        let all_tool_names = tool_index
            .pack_names()
            .into_iter()
            .flat_map(|pack| tool_index.leaf_names_for_pack(&pack))
            .collect::<Vec<_>>();
        let current_direct_grants =
            magician::magician_v2::chat::service::resolved_surface_tools_from_definition(
                &definition,
                Some(&all_tool_names),
            );
        let mut authorized_tool_names =
            magician::magician_v2::execution::flat_loop::expand_direct_grants_to_leaves(
                tool_index,
                &current_direct_grants,
            );
        // Rebuild platform-owned autonomous controls through the same catalog
        // builder used by task decisions. These controls do not live in the
        // pack index, so definition-only expansion would incorrectly revoke a
        // valid result from e.g. `read_result` or a delegation control. The
        // builder also reapplies the current deny list instead of hardcoding a
        // second structural-tool allowlist in this API.
        let current_catalog = magician::magician_v2::execution::flat_loop::build_flat_loop_tools(
            &magician::magician_v2::execution::agentic::native_catalog::CatalogBuildContext {
                allowed_action_types: None,
                credentials_enabled: false,
                has_delegation_targets: !current_delegation_targets.is_empty()
                    || !current_handover_targets.is_empty(),
                direct_capabilities: current_direct_grants
                    .iter()
                    .map(|name| (name.clone(), String::new(), serde_json::json!({})))
                    .collect(),
                is_chat_mode: false,
                available_procedure_skills: Vec::new(),
                denied_tool_names: definition
                    .excluded_tools
                    .iter()
                    .chain(definition.denied_tools.iter())
                    .cloned()
                    .collect(),
                delegate_only_grant_gate: None,
            },
            tool_index,
            &[],
        );
        authorized_tool_names.extend(current_catalog.hot.into_iter().map(|tool| tool.name));
        authorized_tool_names.extend(current_catalog.deferred.into_iter().map(|tool| tool.name));
        let trust_path = scoped_store.storage().trust_policies_path();
        let enforcer = match TrustPolicyEnforcer::from_yaml_file(&trust_path) {
            Ok(enforcer) if enforcer.has_level(&definition.trust_level.0) => enforcer,
            _ => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "result_temporarily_unavailable"
                })));
            },
        };
        let definition_digest = match magician::magician_v2::execution::agentic::policy_snapshot::canonical_definition_digest(&definition) {
            Ok(digest) => digest,
            Err(_) => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "result_temporarily_unavailable"
                })));
            },
        };
        let authority = match ScopedResultReadAuthority::for_current_policy(
            scope.clone(),
            RawResultOwner::Task {
                task_id: task_id.clone(),
                execution_id: body.execution_id.clone(),
            },
            agent_id.clone(),
            authorized_tool_names,
            definition_digest,
        ) {
            Ok(authority) => authority.with_policy_guard(Arc::new(TaskResultReadPolicyGuard {
                level: definition.trust_level,
                enforcer,
            })),
            Err(_) => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "result_temporarily_unavailable"
                })));
            },
        };
        let content_ref = match ScopedResultRef::parse(body.result_ref.trim()) {
            Ok(content_ref) => content_ref,
            Err(_) => {
                return Ok(HttpResponse::NotFound().json(serde_json::json!({
                    "error": "result_not_found"
                })));
            },
        };
        let owner = RawResultOwner::Task {
            task_id: task_id.clone(),
            execution_id: body.execution_id.clone(),
        };
        let telemetry_scope = scope.clone();
        let telemetry_agent_id = agent_id.clone();
        let telemetry_execution_id = body.execution_id.clone();
        let requested_field_count = body.field_paths.len();
        let request = RawResultReadRequest {
            content_ref,
            cursor: body.cursor.clone(),
            field_paths: body.field_paths.clone(),
            max_records: body.max_records,
            max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
        };
        let started = Instant::now();
        let outcome =
            CanonicalRawResultStore::new(resources.artifact_workspace.clone(), Arc::new(authority))
                .read(
                    &RawResultReadContext {
                        scope,
                        owner,
                        agent_id,
                    },
                    &request,
                )
                .await;
        if let Some(broadcaster) = resources.event_broadcaster.as_ref() {
            let mut telemetry =
                magician::magician_v2::tool_result_materialization::content_free_read_telemetry(
                    &outcome,
                    requested_field_count,
                    started.elapsed().as_secs_f64() * 1_000.0,
                );
            if let Some(fields) = telemetry.as_object_mut() {
                fields.insert("surface".to_string(), serde_json::json!("display_api"));
                fields.insert("owner_kind".to_string(), serde_json::json!("task"));
                fields.insert("task_id".to_string(), serde_json::json!(&task_id));
                fields.insert(
                    "execution_id".to_string(),
                    serde_json::json!(&telemetry_execution_id),
                );
            }
            broadcaster.emit_named(
                "tool.result.read",
                &telemetry_agent_id,
                Some(&telemetry_scope.principal()),
                Some(&telemetry_scope.workspace()),
                telemetry,
            );
        }
        match outcome {
            Ok(page) => Ok(HttpResponse::Ok().json(
                magician::magician_v2::tool_result_materialization::lossless_read_success_payload(
                    page,
                ),
            )),
            Err(error) => {
                let (status, code) = task_result_error_status(&error);
                Ok(HttpResponse::build(status).json(serde_json::json!({ "error": code })))
            },
        }
    }

    pub async fn update_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        req: web::Json<UpdateTaskV3Request>,
        resources: &Arc<AgentResources>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let agent_id = req
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|agent_id| !agent_id.is_empty())
            .map(ToOwned::to_owned);
        if let Some(agent_id) = agent_id.as_deref() {
            if let Err(response) = validate_task_agent_assignment(resources, &scope, agent_id).await
            {
                return Ok(response);
            }
        }
        let response = self
            .service
            .update_task(
                &scope,
                &path.into_inner(),
                UpdateTaskInput {
                    title: req.title.clone(),
                    description: req.description.clone(),
                    agent_id,
                    ui_thread_id: req.ui_thread_id.clone(),
                    priority: req.priority.clone(),
                    due_date: req.due_date.clone(),
                    tags: req.tags.clone(),
                    created_by: req.created_by.clone(),
                    depends_on: req.depends_on.clone(),
                    approved: req.approved,
                    schedule: req.schedule.clone(),
                    output_mode: req.output_mode.clone(),
                    // Monitor specs are written only through the validated
                    // /monitors admission path (monitors_api), never the
                    // generic task update.
                    monitor_spec: None,
                },
            )
            .await;
        map_result(
            response.map(|task| HttpResponse::Ok().json(TaskResponseV3::new(&self.service, task))),
        )
    }

    pub async fn delete_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<DeleteTaskV3Query>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task_id = path.into_inner();
        let remove_files = query.remove_files;
        let response = self
            .service
            .archive_task_with_options(&scope, &task_id, remove_files)
            .await;
        if response.is_ok() && remove_files {
            let cleanup =
                magician::magician_v2::tool_result_materialization::CanonicalRawResultStore::for_lifecycle_cleanup(
                    self.service.workspace().clone(),
                )
                .cleanup_task(&scope, &task_id)
                .await;
            if let Err(error) = cleanup {
                tracing::warn!(
                    task_id = %task_id,
                    error = %error,
                    "canonical tool-result cleanup failed after task file removal"
                );
            }
        }
        map_result(response.map(|_| {
            HttpResponse::Ok().json(serde_json::json!({
                "ok": true,
                "task_id": task_id,
                "files_removed": remove_files,
            }))
        }))
    }

    pub async fn update_task_status(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        req: web::Json<UpdateTaskStatusV3Request>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self
            .service
            .update_task_status(&scope, &path.into_inner(), &req.status)
            .await;
        map_result(
            response.map(|task| HttpResponse::Ok().json(TaskResponseV3::new(&self.service, task))),
        )
    }

    pub async fn approve_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self.service.approve_task(&scope, &path.into_inner()).await;
        map_result(
            response.map(|task| HttpResponse::Ok().json(TaskResponseV3::new(&self.service, task))),
        )
    }

    pub async fn list_tasks(
        &self,
        http_req: &HttpRequest,
        query: web::Query<TasksListQueryV3>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        // Legacy shape when no pagination is requested — byte-compatible with
        // every pre-pagination consumer. Answered from the walk and BEFORE
        // any of the paginated path's work, so this branch can never acquire
        // a failure mode, an extra field, or a different order from a change
        // made for paginated callers.
        if query.limit.is_none() && query.offset.is_none() && query.cursor.is_none() {
            let tasks = self.service.list_tasks(&scope).await;
            return map_result(
                tasks.map(|tasks| HttpResponse::Ok().json(TaskListResponseV3 { tasks })),
            );
        }

        // The reader's local date. The server has no idea what timezone the
        // reader is in, so the date lanes take it explicitly and refuse to
        // guess — a lane computed in the wrong day is a wrong answer that
        // looks entirely right.
        let reader_today = query
            .today
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let requested_view = query
            .view
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let lane = match requested_view {
            Some(value) => match TaskLane::parse(value) {
                Some(lane) => Some(lane),
                // Rejected rather than ignored: silently serving the whole
                // pool for a mistyped lane is the page-search bug again.
                None => {
                    return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "unknown_task_view",
                        "detail": format!(
                            "unknown view {value:?}; expected one of {:?}",
                            TaskLane::ALL.map(TaskLane::wire_name)
                        )
                    })));
                },
            },
            None => None,
        };
        if lane.is_some_and(TaskLane::needs_today) && reader_today.is_none() {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "task_view_requires_today",
                "detail": "view=today and view=overdue need the reader's local date as today=YYYY-MM-DD"
            })));
        }

        // Filter / sort / page — the same in-memory contract the internal
        // listing uses. Read here, before either path runs, because whether
        // the index CAN serve the request depends on them.
        let filter_status = query
            .status
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let filter_query = query
            .query
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_lowercase);
        let order_is_keyset = task_order_is_keyset(query.sort.as_deref(), query.order.as_deref());
        let limit = query.limit.unwrap_or(50).clamp(1, 500);
        let offset = query.offset.unwrap_or(0);

        let cursor = match query
            .cursor
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            None => None,
            Some(raw) => match ListCursor::decode(raw) {
                Ok(cursor) => Some(cursor),
                // Refused rather than ignored, for the same reason a mistyped
                // view is: a cursor silently dropped serves page one under
                // the banner of page nine.
                Err(error) => {
                    return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "invalid_task_cursor",
                        "detail": error.to_string(),
                    })));
                },
            },
        };
        if cursor.is_some() && !order_is_keyset {
            // A cursor is a position in `updated_at DESC`; under any other
            // order it names a row that is not where the page would resume.
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "cursor_requires_updated_at_order",
                "detail": "cursor= pages the updated_at/desc order; drop sort=/order= or drop cursor="
            })));
        }

        // The index answers the exact question asked, or it does not answer
        // it at all. `is_ready()` is false for the WHOLE of a rebuild — a
        // half-built index looks exactly like a complete one holding fewer
        // tasks — and the filters below are ones the index does not carry.
        // Everything it cannot express falls through to the walk, which is
        // still the source of truth.
        if filter_status.is_none() && filter_query.is_none() && order_is_keyset {
            if let Some(response) = self
                .indexed_task_page(
                    &scope,
                    ListKind::Task,
                    lane,
                    reader_today,
                    cursor.as_ref(),
                    limit,
                    offset,
                    reader_today.is_some(),
                )
                .await
            {
                return Ok(response);
            }
        }

        let mut tasks = match self.service.list_tasks(&scope).await {
            Ok(tasks) => tasks,
            Err(error) => return map_result(Err(error)),
        };

        // Counts run over the WHOLE scoped pool, BEFORE the lane filter and
        // before status/query. Counting afterwards would make every badge
        // report the lane the reader is already looking at.
        //
        // Reported only when the reader told us their date: two of the six
        // lanes are date lanes, and a badge computed against a guessed day
        // is exactly the confident-looking wrong answer this endpoint is
        // meant to stop serving.
        let lane_pool: Vec<LaneTask> = tasks.iter().map(lane_task_slice).collect();
        let counts = reader_today.map(|today| lane_counts(&lane_pool, today));

        if let Some(lane) = lane {
            // Non-date lanes ignore `today`; the date lanes were guaranteed a
            // real one by the guard above.
            let today = reader_today.unwrap_or("");
            tasks = tasks
                .into_iter()
                .zip(lane_pool.iter())
                .filter(|(_, slice)| lane.matches(*slice, today))
                .map(|(task, _)| task)
                .collect();
        }

        tasks.retain(|task| {
            if let Some(status) = filter_status {
                if task.status != status {
                    return false;
                }
            }
            if let Some(needle) = filter_query.as_deref() {
                let hay = format!(
                    "{} {} {}",
                    task.id.to_lowercase(),
                    task.title.to_lowercase(),
                    task.status.to_lowercase()
                );
                if !hay.contains(needle) {
                    return false;
                }
            }
            true
        });
        if order_is_keyset {
            // The walk must produce the index's order, TIEBREAK INCLUDED.
            // Sorting on `updated_at` alone left same-millisecond rows in
            // whatever order `read_dir` happened to yield, which is fine for
            // an offset and fatal for a cursor: the seek would land inside a
            // tie group in one order and outside it in the other, and the
            // page would quietly skip or repeat those rows.
            tasks.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| b.id.cmp(&a.id))
            });
        } else {
            match query.sort.as_deref().unwrap_or("updated_at") {
                "title" => tasks.sort_by(|a, b| a.title.cmp(&b.title)),
                "status" => tasks.sort_by(|a, b| a.status.cmp(&b.status)),
                "created_at" => tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
                _ => tasks.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            }
            if query.order.as_deref().unwrap_or("desc") != "asc" {
                tasks.reverse();
            }
        }

        let total = tasks.len();
        // A cursor supersedes the offset here exactly as it does in the
        // index, so a client that started paging while the index was
        // rebuilding keeps working when it finishes mid-walk.
        let start = match cursor.as_ref() {
            Some(cursor) => keyset_start_offset(&tasks, cursor),
            None => offset,
        };
        let page: Vec<_> = tasks.into_iter().skip(start).take(limit).collect();
        let has_more = start.saturating_add(page.len()) < total;
        let next_cursor = next_keyset_cursor(&page, has_more, order_is_keyset);
        Ok(HttpResponse::Ok().json(task_list_page_body(
            serde_json::json!(page),
            total,
            limit,
            start,
            has_more,
            next_cursor,
            counts,
        )))
    }

    /// Serve one task-list page out of the storage index, or decline.
    ///
    /// `None` means "the index cannot answer this" — it was never wired in,
    /// it is mid-rebuild, or a query failed — and the caller must fall back
    /// to the file walk. Declining is always safe; the files are the source
    /// of truth and the index is a cache that may be deleted at any moment.
    ///
    /// The readiness check is the important one. `is_ready()` is false from
    /// the moment a rebuild starts until it finishes, INCLUDING across a
    /// crash, because the marker is on disk. Serving a half-built index
    /// would not error or look empty — it would look like a complete index
    /// holding fewer tasks, which is the one failure here that a reader
    /// could not detect.
    #[allow(clippy::too_many_arguments)]
    async fn indexed_task_page(
        &self,
        scope: &ScopeRef,
        kind: ListKind,
        lane: Option<TaskLane>,
        reader_today: Option<&str>,
        cursor: Option<&ListCursor>,
        limit: usize,
        offset: usize,
        want_counts: bool,
    ) -> Option<HttpResponse> {
        let index = self.service.list_index()?;
        if !index.is_ready().unwrap_or(false) {
            return None;
        }
        let list_scope = ArtifactV2Service::list_scope(scope);
        let today = reader_today.unwrap_or("");

        let mut request = ListPageQuery::new(kind, list_scope.clone(), limit).offset(offset);
        if let Some(lane) = lane {
            request = request.lane(lane, today);
        }
        if let Some(cursor) = cursor {
            request = request.cursor(cursor.clone());
        }

        let page = match index.page(&request) {
            Ok(page) => page,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[LIST-INDEX] Page query failed; falling back to the file walk"
                );
                return None;
            },
        };
        let counts = if want_counts {
            match index.lane_counts(kind, &list_scope, today) {
                Ok(counts) => Some(counts),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "[LIST-INDEX] Lane counts failed; falling back to the file walk"
                    );
                    return None;
                },
            }
        } else {
            None
        };

        // The index holds ids and the narrow slice a lane reads; the wire
        // row is built from the record on disk, so a stale cache can never
        // put a field in front of a reader that the file does not carry.
        let ids: Vec<String> = page.items.iter().map(|entry| entry.id.clone()).collect();
        let items = match kind {
            ListKind::Internal => {
                self.service
                    .list_internal_task_page_items(scope, &ids)
                    .await
            },
            _ => self.service.list_task_page_items(scope, &ids).await,
        };
        let items = match items {
            Ok(items) => items,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[LIST-INDEX] Loading an indexed page's records failed; falling back to the file walk"
                );
                return None;
            },
        };

        Some(HttpResponse::Ok().json(task_list_page_body(
            serde_json::json!(items),
            page.total,
            page.limit,
            page.offset,
            page.has_more,
            page.next_cursor,
            counts,
        )))
    }

    /// `GET /api/magician/v3/tasks/internal` — backs the `/internal-tasks`
    /// UI surface. Supports server-side pagination, sort, and filter.
    /// Reads from the canonical `internal_tasks/` storage root.
    pub async fn list_internal_tasks(
        &self,
        http_req: &HttpRequest,
        query: web::Query<InternalTasksQueryV3>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let limit = query.limit.unwrap_or(50).clamp(1, 500);
        let offset = query.offset.unwrap_or(0);
        let sort_field = query.sort.as_deref().unwrap_or("updated_at");
        let sort_order = query.order.as_deref().unwrap_or("desc");
        let filter_agent = query
            .agent_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let filter_status = query
            .status
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let filter_query = query
            .query
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_lowercase);
        let filter_thread = query
            .ui_thread_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let order_is_keyset = task_order_is_keyset(query.sort.as_deref(), query.order.as_deref());

        let cursor = match query
            .cursor
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            None => None,
            Some(raw) => match ListCursor::decode(raw) {
                Ok(cursor) => Some(cursor),
                Err(error) => {
                    return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "invalid_task_cursor",
                        "detail": error.to_string(),
                    })));
                },
            },
        };
        if cursor.is_some() && !order_is_keyset {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "cursor_requires_updated_at_order",
                "detail": "cursor= pages the updated_at/desc order; drop sort=/order= or drop cursor="
            })));
        }

        // The index carries neither `agent_id` nor `ui_thread_id`, so a
        // request that filters on either is served by the walk. So is any
        // request made while the index is rebuilding — see `indexed_task_page`.
        if filter_agent.is_none()
            && filter_status.is_none()
            && filter_query.is_none()
            && filter_thread.is_none()
            && order_is_keyset
        {
            if let Some(response) = self
                .indexed_task_page(
                    &scope,
                    ListKind::Internal,
                    None,
                    None,
                    cursor.as_ref(),
                    limit,
                    offset,
                    false,
                )
                .await
            {
                return Ok(response);
            }
        }

        let tasks_result = self.service.list_internal_tasks(&scope).await;
        let mut tasks = match tasks_result {
            Ok(value) => value,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("Failed to list internal tasks: {error}")
                })));
            },
        };

        // Filter (applied after the read because list_internal_tasks
        // already pays the disk-walk cost; filtering in-memory keeps
        // the surface simple).
        tasks.retain(
            |task: &magician::magician_v2::artifact_v2::models::TaskListItemV3| {
                if let Some(agent) = filter_agent {
                    if task.agent_id != agent {
                        return false;
                    }
                }
                if let Some(status) = filter_status {
                    if task.status != status {
                        return false;
                    }
                }
                if let Some(thread) = filter_thread {
                    if task.ui_thread_id != thread {
                        return false;
                    }
                }
                if let Some(needle) = filter_query.as_deref() {
                    let hay = format!(
                        "{} {} {} {}",
                        task.id.to_lowercase(),
                        task.title.to_lowercase(),
                        task.agent_id.to_lowercase(),
                        task.status.to_lowercase()
                    );
                    if !hay.contains(needle) {
                        return false;
                    }
                }
                true
            },
        );

        // Sort. The keyset order carries the index's tiebreak so a cursor
        // means the same position whichever path served the previous page.
        if order_is_keyset {
            tasks.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| b.id.cmp(&a.id))
            });
        } else {
            match sort_field {
                "title" => tasks.sort_by(|a, b| a.title.cmp(&b.title)),
                "agent_id" => tasks.sort_by(|a, b| a.agent_id.cmp(&b.agent_id)),
                "status" => tasks.sort_by(|a, b| a.status.cmp(&b.status)),
                "created_at" => tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
                _ => tasks.sort_by(|a, b| a.updated_at.cmp(&b.updated_at)),
            }
            if sort_order != "asc" {
                tasks.reverse();
            }
        }

        let total = tasks.len();
        let start = match cursor.as_ref() {
            Some(cursor) => keyset_start_offset(&tasks, cursor),
            None => offset,
        };
        let page: Vec<_> = tasks.into_iter().skip(start).take(limit).collect();
        let has_more = start.saturating_add(page.len()) < total;
        let next_cursor = next_keyset_cursor(&page, has_more, order_is_keyset);
        Ok(HttpResponse::Ok().json(task_list_page_body(
            serde_json::json!(page),
            total,
            limit,
            start,
            has_more,
            next_cursor,
            None,
        )))
    }

    /// `GET /api/magician/v3/tasks/{task_id}/details` — task + a bounded
    /// execution page with each execution's outputs and persisted artifacts.
    /// Powers the `/internal-tasks` expandable rows so the UI doesn't
    /// have to round-trip per execution to render the nested table.
    pub async fn get_task_details(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<TaskExecutionPageQuery>,
    ) -> Result<HttpResponse> {
        let task_id = path.into_inner();
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        match self
            .service
            .get_task_with_executions_page(
                &scope,
                &task_id,
                query.limit.unwrap_or(25),
                query.cursor.as_deref(),
            )
            .await
        {
            Ok(value) => {
                let mut body = match serde_json::to_value(value) {
                    Ok(body) => body,
                    Err(error) => {
                        return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                            "error": format!("Failed to serialize task details: {error}")
                        })));
                    },
                };
                if let Err(error) = self
                    .service
                    .enrich_task_details_output_snippets(&scope, &task_id, &mut body)
                    .await
                {
                    return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": format!("Failed to enrich task output previews: {error}")
                    })));
                }
                Ok(HttpResponse::Ok().json(body))
            },
            Err(ArtifactV2Error::TaskNotFound(_)) => {
                Ok(HttpResponse::NotFound().json(serde_json::json!({
                    "error": "Task not found"
                })))
            },
            Err(ArtifactV2Error::InvalidRequest(error)) => {
                Ok(HttpResponse::BadRequest().json(serde_json::json!({ "error": error })))
            },
            Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to load task details: {error}")
            }))),
        }
    }

    /// `DELETE /api/magician/v3/tasks/internal/{task_id}` — removes
    /// the task dir (both `internal_tasks/<id>/` and any leftover
    /// `tasks/<id>/`). Used by the `/internal-tasks` row-delete action.
    pub async fn delete_internal_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let task_id = path.into_inner();
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        match self.service.delete_internal_task(&scope, &task_id).await {
            Ok(()) => Ok(HttpResponse::Ok().json(serde_json::json!({
                "deleted": true,
                "task_id": task_id,
            }))),
            Err(ArtifactV2Error::InvalidRequest(message))
                if message.starts_with(&format!("{APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED}:")) =>
            {
                map_result(Err(ArtifactV2Error::InvalidRequest(message)))
            },
            Err(error) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Failed to delete internal task: {error}")
            }))),
        }
    }

    pub async fn start_task_planning(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let plan = Arc::clone(&self.service)
            .start_task_planning(scope.clone(), path.into_inner())
            .await;
        map_result(
            plan.map(|plan| HttpResponse::Accepted().json(TaskPlanResponseV3::new(plan, &scope))),
        )
    }

    pub async fn get_task_plan(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let plan = self.service.get_task_plan(&scope, &path.into_inner()).await;
        map_result(plan.map(|plan| HttpResponse::Ok().json(TaskPlanResponseV3::new(plan, &scope))))
    }

    pub async fn update_task_plan(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
        req: web::Json<UpdateTaskPlanV3Request>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self
            .service
            .update_task_plan(
                &scope,
                &path.into_inner(),
                req.plan.clone(),
                req.label.clone(),
            )
            .await;
        map_result(
            response.map(|plan| HttpResponse::Ok().json(TaskPlanResponseV3::new(plan, &scope))),
        )
    }

    pub async fn list_task_plan_versions(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<TaskPlanVersionsQueryV3>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self
            .service
            .list_task_plan_versions(&scope, &path.into_inner(), query.before, query.limit)
            .await;
        map_result(response.map(|(versions, has_more)| {
            HttpResponse::Ok().json(TaskPlanVersionsResponseV3 {
                versions: versions
                    .into_iter()
                    .map(|version| TaskPlanVersionSummaryV3 {
                        epoch_ms: version.epoch_ms,
                        label: version.label,
                        step_count: version
                            .plan_graph
                            .as_ref()
                            .map(|plan| plan.steps.len())
                            .unwrap_or(0),
                        confidence: version.plan_graph.as_ref().map(|plan| plan.confidence),
                    })
                    .collect(),
                has_more,
            })
        }))
    }

    pub async fn get_task_plan_version(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, i64)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, epoch_ms) = path.into_inner();
        let response = self
            .service
            .get_task_plan_version(&scope, &task_id, epoch_ms)
            .await;
        map_result(
            response.map(|version| HttpResponse::Ok().json(TaskPlanVersionResponseV3 { version })),
        )
    }

    pub async fn restore_task_plan_version(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, i64)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, epoch_ms) = path.into_inner();
        let response = self
            .service
            .restore_task_plan_version(&scope, &task_id, epoch_ms)
            .await;
        map_result(
            response.map(|plan| HttpResponse::Ok().json(TaskPlanResponseV3::new(plan, &scope))),
        )
    }

    pub async fn delete_task_plan_version(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, i64)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, epoch_ms) = path.into_inner();
        let response = self
            .service
            .delete_task_plan_version(&scope, &task_id, epoch_ms)
            .await;
        map_result(response.map(|version_count| {
            HttpResponse::Ok().json(serde_json::json!({ "version_count": version_count }))
        }))
    }

    pub async fn approve_task_plan(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task_id = path.into_inner();
        let Some(plan_id) = required_plan_id(query.plan_id.as_deref()) else {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "plan_id_required"
            })));
        };
        let plan = self
            .service
            .approve_task_plan_version(&scope, &task_id, plan_id)
            .await;
        map_result(plan.map(|plan| HttpResponse::Ok().json(TaskPlanResponseV3::new(plan, &scope))))
    }

    pub async fn reject_task_plan(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task_id = path.into_inner();
        let Some(plan_id) = required_plan_id(query.plan_id.as_deref()) else {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "plan_id_required"
            })));
        };
        let plan = self
            .service
            .reject_task_plan_version(&scope, &task_id, plan_id)
            .await;
        map_result(plan.map(|plan| HttpResponse::Ok().json(TaskPlanResponseV3::new(plan, &scope))))
    }

    pub async fn replan_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let plan = Arc::clone(&self.service)
            .replan_task(scope.clone(), path.into_inner())
            .await;
        map_result(
            plan.map(|plan| HttpResponse::Accepted().json(TaskPlanResponseV3::new(plan, &scope))),
        )
    }

    pub async fn get_task_plan_analysis(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self.service.get_task_plan(&scope, &path.into_inner()).await;
        map_result(response.map(|plan| {
            HttpResponse::Ok().json(TaskPlanAnalysisResponseV3 {
                query_analysis: plan.query_analysis,
            })
        }))
    }

    pub async fn get_task_plan_slots(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self.service.get_task_plan(&scope, &path.into_inner()).await;
        map_result(response.map(|plan| {
            HttpResponse::Ok().json(TaskPlanSlotsResponseV3 {
                slot_graph_snapshot: plan.slot_graph_snapshot,
            })
        }))
    }

    pub async fn get_task_plan_attempts(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self.service.get_task_plan(&scope, &path.into_inner()).await;
        map_result(response.map(|plan| {
            HttpResponse::Ok().json(TaskPlanAttemptsResponseV3 {
                strategy_attempts: plan.strategy_attempts,
            })
        }))
    }

    pub async fn get_task_plan_clarifications(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self.service.get_task_plan(&scope, &path.into_inner()).await;
        map_result(response.map(|plan| {
            HttpResponse::Ok().json(TaskPlanClarificationsResponseV3 {
                clarification_history: plan.clarification_history,
            })
        }))
    }

    pub async fn get_task_plan_pending_questions(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self.service.get_task_plan(&scope, &path.into_inner()).await;
        map_result(response.map(|plan| {
            HttpResponse::Ok().json(TaskPlanPendingQuestionsResponseV3 {
                pending_questions: plan.pending_questions,
            })
        }))
    }

    pub async fn submit_task_plan_clarification(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        req: web::Json<SubmitTaskPlanClarificationV3Request>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, question_id) = path.into_inner();
        let response = Arc::clone(&self.service)
            .submit_task_plan_clarification(
                scope.clone(),
                task_id,
                question_id,
                req.response_text.trim().to_string(),
            )
            .await;
        map_result(
            response.map(|plan| HttpResponse::Ok().json(TaskPlanResponseV3::new(plan, &scope))),
        )
    }

    pub async fn resume_task_plan_clarifications(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        req: web::Json<ResumeTaskPlanClarificationsV3Request>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task_id = path.into_inner();
        let response = Arc::clone(&self.service)
            .resume_task_plan_clarifications(scope, task_id, req.into_inner().into())
            .await;
        map_result(response.map(|(plan, workflow_resumed)| {
            HttpResponse::Ok().json(TaskPlanResumeResponseV3 {
                plan,
                workflow_resumed,
            })
        }))
    }

    pub async fn list_pending_task_plan_clarifications(
        &self,
        http_req: &HttpRequest,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self
            .service
            .list_pending_task_plan_clarifications(&scope)
            .await;
        map_result(response.map(|pending| {
            HttpResponse::Ok().json(PendingTaskPlanClarificationsResponseV3 {
                pending: pending
                    .into_iter()
                    .map(|entry| PendingTaskPlanClarificationV3 {
                        principal: entry.scope.principal().to_string(),
                        workspace: entry.scope.workspace().to_string(),
                        task_id: entry.task_id,
                        task_title: entry.task_title,
                        plan_id: entry.plan_id,
                        plan_status: entry.plan_status,
                        question: entry.question,
                    })
                    .collect(),
            })
        }))
    }

    pub async fn analyze_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let response = self
            .service
            .analyze_task_goal(&scope, &path.into_inner())
            .await;
        map_result(response.map(|analysis| {
            HttpResponse::Ok().json(TaskAnalysisResponseV3 {
                task_id: analysis.task_id,
                query: analysis.query,
                analysis: analysis.analysis,
                metadata: analysis.metadata,
            })
        }))
    }

    pub async fn execute_task(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
        req: Option<web::Json<ExecuteTaskV3Request>>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let refinement = req
            .as_ref()
            .and_then(|value| value.refinement.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let explicit_delegate_to_agent = req
            .as_ref()
            .and_then(|value| value.delegate_to_agent.as_deref())
            .map(str::trim)
            .map(ToOwned::to_owned);
        let execution = Arc::clone(&self.service)
            .start_execution_with_launch_options(
                scope,
                path.into_inner(),
                refinement,
                req.as_ref()
                    .and_then(|value| value.overwrite)
                    .unwrap_or(false),
                req.as_ref()
                    .and_then(|value| value.client_llm_routing_overrides()),
                explicit_delegate_to_agent,
            )
            .await;
        map_result(execution.map(|(task, execution)| {
            HttpResponse::Accepted().json(ExecutionResponseV3 { task, execution })
        }))
    }

    pub async fn get_execution(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, execution_id) = path.into_inner();
        let execution = self
            .service
            .get_execution(&scope, &task_id, &execution_id)
            .await;
        map_result(execution.map(|execution| HttpResponse::Ok().json(execution)))
    }

    pub async fn cancel_execution(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let execution_id = path.into_inner();
        let response = self
            .service
            .cancel_execution_by_id(&scope, &execution_id)
            .await;
        map_result(response.map(|(task, execution)| {
            HttpResponse::Ok().json(CancelExecutionResponseV3 {
                task,
                execution,
                cancelled: true,
            })
        }))
    }

    /// `POST /api/magician/v3/tasks/{task_id}/executions/{execution_id}/retry-synthesis`
    ///
    /// Operator-driven recovery for executions whose output synthesis
    /// pipeline exhausted its retries. Validates the
    /// `synthesis_failed` marker, clears it atomically, re-registers
    /// the execution in `synthesis_pending_executions`, and spawns
    /// the synthesis pipeline (1.1/1.2/1.3) via the shared helper.
    /// 200 on schedule; 400/422 if the execution isn't in a
    /// retryable state (e.g. no failure marker, no recoverable
    /// outcome from event log).
    pub async fn retry_synthesis(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, execution_id) = path.into_inner();
        let response = self
            .service
            .retry_synthesis_for_execution(&scope, &task_id, &execution_id)
            .await;
        map_result(response.map(|outcome| {
            HttpResponse::Ok().json(serde_json::json!({
                "task_id": task_id,
                "execution_id": execution_id,
                // True only when this call was the race-winner and a
                // synthesis pipeline was actually spawned.
                "synthesis_retry_scheduled": outcome.was_scheduled(),
                // True when a concurrent retry beat us to it; no
                // second spawn fired. UI can use this to message the
                // operator honestly ("already retrying").
                "coalesced": outcome.was_coalesced(),
                "outcome": outcome,
            }))
        }))
    }

    pub async fn list_executions(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let executions = self
            .service
            .list_executions(&scope, &path.into_inner())
            .await;
        map_result(
            executions
                .map(|executions| HttpResponse::Ok().json(ExecutionListResponseV3 { executions })),
        )
    }

    pub async fn get_task_refs(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let refs = self.service.get_task_refs(&scope, &path.into_inner()).await;
        map_result(refs.map(|refs| HttpResponse::Ok().json(TaskRefsResponseV3 { refs })))
    }

    pub async fn get_task_outputs(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let outputs = self
            .service
            .get_task_outputs(&scope, &path.into_inner())
            .await;
        map_result(
            outputs.map(|outputs| HttpResponse::Ok().json(TaskOutputsResponseV3 { outputs })),
        )
    }

    pub async fn get_execution_refs(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, execution_id) = path.into_inner();
        let refs = self
            .service
            .get_execution_refs(&scope, &task_id, &execution_id)
            .await;
        map_result(refs.map(|refs| HttpResponse::Ok().json(ExecutionRefsResponseV3 { refs })))
    }

    pub async fn get_execution_outputs(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, execution_id) = path.into_inner();
        let outputs = self
            .service
            .get_execution_outputs(&scope, &task_id, &execution_id)
            .await;
        map_result(
            outputs.map(|outputs| HttpResponse::Ok().json(ExecutionOutputsResponseV3 { outputs })),
        )
    }

    pub async fn get_execution_delegations(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, execution_id) = path.into_inner();
        let delegations = self
            .service
            .get_execution_delegation_readiness(&scope, &task_id, &execution_id)
            .await;
        map_result(delegations.map(|delegations| {
            HttpResponse::Ok().json(ExecutionDelegationsResponseV3 { delegations })
        }))
    }

    pub async fn get_execution_schedule(
        &self,
        http_req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (task_id, execution_id) = path.into_inner();
        let schedule = self
            .service
            .get_execution_schedule_readiness(&scope, &task_id, &execution_id)
            .await;
        map_result(
            schedule
                .map(|schedule| HttpResponse::Ok().json(ExecutionScheduleResponseV3 { schedule })),
        )
    }

    pub async fn get_execution_tree(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let tree = self
            .service
            .get_execution_tree(&scope, &path.into_inner())
            .await;
        map_result(tree.map(|tree| HttpResponse::Ok().json(ExecutionTreeResponseV3 { tree })))
    }

    pub async fn get_task_progress(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let progress = self
            .service
            .get_task_progress_projection(&scope, &path.into_inner())
            .await;
        map_result(
            progress.map(|progress| HttpResponse::Ok().json(TaskProgressResponseV3 { progress })),
        )
    }

    pub async fn list_published_surfaces(
        &self,
        http_req: &HttpRequest,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let surfaces = self.service.list_published_surfaces(&scope).await;
        map_result(
            surfaces.map(|surfaces| {
                HttpResponse::Ok().json(PublishedSurfaceListResponseV3 { surfaces })
            }),
        )
    }

    pub async fn get_published_surface(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let surface = self
            .service
            .get_published_surface(&scope, &path.into_inner())
            .await;
        map_result(
            surface.map(|surface| HttpResponse::Ok().json(PublishedSurfaceResponseV3 { surface })),
        )
    }

    pub async fn get_published_surface_render(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let render = self
            .service
            .get_published_surface_render(&scope, &path.into_inner())
            .await;
        map_result(
            render
                .map(|render| HttpResponse::Ok().json(PublishedSurfaceRenderResponseV3 { render })),
        )
    }

    pub async fn list_published_surface_projections(
        &self,
        http_req: &HttpRequest,
        query: web::Query<PublishedSurfaceProjectionQueryV3>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let surfaces = self
            .service
            .list_published_surface_projections(
                &scope,
                PublishedSurfaceProjectionFilter {
                    route: query.route,
                    task_id: query.task_id,
                    agent_id: query.agent_id,
                    ui_thread_id: query.ui_thread_id,
                    placement_kind: query.placement_kind,
                    status: query.status,
                    pinned_only: query.pinned_only.unwrap_or(false),
                    limit: query.limit,
                },
            )
            .await;
        map_result(surfaces.map(|surfaces| {
            HttpResponse::Ok().json(PublishedSurfaceProjectionListResponseV3 { surfaces })
        }))
    }

    pub async fn get_published_surface_top_feed(
        &self,
        http_req: &HttpRequest,
        query: web::Query<PublishedSurfaceTopFeedQueryV3>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let top_feed = self
            .service
            .get_top_feed_published_surfaces(
                &scope,
                query.route,
                query.ui_thread_id,
                query.per_section.unwrap_or(6),
            )
            .await;
        map_result(top_feed.map(|top_feed| {
            HttpResponse::Ok().json(PublishedSurfaceTopFeedResponseV3 { top_feed })
        }))
    }

    pub async fn publish_surface(
        &self,
        http_req: &HttpRequest,
        req: web::Json<PublishSurfaceV3Request>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let surface = self
            .service
            .publish_surface_record(
                &scope,
                PublishSurfaceInput {
                    task_id: req.task_id.clone(),
                    source_output_id: req.source_output_id.clone(),
                    materialize_as: req.materialize_as.clone(),
                    logical_surface_id: req.logical_surface_id.clone(),
                    surface_kind: req.surface_kind.clone(),
                    route: req.route.clone(),
                    title: req.title.clone(),
                    summary: req.summary.clone(),
                    placement: request_placement(
                        req.placement_kind.clone(),
                        req.placement_id.clone(),
                        req.pinned,
                    ),
                },
            )
            .await;
        map_result(
            surface.map(|surface| {
                HttpResponse::Created().json(PublishedSurfaceResponseV3 { surface })
            }),
        )
    }

    pub async fn unpublish_surface(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<ScopeQuery>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let surface = self
            .service
            .unpublish_surface_record(&scope, &path.into_inner())
            .await;
        map_result(
            surface.map(|surface| HttpResponse::Ok().json(PublishedSurfaceResponseV3 { surface })),
        )
    }

    /// POST /api/magician/v3/tasks/:task_id/resynthesize_user_output
    ///
    /// Re-runs the v1.1.0 task-user-output synthesis prompt against an
    /// existing task and updates `primary_user_output_id` to the freshly
    /// synthesized OutputRef. Use this to upgrade dashboards published in
    /// the past (markdown snapshots) to richer MUI-JSON dashboards with
    /// live data bindings.
    ///
    /// Returns the new OutputRef on success. Returns 501-style Runtime
    /// error until the underlying context-reconstruction work lands in
    /// `ArtifactV2Service::resynthesize_task_user_output`.
    pub async fn resynthesize_task_user_output(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), None) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let task_id = path.into_inner();
        let result = self
            .service
            .resynthesize_task_user_output(&scope, &task_id)
            .await;
        map_result(result.map(|output| HttpResponse::Ok().json(output)))
    }

    pub async fn republish_surface(
        &self,
        http_req: &HttpRequest,
        path: web::Path<String>,
        req: web::Json<RepublishSurfaceV3Request>,
    ) -> Result<HttpResponse> {
        let scope = match resolve_required_scope_ref(http_req.headers(), req.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let surface = self
            .service
            .republish_surface_record(
                &scope,
                &path.into_inner(),
                RepublishSurfaceInput {
                    source_output_id: req.source_output_id.clone(),
                    materialize_as: req.materialize_as.clone(),
                    logical_surface_id: req.logical_surface_id.clone(),
                    surface_kind: req.surface_kind.clone(),
                    route: req.route.clone(),
                    title: req.title.clone(),
                    summary: req.summary.clone(),
                    placement: request_placement(
                        req.placement_kind.clone(),
                        req.placement_id.clone(),
                        req.pinned,
                    ),
                },
            )
            .await;
        map_result(
            surface.map(|surface| {
                HttpResponse::Created().json(PublishedSurfaceResponseV3 { surface })
            }),
        )
    }
}

fn resolve_ui_thread_id(headers: &HeaderMap, ui_thread_id: Option<String>) -> String {
    ui_thread_id
        .or_else(|| {
            headers
                .get("X-UI-Thread-Id")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "general".to_string())
}

fn is_plain_task_reference_id(task_id: &str) -> bool {
    magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::validate_task_id(task_id)
        .is_ok()
}

fn required_plan_id(plan_id: Option<&str>) -> Option<&str> {
    plan_id.map(str::trim).filter(|id| !id.is_empty())
}

fn request_placement(
    placement_kind: Option<String>,
    placement_id: Option<String>,
    pinned: Option<bool>,
) -> Option<PublishedSurfacePlacement> {
    placement_kind.map(|placement_kind| PublishedSurfacePlacement {
        placement_kind,
        placement_id,
        pinned: pinned.unwrap_or(false),
    })
}

pub(crate) fn map_result(response: Result<HttpResponse, ArtifactV2Error>) -> Result<HttpResponse> {
    match response {
        Ok(response) => Ok(response),
        Err(ArtifactV2Error::TaskNotFound(task_id)) => {
            Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "task_not_found",
                "task_id": task_id
            })))
        },
        Err(ArtifactV2Error::TaskPlanNotFound(task_id)) => {
            Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "task_plan_not_found",
                "task_id": task_id
            })))
        },
        Err(ArtifactV2Error::ExecutionNotFound(execution_id)) => {
            Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "execution_not_found",
                "execution_id": execution_id
            })))
        },
        Err(ArtifactV2Error::InvalidRequest(message)) => {
            let app_lifecycle_prefix = format!("{APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED}:");
            if let Some(task_id) = message.strip_prefix(&app_lifecycle_prefix) {
                return Ok(HttpResponse::Conflict().json(serde_json::json!({
                    "error": APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED,
                    "reason": APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED,
                    "task_id": task_id,
                    "message": "Governed app runs must be changed through app run-control and settlement APIs."
                })));
            }
            Ok(HttpResponse::BadRequest().json(serde_json::json!({ "error": message })))
        },
        // An already-answered / double-submitted clarification surfaces as
        // `AlreadyResolved`; render it as a distinguishable 409 so the frontend
        // `adapters.ts` soft-success path dismisses the card instead of erroring.
        Err(ArtifactV2Error::AlreadyResolved(message)) => {
            Ok(HttpResponse::Conflict().json(serde_json::json!({
                "error": "already_resolved",
                "reason": "already_resolved",
                "message": message
            })))
        },
        Err(ArtifactV2Error::Runtime(message))
            if message.starts_with("stateless_scope_not_activated:") =>
        {
            Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "stateless_scope_not_activated",
                "reason": "stateless_scope_not_activated",
                "message": message
            })))
        },
        Err(err) => Ok(HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err.to_string()
        }))),
    }
}

#[cfg(test)]
mod stateless_scope_activation_mapping_tests {
    use super::*;
    use actix_web::http::StatusCode;

    #[test]
    fn inactive_stateless_scope_is_a_stable_service_unavailable_response() {
        let response = map_result(Err(ArtifactV2Error::Runtime(
            "stateless_scope_not_activated:legacy_writer_cutover_required:p:w".to_owned(),
        )))
        .expect("mapping response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

pub async fn create_task_v3_handler(
    api: web::Data<TaskApiV3>,
    resources: web::Data<Arc<AgentResources>>,
    http_req: HttpRequest,
    req: web::Json<CreateTaskV3Request>,
) -> Result<HttpResponse> {
    api.create_task(&http_req, req, resources.get_ref()).await
}

pub async fn update_task_v3_handler(
    api: web::Data<TaskApiV3>,
    resources: web::Data<Arc<AgentResources>>,
    http_req: HttpRequest,
    path: web::Path<String>,
    req: web::Json<UpdateTaskV3Request>,
) -> Result<HttpResponse> {
    api.update_task(&http_req, path, req, resources.get_ref())
        .await
}

pub async fn delete_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<DeleteTaskV3Query>,
) -> Result<HttpResponse> {
    api.delete_task(&http_req, path, query).await
}

pub async fn update_task_status_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    req: web::Json<UpdateTaskStatusV3Request>,
) -> Result<HttpResponse> {
    api.update_task_status(&http_req, path, req).await
}

pub async fn approve_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.approve_task(&http_req, path, query).await
}

pub async fn get_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task(&http_req, path, query).await
}

pub async fn read_task_result_v3_handler(
    api: web::Data<TaskApiV3>,
    resources: web::Data<Arc<AgentResources>>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<ReadTaskResultRequest>,
) -> Result<HttpResponse> {
    api.read_task_result(&http_req, path, query, body, resources.get_ref())
        .await
}

pub async fn list_tasks_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<TasksListQueryV3>,
) -> Result<HttpResponse> {
    api.list_tasks(&http_req, query).await
}

pub async fn list_internal_tasks_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<InternalTasksQueryV3>,
) -> Result<HttpResponse> {
    api.list_internal_tasks(&http_req, query).await
}

pub async fn get_task_details_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TaskExecutionPageQuery>,
) -> Result<HttpResponse> {
    api.get_task_details(&http_req, path, query).await
}

pub async fn delete_internal_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.delete_internal_task(&http_req, path, query).await
}

pub async fn start_task_planning_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.start_task_planning(&http_req, path, query).await
}

pub async fn get_task_plan_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan(&http_req, path, query).await
}

pub async fn update_task_plan_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    req: web::Json<UpdateTaskPlanV3Request>,
) -> Result<HttpResponse> {
    api.update_task_plan(&http_req, path, query, req).await
}

pub async fn list_task_plan_versions_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TaskPlanVersionsQueryV3>,
) -> Result<HttpResponse> {
    api.list_task_plan_versions(&http_req, path, query).await
}

pub async fn get_task_plan_version_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, i64)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan_version(&http_req, path, query).await
}

pub async fn restore_task_plan_version_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, i64)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.restore_task_plan_version(&http_req, path, query).await
}

pub async fn delete_task_plan_version_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, i64)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.delete_task_plan_version(&http_req, path, query).await
}

pub async fn approve_task_plan_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.approve_task_plan(&http_req, path, query).await
}

pub async fn reject_task_plan_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.reject_task_plan(&http_req, path, query).await
}

pub async fn replan_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.replan_task(&http_req, path, query).await
}

pub async fn get_task_plan_analysis_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan_analysis(&http_req, path, query).await
}

pub async fn get_task_plan_slots_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan_slots(&http_req, path, query).await
}

pub async fn get_task_plan_attempts_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan_attempts(&http_req, path, query).await
}

pub async fn get_task_plan_clarifications_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan_clarifications(&http_req, path, query)
        .await
}

pub async fn get_task_plan_pending_questions_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_plan_pending_questions(&http_req, path, query)
        .await
}

pub async fn submit_task_plan_clarification_v3_handler(
    _api: web::Data<TaskApiV3>,
    _http_req: HttpRequest,
    _path: web::Path<(String, String)>,
    _req: web::Json<SubmitTaskPlanClarificationV3Request>,
) -> Result<HttpResponse> {
    // Phase H7.x — legacy resolve URL retired. The canonical
    // `/api/magician/v2/hitl/{correlation_id}/respond` endpoint with
    // `{ source: "clarification", value, task_id, execution_id? }` is the
    // sole supported entry point. The counter keeps recording hits
    // so we can identify any remaining callers that need migration.
    crate::hitl_deprecation_metrics::record_hit(
        "/api/magician/v3/tasks/{task_id}/plan/clarifications/{question_id}/respond",
    );
    Ok(HttpResponse::Gone().json(serde_json::json!({
        "error": "endpoint_retired",
        "message": "POST /api/magician/v2/hitl/{correlation_id}/respond with body { source: \"clarification\", value, task_id, execution_id? }",
        "retired_in": "magician v0.6.502 (Phase H7.x)",
        "see": "docs/plans/2026-05-11-hitl-h5-h7-dual-emit-retirement.md",
    })))
}

pub async fn resume_task_plan_clarifications_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    req: web::Json<ResumeTaskPlanClarificationsV3Request>,
) -> Result<HttpResponse> {
    api.resume_task_plan_clarifications(&http_req, path, req)
        .await
}

pub async fn list_pending_task_plan_clarifications_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.list_pending_task_plan_clarifications(&http_req, query)
        .await
}

pub async fn analyze_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.analyze_task(&http_req, path, query).await
}

pub async fn execute_task_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    req: Option<web::Json<ExecuteTaskV3Request>>,
) -> Result<HttpResponse> {
    api.execute_task(&http_req, path, query, req).await
}

pub async fn list_executions_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.list_executions(&http_req, path, query).await
}

pub async fn get_task_refs_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_refs(&http_req, path, query).await
}

pub async fn get_task_outputs_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_outputs(&http_req, path, query).await
}

pub async fn get_execution_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_execution(&http_req, path, query).await
}

pub async fn cancel_execution_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.cancel_execution(&http_req, path, query).await
}

pub async fn retry_synthesis_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.retry_synthesis(&http_req, path, query).await
}

pub async fn get_execution_refs_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_execution_refs(&http_req, path, query).await
}

pub async fn get_execution_outputs_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_execution_outputs(&http_req, path, query).await
}

pub async fn get_execution_delegations_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_execution_delegations(&http_req, path, query).await
}

pub async fn get_execution_schedule_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_execution_schedule(&http_req, path, query).await
}

pub async fn get_execution_tree_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_execution_tree(&http_req, path, query).await
}

pub async fn get_task_progress_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_task_progress(&http_req, path, query).await
}

pub async fn list_published_surfaces_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.list_published_surfaces(&http_req, query).await
}

pub async fn get_published_surface_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_published_surface(&http_req, path, query).await
}

pub async fn get_published_surface_render_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.get_published_surface_render(&http_req, path, query)
        .await
}

pub async fn list_published_surface_projections_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<PublishedSurfaceProjectionQueryV3>,
) -> Result<HttpResponse> {
    api.list_published_surface_projections(&http_req, query)
        .await
}

pub async fn get_published_surface_top_feed_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    query: web::Query<PublishedSurfaceTopFeedQueryV3>,
) -> Result<HttpResponse> {
    api.get_published_surface_top_feed(&http_req, query).await
}

pub async fn publish_surface_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    req: web::Json<PublishSurfaceV3Request>,
) -> Result<HttpResponse> {
    api.publish_surface(&http_req, req).await
}

pub async fn unpublish_surface_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> Result<HttpResponse> {
    api.unpublish_surface(&http_req, path, query).await
}

pub async fn republish_surface_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    req: web::Json<RepublishSurfaceV3Request>,
) -> Result<HttpResponse> {
    api.republish_surface(&http_req, path, req).await
}

pub async fn resynthesize_task_user_output_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
) -> Result<HttpResponse> {
    api.resynthesize_task_user_output(&http_req, path).await
}

pub async fn download_artifact_v3_handler(
    api: web::Data<TaskApiV3>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (execution_id, artifact_path) = path.into_inner();

    if execution_id.contains("..") || execution_id.contains('/') || execution_id.contains('\\') {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Invalid execution_id"
        }));
    }

    let base = match api
        .service()
        .resolve_runtime_execution_dir(&execution_id)
        .await
    {
        Ok(Some(path)) => path,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Execution directory not found"
            }))
        },
        Err(_) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve execution directory"
            }))
        },
    };

    let service = api.service();
    let workspace = service.workspace();
    let file_path = base.join(&artifact_path);
    let canonical_base = match workspace.canonicalize_path(&base).await {
        Ok(path) => path,
        Err(_) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Execution directory not found"
            }))
        },
    };
    let canonical_file = match workspace.canonicalize_path(&file_path).await {
        Ok(path) => path,
        Err(_) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Artifact not found"
            }))
        },
    };
    if !canonical_file.starts_with(&canonical_base) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Path traversal denied"
        }));
    }

    match workspace.read_path(&canonical_file).await {
        Ok(contents) => {
            let filename = canonical_file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("download")
                .replace(['"', '\\', ';', '\n', '\r'], "");
            HttpResponse::Ok()
                .insert_header((
                    "Content-Disposition",
                    format!("attachment; filename=\"{}\"", filename),
                ))
                .insert_header(("Content-Type", "application/octet-stream"))
                .body(contents)
        },
        Err(_) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Artifact not found"
        })),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Portable Export — bundle a task's user-facing outputs for sharing.
//
// `GET /api/magician/v3/tasks/{task_id}/export?format=zip|single-html`
//   * `zip` (default): a ZIP whose root `index.html` is the (URL-rewritten)
//     primary HTML deliverable and whose `outputs/` dir holds every shareable
//     output, so the bundle renders standalone after unzip.
//   * `single-html`: the primary HTML with its referenced media inlined as
//     `data:` URIs — one self-contained file to email (HTML primary required).
//
// Only user-audience outputs (`primary_task_user` + `user_media`) are included;
// agent-only JSON (continuation context etc.) never leaves. Scope + path-
// traversal safety mirror `download_task_output_v3_handler`. Resolution goes
// through `task_outputs_dir`, which probes BOTH `internal_tasks/` and `tasks/`,
// so this works regardless of where the task lives.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    pub workspace: Option<String>,
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "zip".to_string()
}

/// Strip any `; charset=…` parameter so we compare/emit a bare MIME type.
fn base_export_media_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_string()
}

/// Sanitize a value destined for a `Content-Disposition` filename.
fn sanitize_export_filename(value: &str) -> String {
    value.replace(['"', '\\', ';', '\n', '\r', '/'], "")
}

/// Regex matching THIS task's machine-generated output URLs inside an HTML
/// deliverable: `/api/magician/v3/tasks/{task_id}/outputs/{path}?…`. Capture
/// group 1 is `{path}` (stops at `?`/quote/space); the optional `?…` query is
/// matched and discarded, so BOTH the raw `&` and the HTML-escaped `&amp;`
/// query forms are handled uniformly.
fn task_output_url_regex(task_id: &str) -> regex::Regex {
    let pattern = format!(
        r#"/api/magician/v3/tasks/{}/outputs/([^"'?\s)]+)(?:\?[^"'\s)]*)?"#,
        regex::escape(task_id)
    );
    regex::Regex::new(&pattern).expect("static task-output export URL pattern")
}

/// Rewrite the task-output media URLs to bundle-relative `outputs/{path}`
/// (relative to the bundle's root `index.html`).
fn rewrite_html_urls_to_bundle(html: &str, task_id: &str) -> String {
    let re = task_output_url_regex(task_id);
    re.replace_all(html, |caps: &regex::Captures| {
        let path = caps[1].trim_start_matches("outputs/");
        format!("outputs/{path}")
    })
    .into_owned()
}

/// Replace each embedded task-output media URL with a `data:` URI built from
/// the referenced file's bytes (self-contained single-file export). Graceful:
/// on any miss / read failure / oversize the original URL is left in place.
async fn inline_html_media_as_data_uris(
    html: &str,
    task_id: &str,
    media_lookup: &std::collections::HashMap<String, String>,
    workspace: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    base: &std::path::Path,
    canonical_base: &std::path::Path,
) -> String {
    const MAX_INLINE_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB / asset
    let re = task_output_url_regex(task_id);

    // Collect unique whole-URL matches first (async work can't run inside the
    // sync regex replace closure), resolve each to a data: URI, then replace.
    let mut replacements: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for caps in re.captures_iter(html) {
        let whole = caps
            .get(0)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if whole.is_empty() || !seen.insert(whole.clone()) {
            continue;
        }
        let rel = caps[1].trim_start_matches("outputs/").to_string();
        let file = base.join(&rel);
        let canonical_file = match workspace.canonicalize_path(&file).await {
            Ok(path) if path.starts_with(canonical_base) => path,
            _ => continue, // miss / traversal → leave original URL (graceful)
        };
        match workspace.metadata_path(&canonical_file).await {
            Ok(Some(meta)) if meta.len() <= MAX_INLINE_BYTES => {},
            _ => continue, // unreadable / too big → leave original URL
        }
        let bytes = match workspace.read_path(&canonical_file).await {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let mime = media_lookup
            .get(&rel)
            .cloned()
            .or_else(|| infer_safe_inline_media_type(&canonical_file).map(base_export_media_type))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let encoded = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        };
        replacements.push((whole, format!("data:{mime};base64,{encoded}")));
    }

    let mut out = html.to_string();
    for (from, to) in replacements {
        out = out.replace(&from, &to);
    }
    out
}

/// Minimal launcher `index.html` for the no-HTML-primary case — links each
/// bundled output so the recipient can open them.
fn build_launcher_index_html(task_id: &str, files: &[(String, String, Vec<u8>)]) -> String {
    let mut items = String::new();
    for (rel, mime, _) in files {
        items.push_str(&format!(
            "<li><a href=\"outputs/{rel}\">{rel}</a> <small>({mime})</small></li>\n"
        ));
    }
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Task {task_id} — outputs</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:720px;margin:2rem auto;padding:0 1rem;\">\
         <h1>Task outputs</h1><p>Files bundled in this export:</p><ul>\n{items}</ul></body></html>"
    )
}

/// Build a ZIP in-memory from `(bundle_path, bytes)` entries. Sync — call
/// inside `spawn_blocking`.
fn build_export_zip(entries: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    let mut cursor = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            writer
                .start_file(name, options.clone())
                .map_err(|e| e.to_string())?;
            writer.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(cursor.into_inner())
}

pub async fn export_task_outputs_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let task_id = path.into_inner();
    if task_id.contains("..") || task_id.contains('/') || task_id.contains('\\') {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": "Invalid task_id" }));
    }
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let service = api.service();
    match service.get_task(&scope, &task_id).await {
        Ok(_) => {},
        Err(ArtifactV2Error::TaskNotFound(_)) => {
            return HttpResponse::NotFound().json(serde_json::json!({ "error": "Task not found" }));
        },
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "Failed to resolve task" }));
        },
    }
    let base =
        service
            .workspace()
            .task_outputs_dir(&scope.principal(), &scope.workspace(), &task_id);
    let workspace = service.workspace();
    let canonical_base = match workspace.canonicalize_path(&base).await {
        Ok(path) => path,
        Err(_) => {
            return HttpResponse::NotFound()
                .json(serde_json::json!({ "error": "Task outputs directory not found" }));
        },
    };
    let record = match service.get_task_outputs(&scope, &task_id).await {
        Ok(record) => record,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "Failed to load task outputs" }));
        },
    };

    // User-facing, shareable outputs only — never the agent JSON / continuation.
    let shareable: Vec<_> = record
        .outputs
        .iter()
        .filter(|output| output.audience == "user")
        .filter(|output| output.role == "primary_task_user" || output.role == "user_media")
        .collect();

    // The primary HTML deliverable becomes the bundle's `index.html`.
    let primary_html_rel: Option<String> = shareable
        .iter()
        .find(|output| {
            record.primary_user_output_id.as_deref() == Some(output.output_id.as_str())
                && output.role == "primary_task_user"
                && base_export_media_type(&output.media_type).eq_ignore_ascii_case("text/html")
        })
        .map(|output| {
            output
                .relative_path
                .trim_start_matches('/')
                .trim_start_matches("outputs/")
                .to_string()
        });

    // Read each shareable file safely → (rel, mime, bytes).
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new();
    for output in &shareable {
        let rel = output
            .relative_path
            .trim_start_matches('/')
            .trim_start_matches("outputs/")
            .to_string();
        let file_path = base.join(&rel);
        let canonical_file = match workspace.canonicalize_path(&file_path).await {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !canonical_file.starts_with(&canonical_base) {
            continue;
        }
        match workspace.read_path(&canonical_file).await {
            Ok(bytes) => files.push((rel, base_export_media_type(&output.media_type), bytes)),
            Err(_) => continue,
        }
    }

    if files.is_empty() {
        return HttpResponse::NotFound()
            .json(serde_json::json!({ "error": "No shareable outputs to export" }));
    }

    if query.format == "single-html" {
        let Some(html_rel) = primary_html_rel.as_ref() else {
            return HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "single-file export requires an HTML primary deliverable",
                "format": "single-html",
            }));
        };
        let Some(html_bytes) = files
            .iter()
            .find(|(rel, _, _)| rel == html_rel)
            .map(|(_, _, bytes)| bytes.clone())
        else {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "Primary HTML output unreadable" }));
        };
        let html = String::from_utf8_lossy(&html_bytes).into_owned();
        let media_lookup: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(rel, mime, _)| (rel.clone(), mime.clone()))
            .collect();
        let inlined = inline_html_media_as_data_uris(
            &html,
            &task_id,
            &media_lookup,
            workspace,
            &base,
            &canonical_base,
        )
        .await;
        return HttpResponse::Ok()
            .insert_header(("Content-Type", "text/html; charset=utf-8"))
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .insert_header((
                "Content-Disposition",
                format!(
                    "attachment; filename=\"task-{}.html\"",
                    sanitize_export_filename(&task_id)
                ),
            ))
            .body(inlined);
    }

    // Default: ZIP bundle.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    match primary_html_rel.as_ref() {
        Some(html_rel) => {
            if let Some((_, _, bytes)) = files.iter().find(|(rel, _, _)| rel == html_rel) {
                let html = String::from_utf8_lossy(bytes).into_owned();
                let rewritten = rewrite_html_urls_to_bundle(&html, &task_id);
                entries.push(("index.html".to_string(), rewritten.into_bytes()));
            }
            for (rel, _, bytes) in &files {
                if rel == html_rel {
                    continue;
                }
                entries.push((format!("outputs/{rel}"), bytes.clone()));
            }
        },
        None => {
            for (rel, _, bytes) in &files {
                entries.push((format!("outputs/{rel}"), bytes.clone()));
            }
            entries.push((
                "index.html".to_string(),
                build_launcher_index_html(&task_id, &files).into_bytes(),
            ));
        },
    }

    let zip_bytes = match tokio::task::spawn_blocking(move || build_export_zip(entries)).await {
        Ok(Ok(bytes)) => bytes,
        _ => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "Failed to build export archive" }));
        },
    };

    HttpResponse::Ok()
        .insert_header(("Content-Type", "application/zip"))
        .insert_header((
            "Content-Disposition",
            format!(
                "attachment; filename=\"task-{}.zip\"",
                sanitize_export_filename(&task_id)
            ),
        ))
        .body(zip_bytes)
}

pub async fn download_task_output_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (task_id, artifact_path) = path.into_inner();
    let canonical_file =
        match resolve_task_output_absolute_path(&api, &http_req, task_id, artifact_path, &query)
            .await
        {
            Ok(path) => path,
            Err(response) => return response,
        };
    match api.service().workspace().read_path(&canonical_file).await {
        Ok(contents) => build_task_output_download_response(&canonical_file, contents),
        Err(_) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Task output not found"
        })),
    }
}

/// Validate + canonicalize a `(task_id, artifact_path)` pair into an
/// absolute filesystem path that's guaranteed to live under the task's
/// outputs directory. Shared by the download / open-folder / open-file
/// handlers so the security checks (path-traversal rejection, scope
/// resolution, task existence) live in one place.
///
/// Returns `Err(HttpResponse)` on any validation failure — caller can
/// just `?` it. The error responses match what the download handler
/// has been emitting (NotFound / BadRequest / Forbidden / Internal).
async fn resolve_task_output_absolute_path(
    api: &TaskApiV3,
    http_req: &HttpRequest,
    task_id: String,
    artifact_path: String,
    query: &ScopeQuery,
) -> Result<std::path::PathBuf, HttpResponse> {
    if task_id.contains("..") || task_id.contains('/') || task_id.contains('\\') {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Invalid task_id"
        })));
    }
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Err(response),
    };

    match api.service().get_task(&scope, &task_id).await {
        Ok(_) => {},
        Err(ArtifactV2Error::TaskNotFound(_)) => {
            return Err(HttpResponse::NotFound().json(serde_json::json!({
                "error": "Task not found"
            })));
        },
        Err(_) => {
            return Err(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve task"
            })));
        },
    }
    let service = api.service();
    let workspace = service.workspace();
    let base = workspace.task_outputs_dir(&scope.principal(), &scope.workspace(), &task_id);
    // Execution-level outputs use paths like `executions/<id>/outputs/<file>`
    // which are relative to the task root (parent of the `outputs/` dir).
    // Task-level outputs use `outputs/<file>` (or bare `<file>`) relative to
    // `task_outputs_dir`. Route each form to the correct base and security root.
    let normalized = artifact_path.trim_start_matches('/');
    let (file_path, security_root) = if normalized.starts_with("executions/") {
        let task_root = base.parent().unwrap_or(base.as_path()).to_path_buf();
        (task_root.join(normalized), task_root)
    } else {
        // Strip legacy leading `outputs/` — task_outputs_dir already ends in `/outputs/`.
        let stripped = normalized.trim_start_matches("outputs/");
        (base.join(stripped), base.clone())
    };
    let canonical_security_root = match workspace.canonicalize_path(&security_root).await {
        Ok(path) => path,
        Err(_) => {
            return Err(HttpResponse::NotFound().json(serde_json::json!({
                "error": "Task outputs directory not found"
            })));
        },
    };
    let canonical_file = match workspace.canonicalize_path(&file_path).await {
        Ok(path) => path,
        Err(_) => {
            return Err(HttpResponse::NotFound().json(serde_json::json!({
                "error": "Task output not found"
            })));
        },
    };
    if !canonical_file.starts_with(&canonical_security_root) {
        return Err(HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Path traversal denied"
        })));
    }
    Ok(canonical_file)
}

#[derive(Debug, Deserialize)]
pub struct OpenTaskOutputRequest {
    /// Relative path within the task's outputs/ dir (e.g. `report.pdf`
    /// or `nested/foo.html`). Required.
    pub relative_path: String,
}

/// POST /api/magician/v3/tasks/{task_id}/outputs/open-folder
///
/// Reveal a task-output file in the host OS's file manager (Finder on
/// macOS, Explorer on Windows, xdg-open on Linux). The task-scoped
/// counterpart to the chat-session `open-folder` endpoint — needed
/// because ExecutionPanel renders task outputs without a chat session,
/// so the session-scoped handler isn't reachable.
///
/// File path comes in the POST body (not the URL) so the route
/// doesn't collide with the catchall `/outputs/{artifact_path:.*}`
/// download route.
pub async fn open_task_output_folder_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<OpenTaskOutputRequest>,
) -> HttpResponse {
    let task_id = path.into_inner();
    let resolved = match resolve_task_output_absolute_path(
        &api,
        &http_req,
        task_id,
        body.relative_path.clone(),
        &query,
    )
    .await
    {
        Ok(path) => path,
        Err(response) => return response,
    };
    let folder = crate::chat_api::containing_folder_path(&resolved).to_path_buf();
    match crate::chat_api::open_folder_in_file_manager(&resolved).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "folder_path": folder.to_string_lossy(),
            "file_path": resolved.to_string_lossy(),
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Failed to reveal folder",
            "details": error.to_string(),
        })),
    }
}

/// POST /api/magician/v3/tasks/{task_id}/outputs/open-file
///
/// Hand a task-output file to the OS's default opener (Preview, Word,
/// VLC, the default browser — whatever the user has registered for the
/// mime type). Counterpart to `open-folder` for "open this file in its
/// native app instead of in the browser tab".
pub async fn open_task_output_file_v3_handler(
    api: web::Data<TaskApiV3>,
    http_req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<OpenTaskOutputRequest>,
) -> HttpResponse {
    let task_id = path.into_inner();
    let resolved = match resolve_task_output_absolute_path(
        &api,
        &http_req,
        task_id,
        body.relative_path.clone(),
        &query,
    )
    .await
    {
        Ok(path) => path,
        Err(response) => return response,
    };
    match crate::chat_api::open_file_with_os_default(&resolved).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "file_path": resolved.to_string_lossy(),
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Failed to open file",
            "details": error.to_string(),
        })),
    }
}

fn build_task_output_download_response(path: &std::path::Path, contents: Vec<u8>) -> HttpResponse {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .replace(['"', '\\', ';', '\n', '\r'], "");
    let mut response = HttpResponse::Ok();
    response.insert_header(("X-Content-Type-Options", "nosniff"));
    if let Some(content_type) = infer_safe_inline_media_type(path) {
        response
            .insert_header(("Content-Type", content_type))
            .insert_header((
                "Content-Disposition",
                format!("inline; filename=\"{}\"", filename),
            ))
            .body(contents)
    } else {
        response
            .insert_header(("Content-Type", "application/octet-stream"))
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"{}\"", filename),
            ))
            .body(contents)
    }
}

/// Map a file extension to a media type the browser can safely render
/// inline (returns `Some(mime)` → handler sends `Content-Disposition:
/// inline`; `None` → falls through to `attachment`, which triggers a
/// browser download dialog).
///
/// We deliberately enumerate inline-safe types instead of letting every
/// extension render inline:
///   * `X-Content-Type-Options: nosniff` is already set, so the browser
///     can't sniff us into a different content type.
///   * Path traversal is prevented by the canonicalize() check upstream,
///     so the served bytes always live inside the task's outputs dir
///     (workspace-owned content, written by the magician runtime).
///   * Inline-rendered HTML / SVG can execute same-origin JS, which
///     means a compromised task output could read same-origin cookies
///     or call same-origin APIs. We accept that for workspace-owned
///     deliverables — agents that publish reports / dashboards / static
///     pages need them to actually render. Compromise of the runtime
///     itself is out of scope here; the artifact API doesn't accept
///     uploads from arbitrary callers.
///
/// Falls back to `attachment` for anything we don't recognize so
/// unknown / proprietary formats become explicit downloads instead of
/// half-rendered noise.
pub(crate) fn infer_safe_inline_media_type(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        // Images — browsers render inline by default.
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("avif") => Some("image/avif"),
        Some("svg") => Some("image/svg+xml; charset=utf-8"),

        // PDF — browsers render via built-in PDF.js / native plugin.
        Some("pdf") => Some("application/pdf"),

        // HTML — task deliverables like reports / dashboards / generated
        // pages. Inline so they render instead of forcing a download.
        //
        // `charset=utf-8` is REQUIRED on every text type below. Our outputs are
        // written as UTF-8 (typographic curly quotes/apostrophes etc.), but with
        // `X-Content-Type-Options: nosniff` set the browser cannot sniff the
        // encoding, and an HTML fragment carries no `<meta charset>` — so a bare
        // `text/html` makes the browser fall back to its locale default
        // (Windows-1252) and render correct UTF-8 bytes as mojibake (`â€œ` for `"`).
        // Declaring the charset on the wire fixes it for all text deliverables.
        Some("html") | Some("htm") => Some("text/html; charset=utf-8"),
        Some("xhtml") => Some("application/xhtml+xml; charset=utf-8"),

        // Text family — markdown / JSON / plain log / CSV / TSV.
        // Inline so the chat / panel "preview" buttons fetch + display
        // them without the browser intercepting the response as a
        // download.
        Some("md") | Some("markdown") => Some("text/markdown; charset=utf-8"),
        Some("json") => Some("application/json; charset=utf-8"),
        Some("txt") | Some("log") => Some("text/plain; charset=utf-8"),
        Some("csv") => Some("text/csv; charset=utf-8"),
        Some("tsv") => Some("text/tab-separated-values; charset=utf-8"),

        // Video — native <video> playback.
        Some("mp4") | Some("m4v") => Some("video/mp4"),
        Some("webm") => Some("video/webm"),
        Some("ogv") => Some("video/ogg"),
        Some("mov") => Some("video/quicktime"),

        // Audio — native <audio> playback.
        Some("mp3") => Some("audio/mpeg"),
        Some("wav") => Some("audio/wav"),
        Some("oga") | Some("ogg") => Some("audio/ogg"),
        Some("flac") => Some("audio/flac"),
        Some("aac") => Some("audio/aac"),

        // Unknown extensions → attachment.
        _ => None,
    }
}

#[cfg(test)]
mod media_type_tests {
    use super::*;

    #[test]
    fn text_outputs_are_served_with_utf8_charset() {
        // UTF-8 deliverables (typographic curly quotes/apostrophes etc.) MUST
        // declare charset on the wire: with `X-Content-Type-Options: nosniff`
        // set and an HTML fragment carrying no `<meta charset>`, a bare
        // `text/html` makes the browser fall back to Windows-1252 and render
        // correct UTF-8 bytes as mojibake (`â€œ` for `"`).
        for (name, expected) in [
            ("report.html", "text/html; charset=utf-8"),
            ("page.htm", "text/html; charset=utf-8"),
            ("notes.md", "text/markdown; charset=utf-8"),
            ("data.json", "application/json; charset=utf-8"),
            ("log.txt", "text/plain; charset=utf-8"),
            ("rows.csv", "text/csv; charset=utf-8"),
            ("diagram.svg", "image/svg+xml; charset=utf-8"),
        ] {
            assert_eq!(
                infer_safe_inline_media_type(std::path::Path::new(name)),
                Some(expected),
                "{name} must be served with an explicit UTF-8 charset"
            );
        }
    }

    #[test]
    fn binary_outputs_carry_no_charset() {
        // Images / PDF / media are binary — a charset param would be meaningless.
        for name in ["photo.jpg", "art.png", "doc.pdf", "clip.mp4", "song.mp3"] {
            let content_type = infer_safe_inline_media_type(std::path::Path::new(name))
                .expect("known binary type should be inline-safe");
            assert!(
                !content_type.contains("charset"),
                "{name} -> {content_type} should not carry a charset"
            );
        }
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn rewrite_html_urls_handles_escaped_and_raw_queries() {
        let task = "task_abc123";

        // Stored-HTML form: HTML-escaped `&amp;` query + double quotes.
        let escaped = format!(
            r#"<img src="/api/magician/v3/tasks/{task}/outputs/comic.jpg?principal=anon&amp;workspace=default">"#
        );
        let out = rewrite_html_urls_to_bundle(&escaped, task);
        assert!(out.contains(r#"src="outputs/comic.jpg""#), "got: {out}");
        assert!(
            !out.contains("/api/magician/v3"),
            "api url must be gone: {out}"
        );

        // Live form: raw `&` query + single quotes + nested path.
        let raw = format!(
            r#"<img src='/api/magician/v3/tasks/{task}/outputs/sub/dir/img.png?principal=anon&workspace=default'>"#
        );
        let out2 = rewrite_html_urls_to_bundle(&raw, task);
        assert!(
            out2.contains("src='outputs/sub/dir/img.png'"),
            "got: {out2}"
        );

        // A DIFFERENT task's URL must be left untouched (rewrite is task-scoped).
        let other = r#"<img src="/api/magician/v3/tasks/task_other/outputs/x.jpg">"#.to_string();
        assert_eq!(rewrite_html_urls_to_bundle(&other, task), other);
    }

    #[test]
    fn base_export_media_type_strips_charset() {
        assert_eq!(
            base_export_media_type("text/html; charset=utf-8"),
            "text/html"
        );
        assert_eq!(base_export_media_type("image/jpeg"), "image/jpeg");
    }

    #[tokio::test]
    async fn inline_html_media_as_data_uris_reads_media_through_workspace_provider() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace =
            magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(tmp.path());
        let task_id = "task_inline_media";
        let base = workspace.task_outputs_dir("principal-a", "workspace-a", task_id);
        workspace
            .create_dir_all_path(&base)
            .await
            .expect("create output dir");
        workspace
            .write_path(&base.join("chart.png"), b"png")
            .await
            .expect("write media");
        let canonical_base = workspace
            .canonicalize_path(&base)
            .await
            .expect("canonical base");
        let media_path = base.join("chart.png");
        let canonical_media = workspace
            .canonicalize_path(&media_path)
            .await
            .expect("canonical media");
        assert!(
            canonical_media.starts_with(&canonical_base),
            "media must stay under base: media={}, base={}",
            canonical_media.display(),
            canonical_base.display()
        );
        assert_eq!(
            workspace
                .metadata_path(&canonical_media)
                .await
                .expect("metadata result")
                .expect("metadata present")
                .len(),
            3
        );
        assert_eq!(
            workspace
                .read_path(&canonical_media)
                .await
                .expect("read media"),
            b"png"
        );
        let html = format!(
            r#"<img src="/api/magician/v3/tasks/{task_id}/outputs/chart.png?principal=principal-a&workspace=workspace-a">"#
        );
        let media_lookup = HashMap::from([("chart.png".to_string(), "image/png".to_string())]);

        let actual = inline_html_media_as_data_uris(
            &html,
            task_id,
            &media_lookup,
            &workspace,
            &base,
            &canonical_base,
        )
        .await;

        assert!(
            actual.contains(r#"src="data:image/png;base64,cG5n""#),
            "got: {actual}"
        );
    }
}

/// The storage index's read path, through the tasks handler.
///
/// The index reaches a handler through a `OnceLock` only `bin/magician.rs`
/// fills in production, so every other test in this crate builds a service
/// with no index and takes the fallback walk. That makes the rest of the
/// suite a complete proof the WALK still answers every case — and the reason
/// nothing exercised a handler CHOOSING the index.
///
/// Every test here turns on the same discriminator: make the index
/// deliberately disagree with the disk (`ListIndex::remove` drops one row)
/// and ask which answer comes back. Comparing two sources that agree would
/// pass whichever one served the request.
#[cfg(test)]
mod task_list_index_read_path_tests {
    use super::*;

    // `crate::*` already carries `CreateTaskInput`, `ScopeRef`, `TaskLane`,
    // `TaskOutputMode`, `ListKind` and `V3ReadApi`; only the two lifecycle
    // enums are new here.
    use actix_web::test;
    use magician::magician_v2::artifact_v2::models::{TaskLifecycle, TaskSyncMode};
    use magician::magician_v2::test_support::{
        build_test_artifact_v2_service, wire_test_list_index, wire_unready_test_list_index,
    };
    use tempfile::TempDir;

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";
    /// The READER's local date. Passed explicitly everywhere, because the
    /// server cannot know it and a lane computed in the wrong day is a wrong
    /// answer that looks entirely right.
    const TODAY: &str = "2026-07-30";
    const YESTERDAY: &str = "2026-07-29";
    /// Today's date carrying a wall-clock time. `due_date` is free text and a
    /// client that sends a full timestamp is not wrong, so the Today lane is a
    /// PREFIX match on both sides. Without a row of this shape in the API
    /// corpus, turning that prefix into an equality passes every test here
    /// while failing the index's own — the lane would simply lose every
    /// timestamped task, on a surface with two clients.
    const TODAY_AT_A_WALL_CLOCK_TIME: &str = "2026-07-30T14:45:00+00:00";

    fn api() -> (TempDir, TaskApiV3) {
        let tmp = TempDir::new().expect("tempdir");
        let service = build_test_artifact_v2_service(tmp.path());
        (tmp, TaskApiV3::from_service(service))
    }

    fn scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(&PRINCIPAL.to_string(), &WORKSPACE.to_string())
    }

    /// One list request, straight at the handler — no route table, because
    /// what is under test is the branch the handler takes, not the routing.
    async fn list(api: &TaskApiV3, query: &str) -> serde_json::Value {
        let http_req = test::TestRequest::default()
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_http_request();
        let parsed = web::Query::<TasksListQueryV3>::from_query(query)
            .unwrap_or_else(|error| panic!("query `{query}` should parse: {error}"));
        let response = api
            .list_tasks(&http_req, parsed)
            .await
            .expect("the list handler should not error");
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::OK,
            "query `{query}` should be a 200"
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("reading the list body");
        serde_json::from_slice(&body).expect("the list body should be JSON")
    }

    fn ids(page: &serde_json::Value) -> Vec<String> {
        page["tasks"]
            .as_array()
            .expect("tasks")
            .iter()
            .map(|task| task["id"].as_str().expect("id").to_string())
            .collect()
    }

    /// Create one task through the ordinary service path — the same write
    /// that reconciles the index in production.
    async fn create(
        api: &TaskApiV3,
        title: &str,
        status: &str,
        due_date: Option<&str>,
        tags: &[&str],
    ) -> String {
        let task = api
            .service()
            .create_task(CreateTaskInput {
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                title: title.to_string(),
                description: String::new(),
                agent_id: "personal-assistant".to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: due_date.map(ToOwned::to_owned),
                tags: tags
                    .iter()
                    .map(
                        |tag| magician::magician_v2::artifact_v2::models::TaskTagRecord {
                            id: format!("tag-{tag}"),
                            name: (*tag).to_string(),
                            color: None,
                        },
                    )
                    .collect(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::default(),
                chat_session_id: None,
                lifecycle: TaskLifecycle::Persistent,
                sync_mode: TaskSyncMode::default(),
            })
            .await
            .expect("creating a task");
        let task_id = task.manifest.task_id.clone();
        if status != "pending" {
            api.service()
                .update_task_status(&scope(), &task_id, status)
                .await
                .expect("setting a task status");
        }
        task_id
    }

    /// A corpus that puts something in every lane and leaves something out
    /// of every lane, so a lane comparison below can pass neither by
    /// matching nothing nor by matching everything. It also carries both
    /// shapes of due date — a bare `YYYY-MM-DD` and a full timestamp — since
    /// the store holds both and one lane is defined by the difference.
    ///
    /// Returned in creation order; `[0]` is the untagged pending task the
    /// filter tests name, because it is the one every fallback query below
    /// is guaranteed to return.
    async fn seed_lane_corpus(api: &TaskApiV3) -> Vec<String> {
        vec![
            create(api, "untagged and pending", "pending", None, &[]).await,
            // `paused`, not `running`, and deliberately: `update_task_status`
            // rejects "running" (`normalize_task_status` accepts pending,
            // ready, paused, completed, failed, cancelled, deferred and
            // archived only) — the execution path writes that status, no API
            // call can. `TaskLane::Running` is `running || paused`, so this
            // still lands in the Running lane; the `running` half of that
            // predicate is covered where fixtures are built directly, in
            // `list_index`'s lane-agreement test.
            create(api, "due today, paused", "paused", Some(TODAY), &["work"]).await,
            create(
                api,
                "due today at a wall clock time",
                "pending",
                Some(TODAY_AT_A_WALL_CLOCK_TIME),
                &["work"],
            )
            .await,
            create(
                api,
                "due yesterday, open",
                "pending",
                Some(YESTERDAY),
                &["home"],
            )
            .await,
            create(api, "finished", "completed", Some(YESTERDAY), &[]).await,
            create(api, "paused", "paused", None, &[]).await,
        ]
    }

    #[actix_web::test]
    async fn an_indexed_page_is_the_page_the_walk_serves() {
        let (_tmp, api) = api();
        seed_lane_corpus(&api).await;

        // Recorded BEFORE any index exists. `set_list_index` is a OnceLock,
        // so walk-then-index is the only available direction — and it is the
        // useful one: the walk's answer cannot have been influenced by the
        // thing it is about to be compared against.
        let walked_first = list(&api, "limit=2").await;
        let walked_cursor = walked_first["pagination"]["next_cursor"]
            .as_str()
            .expect("a next cursor")
            .to_string();
        let walked_second = list(&api, &format!("limit=2&cursor={walked_cursor}")).await;
        let walked_offset = list(&api, "limit=2&offset=2").await;

        wire_test_list_index(&api.service());

        assert_eq!(
            list(&api, "limit=2").await,
            walked_first,
            "the index must serve the identical page — tasks, order, total, \
             offset and cursor — or a reader's list changes when a cache warms"
        );
        assert_eq!(
            list(&api, &format!("limit=2&cursor={walked_cursor}")).await,
            walked_second,
            "a cursor minted by the walk must resolve to the same place in the index"
        );
        assert_eq!(
            list(&api, "limit=2&offset=2").await,
            walked_offset,
            "and an offset page must land in the same place too"
        );
    }

    #[actix_web::test]
    async fn the_handler_really_reads_the_index_and_not_the_walk() {
        let (_tmp, api) = api();
        seed_lane_corpus(&api).await;
        let index = wire_test_list_index(&api.service());

        let before = list(&api, "limit=50").await;
        assert_eq!(before["pagination"]["total"], 6);
        let dropped = ids(&before).first().cloned().expect("a task");

        // Drop ONE row from the index and leave the record on disk untouched.
        // Only a handler reading the index can notice.
        assert!(
            index
                .remove(ListKind::Task, &dropped)
                .expect("removing a task row"),
            "the rebuild must have written a row to remove"
        );

        let after = list(&api, "limit=50").await;
        assert_eq!(
            after["pagination"]["total"], 5,
            "the walk would still have found five tasks on disk; a total of four \
             is proof the index answered this request"
        );
        assert!(
            !ids(&after).contains(&dropped),
            "the row removed from the index must be the one missing from the page"
        );

        // The record is untouched, which is what makes the assertion above
        // about the READ path rather than about a deletion.
        assert!(
            api.service().get_task(&scope(), &dropped).await.is_ok(),
            "detail reads the record, not the index, so it still resolves"
        );
    }

    #[actix_web::test]
    async fn an_unready_index_is_never_read() {
        let (_tmp, api) = api();
        // Wired BEFORE the tasks exist and never rebuilt, so it is unready for
        // the whole test — the state a reader meets during a rebuild, and
        // still meets after a crash partway through one.
        let index = wire_unready_test_list_index(&api.service());
        seed_lane_corpus(&api).await;
        assert!(!index.is_ready().expect("readiness"));

        // The write hook kept it current anyway, so make it disagree: a
        // handler that consulted it would drop this task.
        let dropped = ids(&list(&api, "limit=50").await)
            .first()
            .cloned()
            .expect("a task");
        assert!(
            index
                .remove(ListKind::Task, &dropped)
                .expect("removing a task row"),
            "the write hook must have written a row, or an unready index holding \
             nothing would agree with the walk by accident"
        );

        let page = list(&api, "limit=50").await;
        assert_eq!(
            page["pagination"]["total"], 6,
            "a half-built index looks exactly like a complete one holding fewer \
             tasks — which is why `is_ready()` gates the read at all"
        );
        assert!(ids(&page).contains(&dropped));
    }

    /// **The lane definition, proved end to end.** `list_index`'s own tests
    /// compare the SQL against `TaskLane::matches` over a fixture; nothing
    /// proved that the lane a HANDLER serves out of the index is that lane.
    /// Two definitions of `overdue` would make the list simply wrong, and
    /// confidently so.
    #[actix_web::test]
    async fn every_lane_the_index_serves_is_the_lane_the_walk_serves() {
        let (_tmp, api) = api();
        let seeded = seed_lane_corpus(&api).await;

        let mut walked = Vec::new();
        for lane in TaskLane::ALL {
            let query = format!("limit=50&view={}&today={TODAY}", lane.wire_name());
            let page = list(&api, &query).await;
            let lane_ids = ids(&page);
            // A lane that matched nothing, or everything, would agree with a
            // predicate of `1 = 1` just as well.
            assert!(
                !lane_ids.is_empty() && lane_ids.len() < seeded.len(),
                "the corpus must exercise `{}` without swallowing it: {lane_ids:?}",
                lane.wire_name()
            );
            walked.push((query, page));
        }

        wire_test_list_index(&api.service());

        for (query, expected) in &walked {
            assert_eq!(
                &list(&api, query).await,
                expected,
                "the index and the walk disagree about `{query}`"
            );
        }
    }

    /// Counts are the other half of a lane: they run over the WHOLE scoped
    /// pool, before any lane filter, so a badge cannot report the lane the
    /// reader is already looking at.
    #[actix_web::test]
    async fn indexed_counts_are_the_walks_counts() {
        let (_tmp, api) = api();
        seed_lane_corpus(&api).await;

        let walked = list(&api, &format!("limit=1&today={TODAY}")).await;
        let walked_in_a_lane = list(&api, &format!("limit=1&view=inbox&today={TODAY}")).await;
        assert_eq!(
            walked["counts"], walked_in_a_lane["counts"],
            "counts run over the whole pool, so filtering to a lane must not move them"
        );

        wire_test_list_index(&api.service());

        assert_eq!(list(&api, &format!("limit=1&today={TODAY}")).await, walked);
        assert_eq!(
            list(&api, &format!("limit=1&view=inbox&today={TODAY}")).await,
            walked_in_a_lane
        );
        assert!(
            walked["counts"]["all"].as_u64().expect("all") > 0,
            "the counts must be non-empty, or this compares two absences"
        );
    }

    /// The index declines anything it cannot express exactly, and the walk
    /// takes it. A `status=` page served from the index would be a page of a
    /// filter the index does not carry.
    #[actix_web::test]
    async fn a_request_the_index_cannot_express_still_reaches_the_walk() {
        let (_tmp, api) = api();
        let seeded = seed_lane_corpus(&api).await;
        let index = wire_test_list_index(&api.service());

        // The untagged pending task, which every query below matches, so
        // "is it there?" is a real question with a known answer.
        let dropped = seeded[0].clone();
        assert!(index
            .remove(ListKind::Task, &dropped)
            .expect("removing a task row"));

        // Each of these names a filter the index does not carry, or an order
        // it cannot page. Each must therefore come back off the disk — with
        // the row the index no longer knows about still in it.
        for (query, expected_total) in [
            ("limit=50&status=pending", 3),
            ("limit=50&query=untagged", 1),
            ("limit=50&sort=title", 6),
            ("limit=50&sort=updated_at&order=asc", 6),
        ] {
            let page = list(&api, query).await;
            assert!(
                ids(&page).contains(&dropped),
                "`{query}` must be answered by the walk, which still sees every \
                 record on disk"
            );
            assert_eq!(
                page["pagination"]["total"], expected_total,
                "`{query}` total"
            );
        }

        // And the one that IS the index's question still is.
        assert_eq!(
            list(&api, "limit=50").await["pagination"]["total"],
            5,
            "declining must be per-request, not a latch that turns the index off"
        );
    }

    /// The legacy unpaginated body is entered before any of the paginated
    /// path's work, so it can never acquire a field, an order or a failure
    /// mode from a change made for paginated callers — including this one.
    #[actix_web::test]
    async fn the_legacy_body_never_meets_the_index() {
        let (_tmp, api) = api();
        seed_lane_corpus(&api).await;
        let index = wire_test_list_index(&api.service());

        let dropped = ids(&list(&api, "limit=50").await)
            .first()
            .cloned()
            .expect("a task");
        assert!(index
            .remove(ListKind::Task, &dropped)
            .expect("removing a task row"));

        let legacy = list(&api, "").await;
        assert!(
            legacy.get("pagination").is_none(),
            "the legacy body carries no pagination envelope"
        );
        assert_eq!(
            legacy["tasks"].as_array().expect("tasks").len(),
            6,
            "a caller sending no limit, offset or cursor is answered from the walk, \
             so the row missing from the index is still in its list"
        );
    }
}

#[cfg(test)]
mod governed_app_lifecycle_route_tests {
    use super::*;
    use actix_web::{body::to_bytes, http::StatusCode, test};
    use magician::magician_v2::test_support::build_test_artifact_v2_service;

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";

    fn request() -> HttpRequest {
        test::TestRequest::default()
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_http_request()
    }

    async fn body(response: HttpResponse) -> serde_json::Value {
        let bytes = to_bytes(response.into_body())
            .await
            .expect("read response body");
        serde_json::from_slice(&bytes).expect("JSON response body")
    }

    #[actix_web::test]
    async fn generic_status_and_delete_routes_return_conflict_for_app_runs() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let api = TaskApiV3::from_service(build_test_artifact_v2_service(tmp.path()));
        let task_id = format!("task_app_{}", "a".repeat(64));

        let status_response = api
            .update_task_status(
                &request(),
                web::Path::from(task_id.clone()),
                web::Json(UpdateTaskStatusV3Request {
                    status: "cancelled".to_string(),
                    workspace: None,
                }),
            )
            .await
            .expect("status handler response");
        assert_eq!(status_response.status(), StatusCode::CONFLICT);
        let status_body = body(status_response).await;
        assert_eq!(status_body["error"], APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED);
        assert_eq!(status_body["task_id"], task_id);

        let delete_response = api
            .delete_task(
                &request(),
                web::Path::from(task_id.clone()),
                web::Query(DeleteTaskV3Query {
                    workspace: None,
                    remove_files: true,
                }),
            )
            .await
            .expect("delete handler response");
        assert_eq!(delete_response.status(), StatusCode::CONFLICT);
        let delete_body = body(delete_response).await;
        assert_eq!(delete_body["reason"], APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED);
        assert_eq!(delete_body["task_id"], task_id);

        let internal_delete_response = api
            .delete_internal_task(
                &request(),
                web::Path::from(task_id.clone()),
                web::Query(ScopeQuery {
                    workspace: None,
                    plan_id: None,
                }),
            )
            .await
            .expect("internal delete handler response");
        assert_eq!(internal_delete_response.status(), StatusCode::CONFLICT);
        let internal_delete_body = body(internal_delete_response).await;
        assert_eq!(
            internal_delete_body["error"],
            APP_WORKFLOW_GENERIC_LIFECYCLE_DENIED
        );
        assert_eq!(internal_delete_body["task_id"], task_id);
    }
}
