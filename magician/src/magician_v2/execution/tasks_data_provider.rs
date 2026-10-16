//! Scoped, read-only Apps binder for the owner's task list.
//!
//! The sixth host-read binder. `list_tasks` and `get_task_details` are
//! agent-facing tools with no app implementation identity, so a package could
//! not show the owner's tasks at all. This binder is the narrow face an app is
//! allowed to see: identity, title, status, owning agent, priority, due date,
//! tag names, timestamps, and a few booleans. It never projects plans,
//! executions, chat sessions, pending-question bodies, schedules or output
//! routing — those carry prompts, conversation content and authority that an
//! app has no business reading.
//!
//! Both actions read through `V3ReadApi::list_tasks`, including the exact read.
//! That keeps one visibility rule (the listing already hides non-user-visible
//! system tasks, and an exact read must not reveal a task the listing would
//! hide) and it never takes the task write guard: `get_task` acquires it to
//! replay interrupted multi-file writes, which is recovery a read binder must
//! not trigger.
//!
//! Read-only (the binder-family invariant): no path here creates, edits,
//! runs, stops or deletes a task, and a compile-time assertion below refuses
//! an action name that reads like one.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use magicllm::LlmScope;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::artifact_v2::service::{ArtifactV2Service, ScopeRef};
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;

pub const TASKS_DATA_TOOL_NAME: &str = "tasks_data";

pub(crate) const APP_BOUND_TASKS_DATA_INPUT_CEILING: u64 = 4 * 1024;
pub(crate) const APP_BOUND_TASKS_DATA_RESULT_CEILING: u64 = 512 * 1024;

const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: u64 = 200;
const MAX_ID_BYTES: usize = 255;
/// `created_at` (RFC 3339) + `|` + task id, with headroom.
const MAX_CURSOR_BYTES: usize = 320;
const MAX_STATUS_BYTES: usize = 32;
const MAX_TITLE_BYTES: usize = 512;
const MAX_TEXT_BYTES: usize = 4 * 1024;
const MAX_TAGS: usize = 32;

const ACTIONS: &[&str] = &["list_tasks", "read_task"];

#[derive(Clone)]
pub struct TasksDataProvider {
    /// The artifact service that owns tasks. Optional because the process-wide
    /// registry binds this provider before a service exists; the per-scope
    /// registries the app path reads attach it. Without it every call fails
    /// with an explicit error rather than answering "no tasks".
    service: Option<Arc<ArtifactV2Service>>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for TasksDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksDataProvider")
            .field("has_service", &self.service.is_some())
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl Default for TasksDataProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TasksDataProvider {
    pub fn new() -> Self {
        Self {
            service: None,
            pack_def: None,
        }
    }

    pub fn with_service(mut self, service: Arc<ArtifactV2Service>) -> Self {
        self.service = Some(service);
        self
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for TasksDataProvider {
    fn tool_name(&self) -> &str {
        TASKS_DATA_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_tasks_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: TASKS_DATA_TOOL_NAME.to_owned(),
            implementation: ImplementationType::Compiled {
                provider_name: TASKS_DATA_TOOL_NAME.to_owned(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => {
                return Err(ExecutionError::Step(
                    "tasks_data: unexpected action type".to_owned(),
                ))
            },
        };
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "list_tasks".to_owned());
        let mut params = authorize_runtime_scope(params)?;
        params.insert(
            "__action_name".to_owned(),
            Value::String(action_name.clone()),
        );
        if !prove_app_tasks_args(&params) {
            return Err(ExecutionError::Step(
                "tasks_data arguments are outside the closed action schema".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&params).map_err(|error| {
            ExecutionError::Step(format!("tasks_data argument serialization failed: {error}"))
        })?;
        if input_bytes.len() as u64 > APP_BOUND_TASKS_DATA_INPUT_CEILING {
            return Err(ExecutionError::Step(format!(
                "tasks_data arguments exceeded the {APP_BOUND_TASKS_DATA_INPUT_CEILING} byte ceiling"
            )));
        }
        let effective_timeout = timeout_secs.max(1);
        let value = timeout(
            Duration::from_secs(effective_timeout),
            self.execute_tasks_action(&action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "tasks_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;
        let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
            ExecutionError::Step(format!("tasks_data result serialization failed: {error}"))
        })?;
        if rendered.len() as u64 > APP_BOUND_TASKS_DATA_RESULT_CEILING {
            return Err(ExecutionError::Step(format!(
                "tasks_data result exceeded the {APP_BOUND_TASKS_DATA_RESULT_CEILING} byte ceiling"
            )));
        }
        Ok(ActionResult::text(rendered))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|pack| pack.execution.as_ref())
            .and_then(|execution| execution.default_timeout_secs)
            .unwrap_or(30)
    }
}

// ---------------------------------------------------------------------------
// Closed argument proof
// ---------------------------------------------------------------------------

fn prove_app_tasks_args(parameters: &HashMap<String, Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    if !ACTIONS.contains(&operation) {
        return false;
    }

    for (key, value) in parameters {
        match key.as_str() {
            "__action_name" => {},
            "operation" | "action" | "method" => {
                let agrees = value.as_str().is_some_and(|alias| {
                    crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                        TASKS_DATA_TOOL_NAME,
                        alias,
                    )
                    .as_deref()
                        == Some(operation)
                });
                if !agrees {
                    return false;
                }
            },
            "principal" | "workspace" => {
                if !bounded_nonblank_string(value, MAX_ID_BYTES) {
                    return false;
                }
            },
            "limit" if operation == "list_tasks" => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=MAX_LIST_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "status" if operation == "list_tasks" => {
                if !value.as_str().is_some_and(admissible_status) {
                    return false;
                }
            },
            "after" if operation == "list_tasks" => {
                if !value
                    .as_str()
                    .is_some_and(|cursor| parse_cursor(cursor).is_some())
                {
                    return false;
                }
            },
            "task_id" if operation == "read_task" => {
                if !value.as_str().is_some_and(admissible_task_id) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }

    match operation {
        // An exact read has no defaultable target.
        "read_task" => parameters.contains_key("task_id"),
        _ => true,
    }
}

fn bounded_nonblank_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max_bytes)
}

/// Exactly the id space `ArtifactV2Workspace::validate_task_id` admits, plus
/// the byte bound: a listed task's id must be readable back by `read_task`.
fn admissible_task_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id.trim() == id
        && id != "."
        && id != ".."
        && !id.starts_with('/')
        && !id
            .chars()
            .any(|ch| ch == '/' || ch == '\\' || ch == ':' || ch.is_control())
}

/// Status filters are compared case-insensitively against the task's status
/// word, so only a short lowercase-able identifier is meaningful.
fn admissible_status(status: &str) -> bool {
    !status.is_empty()
        && status.len() <= MAX_STATUS_BYTES
        && status
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Keyset cursor: `<created_at>|<task_id>`. The listing is ordered newest
/// first with the id as tie-breaker, so the pair is a total-order position
/// that stays valid when the task it names is removed between pages.
fn parse_cursor(cursor: &str) -> Option<(&str, &str)> {
    if cursor.len() > MAX_CURSOR_BYTES || cursor.chars().any(char::is_control) {
        return None;
    }
    let (created_at, task_id) = cursor.split_once('|')?;
    // `created_at` is an RFC 3339 timestamp, which never contains `|`, so the
    // first separator splits unambiguously even for a task id containing one.
    if !admissible_timestamp(created_at) || !admissible_task_id(task_id) {
        return None;
    }
    Some((created_at, task_id))
}

fn admissible_timestamp(value: &str) -> bool {
    (10..=40).contains(&value.len())
        && value.as_bytes()[0].is_ascii_digit()
        && value.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'T' | b'Z' | b'.' | b'+')
        })
}

fn cursor_for(task: &TaskListItemV3) -> String {
    format!("{}|{}", task.created_at, task.id)
}

/// Newest first; the id breaks ties so the order is total.
fn listing_order(left: &TaskListItemV3, right: &TaskListItemV3) -> Ordering {
    right
        .created_at
        .cmp(&left.created_at)
        .then_with(|| left.id.cmp(&right.id))
}

/// Whether `task` sorts strictly after the cursor position in listing order.
fn after_cursor(task: &TaskListItemV3, created_at: &str, task_id: &str) -> bool {
    match created_at.cmp(task.created_at.as_str()) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => task.id.as_str() > task_id,
    }
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// What an app may see of one task. Everything else on the record — plans,
/// executions, chat links, question bodies, schedules, output routing — stays
/// behind this seam.
#[derive(Debug, Serialize)]
struct VisibleTask {
    task_id: String,
    title: String,
    status: String,
    agent_id: String,
    priority: Option<String>,
    due_date: Option<String>,
    tags: Vec<String>,
    created_at: String,
    updated_at: String,
    is_blocked: bool,
    /// Whether the task is waiting on the owner. The question itself is not
    /// projected: its body can quote conversation content.
    awaiting_owner: bool,
    completion_outcome: Option<String>,
    /// Only on `read_task`: the owner-authored description and the completion
    /// summary, each bounded.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_summary: Option<String>,
}

fn visible_task(task: &TaskListItemV3, detailed: bool) -> VisibleTask {
    VisibleTask {
        task_id: task.id.clone(),
        title: bounded_text(&task.title, MAX_TITLE_BYTES),
        status: bounded_text(&task.status, MAX_STATUS_BYTES),
        agent_id: bounded_text(&task.agent_id, MAX_ID_BYTES),
        priority: task
            .priority
            .as_deref()
            .map(|value| bounded_text(value, MAX_STATUS_BYTES)),
        due_date: task
            .due_date
            .as_deref()
            .map(|value| bounded_text(value, 64)),
        tags: task
            .tags
            .iter()
            .take(MAX_TAGS)
            .map(|tag| bounded_text(&tag.name, 64))
            .collect(),
        created_at: bounded_text(&task.created_at, 64),
        updated_at: bounded_text(&task.updated_at, 64),
        is_blocked: task.is_blocked,
        awaiting_owner: task.pending_question.is_some() || !task.pending_questions.is_empty(),
        completion_outcome: task
            .completion_outcome
            .as_deref()
            .map(|value| bounded_text(value, 64)),
        description: detailed.then(|| bounded_text(&task.description, MAX_TEXT_BYTES)),
        completion_summary: if detailed {
            task.completion_summary
                .as_deref()
                .map(|value| bounded_text(value, MAX_TEXT_BYTES))
        } else {
            None
        },
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl TasksDataProvider {
    async fn execute_tasks_action(
        &self,
        action: &str,
        params: &HashMap<String, Value>,
    ) -> Result<Value, ExecutionError> {
        match action {
            "list_tasks" => self.list_tasks(params).await,
            "read_task" => self.read_task(params).await,
            other => Err(ExecutionError::Step(format!(
                "tasks_data: unknown action `{other}`"
            ))),
        }
    }

    async fn scoped_tasks(&self, scope: &Scope) -> Result<Vec<TaskListItemV3>, ExecutionError> {
        let service = self.service.as_ref().ok_or_else(|| {
            ExecutionError::Step(
                "tasks_data has no task service in this registry; the per-scope app registry provides one"
                    .to_owned(),
            )
        })?;
        let scope_ref =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        V3ReadApi::list_tasks(service.as_ref(), &scope_ref)
            .await
            .map_err(|error| {
                ExecutionError::Step(format!(
                    "tasks_data reading the scope's tasks failed: {error}"
                ))
            })
    }

    async fn list_tasks(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let status = string_param(params, "status").map(|status| status.to_ascii_lowercase());
        let mut tasks = self
            .scoped_tasks(&scope)
            .await?
            .into_iter()
            .filter(|task| admissible_task_id(&task.id))
            .filter(|task| {
                status
                    .as_deref()
                    .is_none_or(|status| task.status.eq_ignore_ascii_case(status))
            })
            .collect::<Vec<_>>();
        tasks.sort_by(listing_order);
        let start = match string_param(params, "after")
            .as_deref()
            .and_then(parse_cursor)
        {
            Some((created_at, task_id)) => {
                tasks.partition_point(|task| !after_cursor(task, created_at, task_id))
            },
            None => 0,
        };
        let remaining = &tasks[start..];
        let limit = bounded_limit(params);
        let page = &remaining[..remaining.len().min(limit)];
        let next_cursor = (remaining.len() > limit)
            .then(|| page.last().map(cursor_for))
            .flatten();
        let rows = page
            .iter()
            .map(|task| visible_task(task, false))
            .collect::<Vec<_>>();

        Ok(json!({
            "scope": scope.as_json(),
            "tasks": rows,
            "next_cursor": next_cursor,
        }))
    }

    async fn read_task(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let task_id = string_param(params, "task_id").ok_or_else(|| {
            ExecutionError::Step("tasks_data: `task_id` must be a non-empty string".to_owned())
        })?;
        let task = self
            .scoped_tasks(&scope)
            .await?
            .into_iter()
            .find(|task| task.id == task_id);
        let present = task.is_some();
        Ok(json!({
            "scope": scope.as_json(),
            // Absent is an answer, not an error: an app holding a task id must
            // be able to learn the task is gone (or not visible to it).
            "task": task.as_ref().map(|task| visible_task(task, true)),
            "present": present,
        }))
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Truncate on a character boundary and mark the cut. Never slices bytes.
fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[derive(Debug, Clone)]
struct Scope {
    principal: String,
    workspace: String,
}

impl Scope {
    fn as_json(&self) -> Value {
        json!({"principal": self.principal, "workspace": self.workspace})
    }
}

fn scope_from_params(params: &HashMap<String, Value>) -> Result<Scope, ExecutionError> {
    Ok(Scope {
        principal: required_runtime_scope_value(params, "__principal")?,
        workspace: required_runtime_scope_value(params, "__workspace")?,
    })
}

fn authorize_runtime_scope(
    mut params: HashMap<String, Value>,
) -> Result<HashMap<String, Value>, ExecutionError> {
    let principal = required_runtime_scope_value(&params, "__principal")?;
    let workspace = required_runtime_scope_value(&params, "__workspace")?;
    if !LlmScope::new(&principal, &workspace).is_valid() {
        return Err(ExecutionError::Step(
            "tasks_data runtime scope contains an unsafe principal or workspace component"
                .to_owned(),
        ));
    }
    for (public_key, trusted_value) in [
        ("principal", principal.as_str()),
        ("workspace", workspace.as_str()),
    ] {
        if let Some(value) = params.get(public_key) {
            let Value::String(value) = value else {
                return Err(ExecutionError::Step(format!(
                    "tasks_data: `{public_key}` is an optional scope assertion and must be a string"
                )));
            };
            if !value.is_empty() && value != trusted_value {
                return Err(ExecutionError::Step(format!(
                    "tasks_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
                )));
            }
        }
    }
    params.insert("principal".to_owned(), Value::String(principal));
    params.insert("workspace".to_owned(), Value::String(workspace));
    Ok(params)
}

fn required_runtime_scope_value(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<String, ExecutionError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ExecutionError::Step(format!(
            "tasks_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "tasks_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_owned())
}

fn bounded_limit(params: &HashMap<String, Value>) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LIST_LIMIT as u64)
        .clamp(1, MAX_LIST_LIMIT) as usize
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Compile-time reminder that the tasks binder never grows a write verb.
/// Creating, editing, running and deleting tasks belong to the task owner.
const _: () = {
    let mut index = 0;
    while index < ACTIONS.len() {
        let action = ACTIONS[index].as_bytes();
        assert!(
            !starts_with(action, b"create")
                && !starts_with(action, b"update")
                && !starts_with(action, b"delete")
                && !starts_with(action, b"run")
                && !starts_with(action, b"stop")
                && !starts_with(action, b"set")
                && !starts_with(action, b"refine")
                && !starts_with(action, b"reassign"),
            "tasks_data is a read binder; task changes belong to the task owner"
        );
        index += 1;
    }
};

const fn starts_with(value: &[u8], prefix: &[u8]) -> bool {
    if value.len() < prefix.len() {
        return false;
    }
    let mut index = 0;
    while index < prefix.len() {
        if value[index] != prefix[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(action: &str) -> HashMap<String, Value> {
        HashMap::from([
            ("__action_name".to_owned(), json!(action)),
            ("__principal".to_owned(), json!("owner")),
            ("__workspace".to_owned(), json!("default")),
        ])
    }

    fn task(id: &str, created_at: &str) -> TaskListItemV3 {
        serde_json::from_value(json!({
            "id": id,
            "title": format!("Task {id}"),
            "description": "private notes ".repeat(10),
            "status": "running",
            "agent_id": "scribe",
            "created_at": created_at,
            "updated_at": created_at,
        }))
        .expect("minimal task list item")
    }

    #[test]
    fn the_action_surface_is_closed_and_an_exact_read_needs_its_target() {
        assert!(prove_app_tasks_args(&params("list_tasks")));
        assert!(!prove_app_tasks_args(&params("read_task")));
        let mut read = params("read_task");
        read.insert("task_id".to_owned(), json!("task-1"));
        assert!(prove_app_tasks_args(&read));
        for refused in [
            "create_task",
            "update_task",
            "delete_task",
            "run_task",
            "catalog",
        ] {
            assert!(
                !prove_app_tasks_args(&params(refused)),
                "`{refused}` is not a tasks_data read"
            );
        }
        let mut headless = params("list_tasks");
        headless.remove("__action_name");
        assert!(!prove_app_tasks_args(&headless));
    }

    #[test]
    fn arguments_are_bounded_and_scoped_to_their_action() {
        let mut listing = params("list_tasks");
        for limit in [json!(0), json!(MAX_LIST_LIMIT + 1), json!("10")] {
            listing.insert("limit".to_owned(), limit.clone());
            assert!(
                !prove_app_tasks_args(&listing),
                "limit {limit} must be refused"
            );
        }
        listing.insert("limit".to_owned(), json!(25));
        assert!(prove_app_tasks_args(&listing));

        for status in [
            "",
            "running tasks",
            "a".repeat(MAX_STATUS_BYTES + 1).as_str(),
            "../x",
        ] {
            listing.insert("status".to_owned(), json!(status));
            assert!(
                !prove_app_tasks_args(&listing),
                "status {status:?} must be refused"
            );
        }
        listing.insert("status".to_owned(), json!("completed"));
        assert!(prove_app_tasks_args(&listing));

        // `task_id` belongs to the exact read, not the listing.
        listing.insert("task_id".to_owned(), json!("task-1"));
        assert!(!prove_app_tasks_args(&listing));
        listing.remove("task_id");

        listing.insert("include_plan".to_owned(), json!(true));
        assert!(!prove_app_tasks_args(&listing), "unknown keys are refused");
    }

    #[test]
    fn task_ids_follow_the_workspace_id_rules() {
        for accepted in ["task-1", "01HZX", "a.b", "with space"] {
            assert!(admissible_task_id(accepted), "{accepted:?}");
        }
        for refused in [
            "", ".", "..", "a/b", "a\\b", "a:b", " lead", "a\u{7}b", "/abs",
        ] {
            assert!(!admissible_task_id(refused), "{refused:?}");
        }
        assert!(!admissible_task_id(&"a".repeat(MAX_ID_BYTES + 1)));
    }

    #[test]
    fn a_cursor_this_binder_emits_is_one_it_accepts_and_pages_do_not_overlap() {
        let mut tasks = vec![
            task("b", "2026-09-20T10:00:00Z"),
            task("a", "2026-09-20T10:00:00Z"),
            task("c", "2026-09-21T10:00:00Z"),
            task("d", "2026-09-19T10:00:00Z"),
        ];
        tasks.sort_by(listing_order);
        let order = tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(order, ["c", "a", "b", "d"], "newest first, id breaks ties");

        let cursor = cursor_for(&tasks[1]);
        let mut listing = params("list_tasks");
        listing.insert("after".to_owned(), json!(cursor));
        assert!(
            prove_app_tasks_args(&listing),
            "an emitted cursor must be admissible"
        );
        let (created_at, task_id) = parse_cursor(&cursor).expect("parses");
        let start = tasks.partition_point(|task| !after_cursor(task, created_at, task_id));
        let rest = tasks[start..]
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(rest, ["b", "d"]);

        // A task id may contain `|`; the timestamp half never does.
        assert_eq!(
            parse_cursor("2026-09-20T10:00:00Z|odd|id"),
            Some(("2026-09-20T10:00:00Z", "odd|id"))
        );
        for refused in [
            "no-separator",
            "|task",
            "2026-09-20T10:00:00Z|a/b",
            "a|b|c",
            "not-a-time|task",
        ] {
            listing.insert("after".to_owned(), json!(refused));
            assert!(!prove_app_tasks_args(&listing), "{refused:?}");
        }
    }

    #[test]
    fn the_listing_face_hides_the_description_and_the_exact_read_bounds_it() {
        let mut item = task("t", "2026-09-20T10:00:00Z");
        item.description = "x".repeat(MAX_TEXT_BYTES * 2);
        let listed = serde_json::to_value(visible_task(&item, false)).unwrap();
        assert!(listed.get("description").is_none());
        assert!(listed.get("completion_summary").is_none());
        let detailed = visible_task(&item, true);
        assert!(detailed.description.as_ref().unwrap().len() <= MAX_TEXT_BYTES);
        // Plans, executions and chat links are not fields of the projection.
        for hidden in [
            "latest_plan_id",
            "chat_session_id",
            "active_root_execution_id",
            "schedule",
        ] {
            assert!(
                listed.get(hidden).is_none(),
                "{hidden} must not be projected"
            );
        }
    }

    #[test]
    fn runtime_scope_is_required_and_a_public_assertion_cannot_switch_it() {
        let mut unscoped = params("list_tasks");
        unscoped.remove("__principal");
        assert!(authorize_runtime_scope(unscoped).is_err());
        let mut mismatched = params("list_tasks");
        mismatched.insert("principal".to_owned(), json!("someone-else"));
        assert!(authorize_runtime_scope(mismatched).is_err());
        assert!(authorize_runtime_scope(params("list_tasks")).is_ok());
    }

    #[tokio::test]
    async fn without_a_task_service_the_read_fails_rather_than_answering_empty() {
        let provider = TasksDataProvider::new();
        let result = provider
            .execute_tasks_action("list_tasks", &params("list_tasks"))
            .await;
        assert!(result.is_err(), "no service must not read as \"no tasks\"");
    }
}
