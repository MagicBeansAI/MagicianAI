//! `create_task` — universal agentic core: validate args, create the
//! task via `ArtifactV2Service`, return identifiers. No chat-specific
//! side effects (status cards, fan-out subscriptions); chat surfaces
//! the task by following up with `subscribe_to_task_for_chat`.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::artifact_v2::models::{TaskListItemV3, TaskOutputMode, TaskTagRecord};
use crate::magician_v2::artifact_v2::service::{CreateTaskInput, ScopeRef};
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::chat::service::{
    create_task_description_from_args, parse_create_task_reference_ids,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::{require_scope_str, scope_arg_str};

pub const AGENTIC_COMPILED_CREATED_BY: &str = "agentic_compiled";
const SOURCE_EXECUTION_TAG_PREFIX: &str = "agentic-source-execution:";
const SOURCE_TASK_TAG_PREFIX: &str = "agentic-source-task:";

static AGENTIC_TASK_CREATION_GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub fn agentic_task_creation_gate() -> &'static tokio::sync::Mutex<()> {
    AGENTIC_TASK_CREATION_GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Provenance tag carrying the auto-dispatch generation depth of a task, so an
/// auto-dispatched run that itself spawns tasks can't fan a fork-bomb: each
/// generation increments the depth, and past [`AUTO_DISPATCH_MAX_DEPTH`] the child
/// is created `ready` (never auto-dispatched) and surfaced to the operator queue.
const AUTO_DISPATCH_DEPTH_TAG_PREFIX: &str = "auto-dispatch-depth:";
pub const AUTO_DISPATCH_MAX_DEPTH: u32 = 3;
/// Marker tag recorded when the caller explicitly passes `run: "manual"` — the created
/// task is deliberately left `ready` and must NOT be auto-dispatched, including by the
/// one-time orphan reconciler (which cannot otherwise distinguish it from a pre-fix orphan).
pub const NO_AUTO_DISPATCH_TAG: &str = "no-auto-dispatch";

/// What to do with a freshly-created `create_task` task. Tasks created here are
/// owner-locked to the calling agent (always same-scope, same-owner, flat), so the
/// trust-tier reduces to run-now vs. leave-ready — see the design doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunDisposition {
    /// Dispatch immediately via `start_execution` (same-scope leaf, guards passed).
    Now,
    /// Leave the task `ready` for the cron scheduler / operator / reconciler.
    Manual,
}

#[derive(Debug, PartialEq, Eq)]
enum DispatchAttempt {
    NotRequested,
    Dispatched(String),
    Failed(String),
}

async fn apply_run_disposition<F, Fut, E>(disposition: RunDisposition, start: F) -> DispatchAttempt
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, E>>,
    E: std::fmt::Display,
{
    match disposition {
        RunDisposition::Manual => DispatchAttempt::NotRequested,
        RunDisposition::Now => match start().await {
            Ok(execution_id) => DispatchAttempt::Dispatched(execution_id),
            Err(error) => DispatchAttempt::Failed(error.to_string()),
        },
    }
}

/// Highest `auto-dispatch-depth:N` across a task's tags (0 when absent — a
/// user/chat/scheduler-origin parent).
pub fn parse_auto_dispatch_depth(tags: &[TaskTagRecord]) -> u32 {
    tags.iter()
        .filter_map(|tag| {
            tag.id
                .strip_prefix(AUTO_DISPATCH_DEPTH_TAG_PREFIX)
                .or_else(|| tag.name.strip_prefix(AUTO_DISPATCH_DEPTH_TAG_PREFIX))
        })
        .filter_map(|value| value.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

fn depth_tag(depth: u32) -> TaskTagRecord {
    let id = format!("{AUTO_DISPATCH_DEPTH_TAG_PREFIX}{depth}");
    TaskTagRecord {
        id: id.clone(),
        name: id,
        color: None,
    }
}

fn no_auto_dispatch_tag() -> TaskTagRecord {
    TaskTagRecord {
        id: NO_AUTO_DISPATCH_TAG.to_string(),
        name: NO_AUTO_DISPATCH_TAG.to_string(),
        color: None,
    }
}

/// Trust-tier disposition for a same-scope leaf `create_task`. A scheduled task,
/// an explicit `run: "manual"`, the kill switch being off, or a depth over the cap
/// all force `Manual`; otherwise the task auto-dispatches.
fn decide_run_disposition(
    has_schedule: bool,
    run_arg: Option<&str>,
    kill_switch_on: bool,
    child_depth: u32,
) -> RunDisposition {
    if has_schedule {
        return RunDisposition::Manual;
    }
    if matches!(run_arg.map(str::trim), Some("manual")) {
        return RunDisposition::Manual;
    }
    if !kill_switch_on {
        return RunDisposition::Manual;
    }
    if child_depth > AUTO_DISPATCH_MAX_DEPTH {
        return RunDisposition::Manual;
    }
    RunDisposition::Now
}

/// Normalize a title for duplicate detection: trim, collapse internal whitespace,
/// case-fold.
fn canonical_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "canceled")
}

/// Find a live (non-terminal) task owned by the same agent with the same canonical
/// title — the recurring-cycle clone that idempotent create must dedup against.
fn find_live_duplicate(
    tasks: &[TaskListItemV3],
    canonical: &str,
    owning_agent_id: &str,
) -> Option<(String, String)> {
    tasks
        .iter()
        .find(|task| {
            task.agent_id == owning_agent_id
                && !is_terminal_status(&task.status)
                && canonical_title(&task.title) == canonical
        })
        .map(|task| (task.id.clone(), task.title.clone()))
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "create_task")?;
    let workspace = require_scope_str(&args, "__workspace", "create_task")?;
    let owning_agent_id = require_scope_str(&args, "__agent_id", "create_task")?;
    let source_task_id = scope_arg_str(&args, "__task_id");
    let source_execution_id = scope_arg_str(&args, "__execution_id");
    let max_spawned_tasks = match hidden_u32_arg(&args, "__max_spawned_tasks") {
        Ok(value) => value,
        Err(reason) => {
            return Ok(json!({
                "status": "error",
                "reason": reason,
            }));
        },
    };
    let ui_thread_id = scope_arg_str(&args, "__ui_thread_id")
        .unwrap_or_else(|| crate::magician_v2::storage::task_models::default_ui_thread_id());
    let chat_session_id = scope_arg_str(&args, "__chat_session_id");

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(json!({
            "status": "error",
            "reason": "ArtifactV2Service is not configured in AgentResources",
        }));
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
    let run_arg = args
        .get("run")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string());
    let allow_duplicate = args
        .get("allow_duplicate")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let Some(title) = args.get("title").and_then(Value::as_str).map(str::trim) else {
        return Ok(json!({
            "status": "error",
            "reason": "create_task requires a non-empty `title`",
        }));
    };
    if title.is_empty() {
        return Ok(json!({
            "status": "error",
            "reason": "create_task requires a non-empty `title`",
        }));
    }
    let description = create_task_description_from_args(&args, title);
    // Task ownership is locked to the calling agent. If the LLM
    // passed a different `agent_id`, log + ignore.
    if let Some(requested) = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != owning_agent_id)
    {
        warn!(
            requested_agent = %requested,
            owning_agent = %owning_agent_id,
            title,
            "[CREATE_TASK] ignored requested agent_id; tasks are owned by the calling agent. Use delegate_to_agent to hand off to a specialist."
        );
    }
    let schedule = match args.get("schedule").filter(|value| !value.is_null()) {
        Some(value) => {
            if let Err(error) = serde_json::from_value::<
                crate::magician_v2::storage::task_models::TaskSchedule,
            >(value.clone())
            {
                return Ok(json!({
                    "status": "error",
                    "reason": format!("create_task schedule is not a valid TaskSchedule: {error}"),
                }));
            }
            Some(value.clone())
        },
        None => None,
    };
    let reference_task_ids = match parse_create_task_reference_ids(&args) {
        Ok(ids) => ids,
        Err(reason) => {
            return Ok(json!({
                "status": "error",
                "reason": reason,
            }));
        },
    };
    if !reference_task_ids.is_empty() {
        for reference_task_id in &reference_task_ids {
            match V3ReadApi::get_task(service.as_ref(), &scope, reference_task_id).await {
                Ok(task) if task.state.status == "completed" => {},
                Ok(task) => {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!(
                            "create_task reference_task_ids must refer to completed tasks; {reference_task_id} is currently {}",
                            task.state.status
                        ),
                        "hint": "Link only completed task ids, or wait for the source task to finish before creating continuation work.",
                    }));
                },
                Err(error) => {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!(
                            "create_task reference_task_ids contains a task that is not available in this scope: {reference_task_id}: {error}"
                        ),
                        "hint": "Use list_tasks to find candidate task ids before passing reference_task_ids to create_task.",
                    }));
                },
            }
        }
    }
    // Auto-dispatch kill switch — read ONCE and threaded through dedup, depth, the depth
    // tag, and the disposition so they stay consistent. OFF ⇒ exact pre-fix behavior
    // (no dedup, no depth tag, task stays `ready`).
    let auto_dispatch_on = crate::config::auto_dispatch_enabled();
    // Idempotent-dedup is active — and races a concurrent same-title create — only when
    // auto-dispatch is on, the task is unscheduled, and duplicates aren't explicitly
    // allowed. In that case hold the creation gate across dedup+create too (not just when
    // a per-cycle spawn cap is set), so the dedup list→create window can't let two
    // concurrent same-title creates both pass.
    let dedup_active = auto_dispatch_on && schedule.is_none() && !allow_duplicate;

    // Serialize the count/durable write with every capped task producer, and the dedup
    // list→create window. Artifact task creation has no atomic conditional-create.
    let _creation_guard = if max_spawned_tasks.is_some() || dedup_active {
        Some(agentic_task_creation_gate().lock().await)
    } else {
        None
    };
    if let Some(max_spawned_tasks) = max_spawned_tasks {
        let Some(source_tag) =
            source_spawn_cap_tag(source_task_id.as_deref(), source_execution_id.as_deref())
        else {
            return Ok(json!({
                "status": "error",
                "reason": "create_task max_spawned_tasks is configured but execution/task provenance is missing; cannot enforce the per-cycle task cap",
            }));
        };
        let spawned_task_count = match V3ReadApi::list_tasks(service.as_ref(), &scope).await {
            Ok(tasks) => count_spawned_tasks_for_source(&tasks, &source_tag),
            Err(error) => {
                return Ok(json!({
                    "status": "error",
                    "reason": format!(
                        "create_task could not enforce max_spawned_tasks because task listing failed: {error}"
                    ),
                }));
            },
        };
        if spawned_task_count >= usize::try_from(max_spawned_tasks).unwrap_or(usize::MAX) {
            return Ok(json!({
                "status": "error",
                "reason": format!(
                    "create_task task spawn limit reached: {spawned_task_count}/{max_spawned_tasks} tasks already created for this execution"
                ),
                "max_spawned_tasks": max_spawned_tasks,
                "spawned_task_count": spawned_task_count,
            }));
        }
    }
    // Idempotent create + auto-dispatch depth: one best-effort scope listing feeds
    // both. Skipped for scheduled tasks (the cron path owns them). Fail-open — a
    // transient listing error must not block a legitimate create.
    let mut child_depth: u32 = 1;
    if auto_dispatch_on && schedule.is_none() {
        match V3ReadApi::list_tasks(service.as_ref(), &scope).await {
            Ok(tasks) => {
                if !allow_duplicate {
                    if let Some((existing_id, existing_title)) =
                        find_live_duplicate(&tasks, &canonical_title(title), &owning_agent_id)
                    {
                        return Ok(json!({
                            "status": "deduplicated",
                            "task_id": existing_id,
                            "title": existing_title,
                            "reason": "a live task with the same title already exists for this agent; returned the existing task instead of creating a duplicate",
                        }));
                    }
                }
                if let Some(source_id) = source_task_id.as_deref() {
                    // Best-effort depth: read the parent's depth from the listing when it is
                    // visible. When the parent ISN'T in `list_tasks` (e.g. an Internal-lifecycle
                    // chat parent), leave `child_depth` at 1 rather than failing closed to
                    // Manual — over-suppressing legit chat/Internal-parented auto-dispatch
                    // (holding it `ready`) is the worse trade, and reset-to-1 only loses ONE
                    // generation of headroom: every auto-dispatched task is created
                    // `lifecycle: Persistent` (user-visible → in `list_tasks`), so the NEXT
                    // generation reads its `auto-dispatch-depth` tag and increments normally.
                    // The depth cap remains the real backstop across a DELEGATION chain — a
                    // delegated child runs on the parent's task_id with `max_spawned_tasks:
                    // None` (unbounded), so the depth cap, not a spawn cap, bounds it there;
                    // the DIRECT self-spawn path is separately hard-capped at
                    // `max_spawned_tasks: 0`.
                    if let Some(source_task) = tasks.iter().find(|task| task.id == source_id) {
                        child_depth =
                            parse_auto_dispatch_depth(&source_task.tags).saturating_add(1);
                    }
                }
            },
            Err(error) => {
                warn!(
                    error = %error,
                    title,
                    "[CREATE_TASK] dedup/depth listing failed; proceeding without dedup (fail-open)"
                );
            },
        }
    }

    let mut tags =
        create_task_provenance_tags(source_task_id.as_deref(), source_execution_id.as_deref());
    if auto_dispatch_on {
        tags.push(depth_tag(child_depth));
    }
    // Record explicit `run: "manual"` intent durably (independent of the kill switch) so
    // the one-time reconciler leaves it `ready` instead of resurrecting it.
    if matches!(run_arg.as_deref(), Some("manual")) {
        tags.push(no_auto_dispatch_tag());
    }
    let input = CreateTaskInput {
        principal: principal.clone(),
        workspace: workspace.clone(),
        title: title.to_string(),
        description,
        agent_id: owning_agent_id.clone(),
        goal_id: None,
        ui_thread_id: ui_thread_id.clone(),
        priority: None,
        due_date: None,
        tags,
        created_by: AGENTIC_COMPILED_CREATED_BY.to_string(),
        depends_on: reference_task_ids.clone(),
        approved: true,
        schedule: schedule.clone(),
        output_mode: TaskOutputMode::Accumulate,
        chat_session_id: chat_session_id.clone(),
        lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
        sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
    };
    let created = match service.create_task(input).await {
        Ok(task) => task,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("create_task failed: {error}"),
            }));
        },
    };

    // Trust-tier: a same-scope leaf auto-dispatches immediately (the RC #3 fix);
    // otherwise it stays `ready`. `start_execution` mirrors `run_task` and crosses
    // the runtime boundary internally, so it is safe to call from here. A dispatch
    // failure is non-fatal — the task remains `ready` for the operator/reconciler.
    let disposition = decide_run_disposition(
        schedule.is_some(),
        run_arg.as_deref(),
        auto_dispatch_on,
        child_depth,
    );
    let mut response = json!({
        "status": "created",
        "task_id": created.manifest.task_id,
        "title": created.manifest.title,
        "agent_id": created.manifest.agent_id,
        "ui_thread_id": created.manifest.ui_thread_id,
        "reference_task_ids": reference_task_ids,
    });
    let dispatch_attempt = apply_run_disposition(disposition, || async {
        Arc::clone(service)
            .start_execution(scope.clone(), created.manifest.task_id.clone(), None, false)
            .await
            .map(|(_task, execution)| execution.state.execution_id)
    })
    .await;
    match dispatch_attempt {
        DispatchAttempt::Dispatched(execution_id) => {
            response["disposition"] = json!("dispatched");
            response["execution_id"] = json!(execution_id);
        },
        DispatchAttempt::Failed(error) => {
            response["disposition"] = json!("dispatch_error");
            response["reason"] = json!(format!("auto-dispatch failed: {error}"));
        },
        DispatchAttempt::NotRequested => {
            response["disposition"] = json!("ready");
        },
    }
    Ok(response)
}

fn hidden_u32_arg(args: &Value, key: &str) -> Result<Option<u32>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let parsed = value.as_u64().and_then(|value| u32::try_from(value).ok());
    parsed.map(Some).ok_or_else(|| {
        format!("create_task received an invalid runtime task cap in `{key}`; refusing to create a task")
    })
}

fn create_task_provenance_tags(
    source_task_id: Option<&str>,
    source_execution_id: Option<&str>,
) -> Vec<TaskTagRecord> {
    let mut tags = Vec::new();
    if let Some(tag) = task_tag(SOURCE_EXECUTION_TAG_PREFIX, source_execution_id) {
        tags.push(tag);
    }
    if let Some(tag) = task_tag(SOURCE_TASK_TAG_PREFIX, source_task_id) {
        tags.push(tag);
    }
    tags
}

fn source_spawn_cap_tag(
    source_task_id: Option<&str>,
    source_execution_id: Option<&str>,
) -> Option<String> {
    source_execution_id
        .and_then(|id| tag_value(SOURCE_EXECUTION_TAG_PREFIX, id))
        .or_else(|| source_task_id.and_then(|id| tag_value(SOURCE_TASK_TAG_PREFIX, id)))
}

fn task_tag(prefix: &str, value: Option<&str>) -> Option<TaskTagRecord> {
    let id = value.and_then(|value| tag_value(prefix, value))?;
    Some(TaskTagRecord {
        id: id.clone(),
        name: id,
        color: None,
    })
}

fn tag_value(prefix: &str, value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| format!("{prefix}{trimmed}"))
}

fn count_spawned_tasks_for_source(tasks: &[TaskListItemV3], source_tag: &str) -> usize {
    tasks
        .iter()
        .filter(|task| task.tags.iter().any(|tag| tag_matches(tag, source_tag)))
        .count()
}

fn tag_matches(tag: &TaskTagRecord, expected: &str) -> bool {
    tag.id == expected || tag.name == expected
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn item(id: &str, created_by: &str, tags: Vec<TaskTagRecord>) -> TaskListItemV3 {
        TaskListItemV3 {
            id: id.to_string(),
            title: "task".to_string(),
            description: String::new(),
            status: "pending".to_string(),
            agent_id: "agent".to_string(),
            ui_thread_id: "general".to_string(),
            priority: None,
            due_date: None,
            tags,
            created_by: created_by.to_string(),
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
            lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
            sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            synthesis_pending: false,
            synthesis_failed_execution_id: None,
            monitor_revision: 0,
            awaiting_diff_approval: false,
            last_progress_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn source_spawn_cap_prefers_execution_and_counts_all_cycle_tasks() {
        let tags = create_task_provenance_tags(Some("task-1"), Some("exec-1"));
        assert_eq!(
            source_spawn_cap_tag(Some("task-1"), Some("exec-1")).as_deref(),
            Some("agentic-source-execution:exec-1")
        );
        assert_eq!(tags.len(), 2);

        let tasks = vec![
            item("a", AGENTIC_COMPILED_CREATED_BY, tags),
            item(
                "b",
                "user",
                vec![TaskTagRecord {
                    id: "agentic-source-execution:exec-1".to_string(),
                    name: "agentic-source-execution:exec-1".to_string(),
                    color: None,
                }],
            ),
            item(
                "c",
                AGENTIC_COMPILED_CREATED_BY,
                vec![TaskTagRecord {
                    id: "agentic-source-execution:other".to_string(),
                    name: "agentic-source-execution:other".to_string(),
                    color: None,
                }],
            ),
        ];

        assert_eq!(
            count_spawned_tasks_for_source(&tasks, "agentic-source-execution:exec-1"),
            2
        );
    }

    #[test]
    fn source_spawn_cap_falls_back_to_task_id() {
        assert_eq!(
            source_spawn_cap_tag(Some("task-1"), None).as_deref(),
            Some("agentic-source-task:task-1")
        );
    }

    #[test]
    fn malformed_runtime_task_cap_fails_closed() {
        assert!(
            hidden_u32_arg(&json!({"__max_spawned_tasks": -1}), "__max_spawned_tasks").is_err()
        );
        assert!(hidden_u32_arg(
            &json!({"__max_spawned_tasks": u64::from(u32::MAX) + 1}),
            "__max_spawned_tasks"
        )
        .is_err());
        assert_eq!(
            hidden_u32_arg(&json!({"__max_spawned_tasks": 3}), "__max_spawned_tasks")
                .expect("valid cap"),
            Some(3)
        );
    }

    fn tag(value: &str) -> TaskTagRecord {
        TaskTagRecord {
            id: value.to_string(),
            name: value.to_string(),
            color: None,
        }
    }

    #[test]
    fn parse_depth_picks_max_and_defaults_zero() {
        assert_eq!(parse_auto_dispatch_depth(&[]), 0);
        assert_eq!(
            parse_auto_dispatch_depth(&[
                tag("auto-dispatch-depth:1"),
                tag("auto-dispatch-depth:3"),
                tag("agentic-source-execution:exec-1"),
            ]),
            3
        );
        assert_eq!(
            parse_auto_dispatch_depth(&[tag("auto-dispatch-depth:notanumber")]),
            0
        );
    }

    #[test]
    fn depth_tag_roundtrips_through_parse() {
        assert_eq!(parse_auto_dispatch_depth(&[depth_tag(4)]), 4);
    }

    #[test]
    fn disposition_now_only_for_unscheduled_enabled_within_depth() {
        assert_eq!(
            decide_run_disposition(false, None, true, 1),
            RunDisposition::Now
        );
        assert_eq!(
            decide_run_disposition(false, None, true, AUTO_DISPATCH_MAX_DEPTH),
            RunDisposition::Now
        );
        // scheduled → the cron path owns dispatch
        assert_eq!(
            decide_run_disposition(true, None, true, 1),
            RunDisposition::Manual
        );
        // explicit run: manual
        assert_eq!(
            decide_run_disposition(false, Some("manual"), true, 1),
            RunDisposition::Manual
        );
        // kill switch off
        assert_eq!(
            decide_run_disposition(false, None, false, 1),
            RunDisposition::Manual
        );
        // over the depth cap (fork-bomb backstop)
        assert_eq!(
            decide_run_disposition(false, None, true, AUTO_DISPATCH_MAX_DEPTH + 1),
            RunDisposition::Manual
        );
    }

    #[tokio::test]
    async fn run_now_invokes_start_seam_and_returns_execution_id() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_for_start = Arc::clone(&called);
        let outcome = apply_run_disposition(RunDisposition::Now, move || async move {
            called_for_start.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, &str>("exec-1".to_string())
        })
        .await;

        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(outcome, DispatchAttempt::Dispatched("exec-1".to_string()));
    }

    #[tokio::test]
    async fn manual_disposition_does_not_invoke_start_seam() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_for_start = Arc::clone(&called);
        let outcome = apply_run_disposition(RunDisposition::Manual, move || async move {
            called_for_start.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, &str>("unexpected".to_string())
        })
        .await;

        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(outcome, DispatchAttempt::NotRequested);
    }

    #[tokio::test]
    async fn dispatch_failure_remains_recoverable() {
        let outcome = apply_run_disposition(RunDisposition::Now, || async {
            Err::<String, _>("admission unavailable")
        })
        .await;

        assert_eq!(
            outcome,
            DispatchAttempt::Failed("admission unavailable".to_string())
        );
    }

    #[test]
    fn canonical_title_normalizes_case_and_whitespace() {
        assert_eq!(
            canonical_title("  Weekly   GTM  Review "),
            "weekly gtm review"
        );
    }

    #[test]
    fn find_live_duplicate_matches_same_agent_live_title() {
        let canonical = canonical_title("weekly gtm review");
        let make = |id: &str, agent: &str, status: &str| {
            let mut task = item(id, "user", vec![]);
            task.title = "Weekly GTM Review".to_string();
            task.agent_id = agent.to_string();
            task.status = status.to_string();
            task
        };

        let tasks = vec![
            make("t-done", "cro", "completed"),
            make("t-other", "cmo", "ready"),
            make("t-live", "cro", "ready"),
        ];
        // a live same-agent same-title task is the duplicate
        assert_eq!(
            find_live_duplicate(&tasks, &canonical, "cro"),
            Some(("t-live".to_string(), "Weekly GTM Review".to_string()))
        );
        // a completed same-title task does NOT block a new create
        assert!(
            find_live_duplicate(&[make("t-done", "cro", "completed")], &canonical, "cro").is_none()
        );
        // a different agent's same-title task is not a duplicate
        assert!(
            find_live_duplicate(&[make("t-other", "cmo", "ready")], &canonical, "cro").is_none()
        );
    }
}
