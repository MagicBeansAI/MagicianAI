//! One-time reconciler for orphaned agent-created tasks (RC #3).
//!
//! Before the dispatch-at-creation fix (`compiled_handlers/create_task.rs`), an agent
//! `create_task` for an unscheduled task orphaned in `ready` — both cron dispatch
//! loops skip a task with no cron expression, so nothing ever ran it. This module
//! drains that pre-existing backlog on demand: durably retire recurring clones, dispatch the
//! same-scope leaves (bounded), and leave plan-pending tasks for operator review.
//!
//! Operator-triggered through a dry-run-first API/CLI or
//! `MAGICIAN_RECONCILE_ORPHANED_TASKS_ON_BOOT` (a one-boot opt-in drain wired into
//! `run_agent_startup_hydration`). Idempotent: duplicate records are cancelled before
//! keeper dispatch, so a re-run cannot launch historical clones one by one.
//!
//! The planning logic ([`plan_reconcile`]) is pure and unit-tested; the async driver
//! ([`try_reconcile_orphaned_ready_tasks`]) adds list, durable retirement, and dispatch I/O.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;
use tracing::warn;

use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::artifact_v2::service::{ArtifactV2Service, ScopeRef};
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::compiled_handlers::create_task::{
    agentic_task_creation_gate, parse_auto_dispatch_depth, AGENTIC_COMPILED_CREATED_BY,
    AUTO_DISPATCH_MAX_DEPTH, NO_AUTO_DISPATCH_TAG,
};

/// Cap on tasks dispatched per scope per reconcile pass (keeps a boot-time drain
/// bounded — a huge orphan backlog is drained over successive re-runs, not all at once).
pub const DEFAULT_RECONCILE_BATCH_CAP: usize = 25;

/// Whether the boot-time one-time reconciler is armed. Opt-in only (default OFF) — the
/// operator sets this for the single restart that should drain the backlog.
pub fn reconcile_on_boot_enabled() -> bool {
    matches!(
        std::env::var("MAGICIAN_RECONCILE_ORPHANED_TASKS_ON_BOOT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Outcome counters for one scope's reconcile pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileOutcome {
    pub scanned: usize,
    pub dispatched: usize,
    pub dedup_planned: usize,
    pub deduped: usize,
    pub skipped_planned: usize,
    pub skipped_capped: usize,
    pub skipped_depth_capped: usize,
    pub skipped_dedup_error: usize,
    pub dedup_errors: usize,
    pub dispatch_errors: usize,
    pub deduped_task_ids: Vec<String>,
    pub dispatched_task_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconcileDedupAction {
    pub duplicate_task_id: String,
    pub keeper_task_id: String,
}

/// A deterministic plan: which orphan task ids to dispatch, plus classification counts.
/// Pure — no I/O — so it is unit-tested directly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReconcilePlan {
    pub scanned: usize,
    pub dispatch_ids: Vec<String>,
    pub dedup_actions: Vec<ReconcileDedupAction>,
    pub deduped: usize,
    pub skipped_planned: usize,
    pub skipped_capped: usize,
    pub skipped_depth_capped: usize,
}

/// Normalize a title for duplicate detection (matches `create_task`'s rule): trim,
/// collapse internal whitespace, case-fold.
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

/// An orphan candidate: an approved, unscheduled, agent/harness-created task stuck
/// `ready`. (User-created tasks are dispatched by their own flow; scheduled tasks are the
/// cron path's; internal/delegation tasks live in a separate store off this listing.)
fn is_reconcile_orphan(task: &TaskListItemV3) -> bool {
    task.status == "ready"
        && task.approved
        && task.schedule.is_none()
        && is_agent_or_harness_created(&task.created_by)
        // Explicit `run: "manual"` intent — deliberately left `ready`; never resurrect it.
        && !task
            .tags
            .iter()
            .any(|tag| tag.id == NO_AUTO_DISPATCH_TAG || tag.name == NO_AUTO_DISPATCH_TAG)
}

/// The orphan origins RC#3 targets: the agent `create_task` compiled handler
/// (`agentic_compiled`) and the meta-harness (`system:harness:<agent>`). A `!= "user"`
/// denylist was too broad — it also swept user-facing `ready` tasks such as
/// `feed_insight` follow-ups, `observe`, and `channel_assist`, which must not auto-run.
fn is_agent_or_harness_created(created_by: &str) -> bool {
    created_by == AGENTIC_COMPILED_CREATED_BY || created_by.starts_with("system:harness:")
}

/// A task carrying an unapproved plan awaiting human review — left for the operator
/// queue, never auto-dispatched.
fn is_plan_pending(task: &TaskListItemV3) -> bool {
    task.has_plan && task.approved_plan_id.is_none()
}

/// Pure planner: given a scope's task list + a dispatch cap, decide which orphans to
/// dispatch. Dedups by `(owning agent, canonical title)` against any already-live
/// instance AND within the orphan set (so recurring clones can't all fire), skips
/// plan-pending tasks, and caps the dispatch count.
pub fn plan_reconcile(tasks: &[TaskListItemV3], batch_cap: usize) -> ReconcilePlan {
    // (agent, canonical_title) already running/pending/planning/paused elsewhere — a
    // ready orphan with the same key is a stale clone and must not be re-dispatched.
    let mut claimed: HashMap<(String, String), String> = tasks
        .iter()
        .filter(|task| !is_terminal_status(&task.status) && task.status != "ready")
        .map(|task| {
            (
                (task.agent_id.clone(), canonical_title(&task.title)),
                task.id.clone(),
            )
        })
        .collect();

    let mut plan = ReconcilePlan::default();
    for task in tasks.iter().filter(|task| is_reconcile_orphan(task)) {
        plan.scanned += 1;
        // A task the dispatch-at-creation depth cap deliberately held (depth > cap) must
        // NOT be resurrected here — that would restart the fork-bomb backstop. Skip it
        // (it stays `ready` for the operator queue).
        if parse_auto_dispatch_depth(&task.tags) > AUTO_DISPATCH_MAX_DEPTH {
            plan.skipped_depth_capped += 1;
            continue;
        }
        if is_plan_pending(task) {
            plan.skipped_planned += 1;
            continue;
        }
        let key = (task.agent_id.clone(), canonical_title(&task.title));
        if let Some(keeper_task_id) = claimed.get(&key) {
            plan.deduped += 1;
            plan.dedup_actions.push(ReconcileDedupAction {
                duplicate_task_id: task.id.clone(),
                keeper_task_id: keeper_task_id.clone(),
            });
            continue;
        }
        if plan.dispatch_ids.len() >= batch_cap {
            plan.skipped_capped += 1;
            continue;
        }
        plan.dispatch_ids.push(task.id.clone());
        // Claim the key so remaining clones of this title in the same pass dedup.
        claimed.insert(key, task.id.clone());
    }
    plan
}

/// Return the exact bounded reconcile plan without mutating task state. Used by the
/// operator API/CLI dry-run path so applying a drain never requires a blind restart.
pub async fn inspect_orphaned_ready_tasks(
    service: &ArtifactV2Service,
    scope: &ScopeRef,
    batch_cap: usize,
) -> Result<ReconcilePlan, crate::magician_v2::artifact_v2::ArtifactV2Error> {
    let _creation_guard = agentic_task_creation_gate().lock().await;
    let tasks = V3ReadApi::list_tasks(service, scope).await?;
    Ok(plan_reconcile(&tasks, batch_cap))
}

/// Drain one scope: list tasks, retire duplicate records, then dispatch planned keepers
/// via the same `start_execution` a human uses. Gated by the global auto-dispatch kill
/// switch. Scope-list failures are returned; per-task mutation failures remain explicit
/// outcome counters so operators can retry safely.
pub async fn try_reconcile_orphaned_ready_tasks(
    service: Arc<ArtifactV2Service>,
    scope: ScopeRef,
    batch_cap: usize,
) -> Result<ReconcileOutcome, String> {
    let mut outcome = ReconcileOutcome::default();
    if !crate::config::auto_dispatch_enabled() {
        return Err("agent-created task auto-dispatch is disabled".to_string());
    }
    // Serialize list -> duplicate retirement -> dispatch with create_task's own
    // dedup/create critical section. This prevents a concurrent same-title create
    // from appearing between the reconcile snapshot and its durable mutations.
    let _creation_guard = agentic_task_creation_gate().lock().await;
    let tasks = match V3ReadApi::list_tasks(service.as_ref(), &scope).await {
        Ok(tasks) => tasks,
        Err(error) => {
            return Err(format!("could not list tasks for reconciliation: {error}"));
        },
    };
    let plan = plan_reconcile(&tasks, batch_cap);
    outcome.scanned = plan.scanned;
    outcome.dedup_planned = plan.deduped;
    outcome.skipped_planned = plan.skipped_planned;
    outcome.skipped_capped = plan.skipped_capped;
    outcome.skipped_depth_capped = plan.skipped_depth_capped;
    let mut blocked_keepers = HashSet::new();
    for action in plan.dedup_actions {
        match service
            .update_task_status(&scope, &action.duplicate_task_id, "cancelled")
            .await
        {
            Ok(_) => {
                outcome.deduped += 1;
                outcome.deduped_task_ids.push(action.duplicate_task_id);
            },
            Err(error) => {
                outcome.dedup_errors += 1;
                blocked_keepers.insert(action.keeper_task_id.clone());
                warn!(
                    error = %error,
                    duplicate_task_id = %action.duplicate_task_id,
                    keeper_task_id = %action.keeper_task_id,
                    "orphaned-task reconcile could not retire a duplicate; keeper will not dispatch"
                );
            },
        }
    }
    for task_id in plan.dispatch_ids {
        if blocked_keepers.contains(&task_id) {
            outcome.skipped_dedup_error += 1;
            continue;
        }
        match Arc::clone(&service)
            .start_execution(scope.clone(), task_id.clone(), None, false)
            .await
        {
            Ok(_) => {
                outcome.dispatched += 1;
                outcome.dispatched_task_ids.push(task_id);
            },
            Err(error) => {
                outcome.dispatch_errors += 1;
                warn!(
                    error = %error,
                    task_id = %task_id,
                    "orphaned-task reconcile failed to dispatch a task (left ready)"
                );
            },
        }
    }
    Ok(outcome)
}

/// Best-effort startup wrapper. Operator API/CLI callers use the checked variant above;
/// hydration keeps its existing fail-soft contract and reports the error in service logs.
pub async fn reconcile_orphaned_ready_tasks(
    service: Arc<ArtifactV2Service>,
    scope: ScopeRef,
    batch_cap: usize,
) -> ReconcileOutcome {
    match try_reconcile_orphaned_ready_tasks(service, scope.clone(), batch_cap).await {
        Ok(outcome) => outcome,
        Err(error) => {
            warn!(
                error,
                principal = %scope.principal(),
                workspace = %scope.workspace(),
                "orphaned-task reconcile skipped"
            );
            ReconcileOutcome::default()
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::{TaskLifecycle, TaskOutputMode, TaskSyncMode};

    fn base_task(id: &str) -> TaskListItemV3 {
        TaskListItemV3 {
            id: id.to_string(),
            title: "Weekly GTM Review".to_string(),
            description: String::new(),
            status: "ready".to_string(),
            agent_id: "cro".to_string(),
            ui_thread_id: "general".to_string(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "agentic_compiled".to_string(),
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
            lifecycle: TaskLifecycle::Persistent,
            sync_mode: TaskSyncMode::default(),
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
    fn dispatches_same_scope_leaf_orphans() {
        let plan = plan_reconcile(&[base_task("t1")], 25);
        assert_eq!(plan.scanned, 1);
        assert_eq!(plan.dispatch_ids, vec!["t1".to_string()]);
    }

    #[test]
    fn dedups_clones_of_same_agent_and_title() {
        // three identical ready clones (same agent + title) → dispatch one, dedup two
        let tasks = vec![base_task("t1"), base_task("t2"), base_task("t3")];
        let plan = plan_reconcile(&tasks, 25);
        assert_eq!(plan.scanned, 3);
        assert_eq!(plan.dispatch_ids.len(), 1);
        assert_eq!(plan.deduped, 2);
        assert_eq!(plan.dedup_actions.len(), 2);
        assert!(plan
            .dedup_actions
            .iter()
            .all(|action| action.keeper_task_id == "t1"));
    }

    #[test]
    fn durable_duplicate_retirement_makes_a_later_pass_a_no_op() {
        let mut tasks = vec![base_task("t1"), base_task("t2"), base_task("t3")];
        let first = plan_reconcile(&tasks, 25);
        assert_eq!(first.dispatch_ids, vec!["t1".to_string()]);
        assert_eq!(first.dedup_actions.len(), 2);

        // Model the driver's durable effects: the keeper ran to completion and every
        // duplicate selected by the plan was cancelled. A later armed restart must not
        // dispatch the historical clones one by one.
        tasks[0].status = "completed".to_string();
        for action in first.dedup_actions {
            let duplicate = tasks
                .iter_mut()
                .find(|task| task.id == action.duplicate_task_id)
                .expect("planned duplicate exists");
            duplicate.status = "cancelled".to_string();
        }

        let second = plan_reconcile(&tasks, 25);
        assert_eq!(second.scanned, 0);
        assert!(second.dispatch_ids.is_empty());
        assert!(second.dedup_actions.is_empty());
    }

    #[test]
    fn skips_orphan_when_a_live_instance_already_runs() {
        let mut running = base_task("running");
        running.status = "running".to_string();
        let orphan = base_task("ready-clone"); // same agent + title as `running`
        let plan = plan_reconcile(&[running, orphan], 25);
        assert_eq!(plan.scanned, 1); // only the ready one is scanned
        assert!(plan.dispatch_ids.is_empty());
        assert_eq!(plan.deduped, 1);
    }

    #[test]
    fn skips_plan_pending_orphan() {
        let mut planned = base_task("t1");
        planned.has_plan = true;
        planned.approved_plan_id = None;
        let plan = plan_reconcile(&[planned], 25);
        assert!(plan.dispatch_ids.is_empty());
        assert_eq!(plan.skipped_planned, 1);
    }

    #[test]
    fn plan_pending_task_is_not_retired_as_a_same_title_duplicate() {
        let mut running = base_task("running");
        running.status = "running".to_string();
        let mut planned = base_task("planned");
        planned.has_plan = true;
        planned.approved_plan_id = None;

        let plan = plan_reconcile(&[running, planned], 25);
        assert_eq!(plan.skipped_planned, 1);
        assert!(plan.dedup_actions.is_empty());
    }

    #[test]
    fn approved_plan_orphan_still_dispatches() {
        let mut planned = base_task("t1");
        planned.has_plan = true;
        planned.approved_plan_id = Some("plan-1".to_string());
        let plan = plan_reconcile(&[planned], 25);
        assert_eq!(plan.dispatch_ids, vec!["t1".to_string()]);
    }

    #[test]
    fn respects_batch_cap() {
        // distinct titles so dedup does not collapse them
        let tasks: Vec<TaskListItemV3> = (0..5)
            .map(|i| {
                let mut task = base_task(&format!("t{i}"));
                task.title = format!("Distinct Title {i}");
                task
            })
            .collect();
        let plan = plan_reconcile(&tasks, 2);
        assert_eq!(plan.scanned, 5);
        assert_eq!(plan.dispatch_ids.len(), 2);
        assert_eq!(plan.skipped_capped, 3);
    }

    #[test]
    fn ignores_user_created_scheduled_and_terminal() {
        let mut user = base_task("user");
        user.created_by = "user".to_string();
        let mut scheduled = base_task("scheduled");
        scheduled.schedule = Some(serde_json::json!({"cron": "0 9 * * 1"}));
        let mut done = base_task("done");
        done.status = "completed".to_string();
        let mut unapproved = base_task("unapproved");
        unapproved.approved = false;

        let plan = plan_reconcile(&[user, scheduled, done, unapproved], 25);
        assert_eq!(plan.scanned, 0);
        assert!(plan.dispatch_ids.is_empty());
    }

    #[test]
    fn skips_depth_capped_orphans() {
        let mut deep = base_task("deep");
        deep.title = "Deep Task".to_string();
        let over = format!("auto-dispatch-depth:{}", AUTO_DISPATCH_MAX_DEPTH + 1);
        deep.tags = vec![crate::magician_v2::artifact_v2::models::TaskTagRecord {
            id: over.clone(),
            name: over,
            color: None,
        }];
        let plan = plan_reconcile(&[deep], 25);
        assert_eq!(plan.scanned, 1);
        assert!(plan.dispatch_ids.is_empty());
        assert_eq!(plan.skipped_depth_capped, 1);
    }

    #[test]
    fn whitelists_agentic_and_harness_origins_only() {
        let mk = |id: &str, created_by: &str| {
            let mut task = base_task(id);
            task.title = format!("Task {id}");
            task.created_by = created_by.to_string();
            task
        };
        let tasks = vec![
            mk("a", "agentic_compiled"),
            mk("h", "system:harness:cro"),
            mk("f", "feed_insight"), // user-facing origin — must NOT be swept
            mk("u", "user"),
        ];
        let plan = plan_reconcile(&tasks, 25);
        assert_eq!(plan.scanned, 2);
        assert_eq!(plan.dispatch_ids.len(), 2);
        assert!(plan.dispatch_ids.contains(&"a".to_string()));
        assert!(plan.dispatch_ids.contains(&"h".to_string()));
    }

    #[test]
    fn skips_explicit_manual_tasks() {
        let mut manual = base_task("manual");
        manual.title = "Manual Task".to_string();
        manual.tags = vec![crate::magician_v2::artifact_v2::models::TaskTagRecord {
            id: NO_AUTO_DISPATCH_TAG.to_string(),
            name: NO_AUTO_DISPATCH_TAG.to_string(),
            color: None,
        }];
        let plan = plan_reconcile(&[manual], 25);
        assert_eq!(plan.scanned, 0); // excluded as an orphan candidate — not resurrected
        assert!(plan.dispatch_ids.is_empty());
    }
}
