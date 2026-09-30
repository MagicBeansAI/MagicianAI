//! Task/episode corpus source: the scope's terminal tasks and their outcomes.
//!
//! Reads the scope's task records through [`V3ReadApi::list_tasks`] on
//! [`ArtifactV2Service`] and yields one [`CorpusItem`] per *terminal* task
//! newer than the watermark — the "something we did earlier" the resurfacing
//! engine exists to bring back. `list_tasks` is a pure disk walk of
//! `scopes/<p>/<w>/tasks/<task_id>/` that already hydrates each task into a
//! [`TaskListItemV3`] carrying its status, title, synthesized
//! `completion_summary`, derived `completion_outcome`, and `updated_at`
//! timestamp, so the adapter normalizes that projection rather than re-reading
//! raw task/execution state.
//!
//! **Granularity — per task.** One corpus item per task (not per execution).
//! A task's `updated_at` is stamped on its terminal transition
//! (`update_task_status` writes `Utc::now().to_rfc3339()`), so the item's
//! `occurred_at` is the moment the task reached its terminal state.
//!
//! **Terminal-only.** Only completed / failed / cancelled tasks resurface —
//! the engine brings back what is DONE, never work that is still pending,
//! ready, running, paused, or deferred. Tasks whose `updated_at` doesn't parse
//! are skipped: without a stamp we cannot tell whether they changed since the
//! watermark, and re-emitting them on every scan would defeat the watermark
//! (mirrors the memory source's undatable-entry skip).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi};
use crate::magician_v2::attention::resurfacing::types::{CorpusItem, SourceKind};

use super::ResurfacingSource;

/// Adapts a scope's terminal tasks into resurfacing corpus items.
///
/// Holds the shared [`ArtifactV2Service`] the rest of the runtime uses to read
/// scoped task state, mirroring how [`MemorySource`](super::memory::MemorySource)
/// holds its `AgentMemoryResolver`.
#[derive(Clone)]
pub struct TaskEpisodeSource {
    service: Arc<ArtifactV2Service>,
}

impl TaskEpisodeSource {
    /// Construct over the shared task service (the same handle the worker and
    /// API layer already hold).
    pub fn new(service: Arc<ArtifactV2Service>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ResurfacingSource for TaskEpisodeSource {
    fn corpus_kind(&self) -> &'static str {
        // == SourceKind::Task.as_str(); the store's watermark row keys on this.
        SourceKind::Task.as_str()
    }

    async fn list_changed_since(
        &self,
        principal: &str,
        workspace: &str,
        watermark: i64,
    ) -> Result<Vec<CorpusItem>> {
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        let tasks =
            self.service.list_tasks(&scope).await.with_context(|| {
                format!("list tasks for {principal}/{workspace} for resurfacing")
            })?;
        Ok(map_task_records(&tasks, watermark))
    }
}

/// Map task list items into corpus items for the *terminal* tasks whose
/// `updated_at` (unix seconds) is strictly greater than `watermark`.
///
/// Pure over the already-loaded [`TaskListItemV3`] rows so the
/// record→[`CorpusItem`] mapping is unit-testable without seeding the full
/// service, exactly as the memory source factored `map_knowledge_tiers`.
/// Non-terminal tasks and tasks with an unparseable `updated_at` are skipped.
fn map_task_records(tasks: &[TaskListItemV3], watermark: i64) -> Vec<CorpusItem> {
    let mut items = Vec::new();
    for task in tasks {
        if !is_terminal_status(&task.status) {
            continue; // resurface only what is DONE
        }
        let Some(occurred_at) = task_updated_at(task) else {
            continue; // undatable: cannot compare against the watermark
        };
        if occurred_at <= watermark {
            continue;
        }
        let title = task_title(task);
        let digest = task_digest(task);
        let embedding_text = if digest.is_empty() {
            title.clone()
        } else {
            format!("{title}\n{digest}")
        };
        items.push(CorpusItem {
            source_kind: SourceKind::Task,
            source_ref: task.id.clone(),
            title,
            digest,
            content_details: None,
            content_revision: None,
            occurred_at,
            watermark_cursor: occurred_at,
            embedding_text,
        });
    }
    items
}

/// Terminal task statuses — the work completed, gave up (failed), or was
/// abandoned (cancelled). `"canceled"` is accepted alongside `"cancelled"`
/// for back-compat with pre-existing single-l data (mirrors the cancel check
/// in `TaskState::synthesis_pending`). Held/transient states
/// (`pending`/`ready`/`running`/`paused`/`deferred`) are NOT terminal.
fn is_terminal_status(status: &str) -> bool {
    matches!(
        status.trim(),
        "completed" | "failed" | "cancelled" | "canceled"
    )
}

/// Parse a task's `updated_at` RFC3339 stamp into unix seconds. The service
/// stamps terminal transitions with `Utc::now().to_rfc3339()`; sub-second
/// precision is intentionally dropped to whole seconds for the watermark
/// comparison.
fn task_updated_at(task: &TaskListItemV3) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(task.updated_at.trim())
        .ok()
        .map(|dt| dt.timestamp())
}

/// Short human-facing title; falls back to the task id when the manifest title
/// is blank so the curator always has a stable handle.
fn task_title(task: &TaskListItemV3) -> String {
    let trimmed = task.title.trim();
    if trimmed.is_empty() {
        task.id.clone()
    } else {
        trimmed.to_string()
    }
}

/// Outcome/result summary a curator would show: prefer the synthesized
/// `completion_summary`, else the derived `completion_outcome` (which the list
/// builder always fills for terminal tasks — the summary, a plan error, or a
/// fallback like "Execution failed"), else empty.
fn task_digest(task: &TaskListItemV3) -> String {
    task.completion_summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            task.completion_outcome
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
        .to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::{TaskLifecycle, TaskOutputMode, TaskSyncMode};
    use crate::magician_v2::artifact_v2::CreateTaskInput;
    use crate::magician_v2::test_support::build_test_artifact_v2_service;
    use tempfile::tempdir;

    fn rfc3339_secs(s: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339")
            .timestamp()
    }

    /// A fully-populated `TaskListItemV3` the pure-mapping tests can clone and
    /// override, so each case only states the fields it cares about.
    fn base_task_item() -> TaskListItemV3 {
        TaskListItemV3 {
            id: "task_base".to_string(),
            title: "Base task".to_string(),
            description: String::new(),
            status: "completed".to_string(),
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
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn map_filters_terminal_and_watermark_and_maps_fields() {
        let tasks = vec![
            // Terminal but older than the watermark → dropped.
            TaskListItemV3 {
                id: "task_old".to_string(),
                status: "completed".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                ..base_task_item()
            },
            // Terminal and newer → survives, carries its summary as digest.
            TaskListItemV3 {
                id: "task_new".to_string(),
                title: "Ship the Q2 report".to_string(),
                status: "completed".to_string(),
                completion_summary: Some("Delivered the Q2 report to the board".to_string()),
                updated_at: "2026-06-01T00:00:00Z".to_string(),
                ..base_task_item()
            },
            // Newer but NON-terminal → dropped (we resurface only what's done).
            TaskListItemV3 {
                id: "task_running".to_string(),
                status: "running".to_string(),
                updated_at: "2026-06-02T00:00:00Z".to_string(),
                ..base_task_item()
            },
            // Terminal + newer but undatable → dropped (no watermark compare).
            TaskListItemV3 {
                id: "task_undatable".to_string(),
                status: "cancelled".to_string(),
                updated_at: "not-a-timestamp".to_string(),
                ..base_task_item()
            },
        ];
        let watermark = rfc3339_secs("2026-03-01T00:00:00Z");

        let items = map_task_records(&tasks, watermark);

        assert_eq!(items.len(), 1, "only the newer, terminal, datable task");
        let item = &items[0];
        assert_eq!(item.source_kind, SourceKind::Task);
        assert_eq!(item.source_ref, "task_new");
        assert_eq!(item.title, "Ship the Q2 report");
        assert_eq!(item.digest, "Delivered the Q2 report to the board");
        assert_eq!(
            item.embedding_text,
            "Ship the Q2 report\nDelivered the Q2 report to the board"
        );
        assert!(item.occurred_at > watermark);
    }

    #[test]
    fn digest_falls_back_to_completion_outcome_then_title_only_embedding() {
        // No summary, only the derived outcome the list builder fills for
        // failed tasks → digest comes from completion_outcome.
        let outcome_only = vec![TaskListItemV3 {
            id: "task_failed".to_string(),
            title: "Reconcile ledger".to_string(),
            status: "failed".to_string(),
            completion_summary: None,
            completion_outcome: Some("Execution failed".to_string()),
            updated_at: "2026-06-01T00:00:00Z".to_string(),
            ..base_task_item()
        }];
        let items = map_task_records(&outcome_only, 0);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].digest, "Execution failed");
        assert_eq!(
            items[0].embedding_text,
            "Reconcile ledger\nExecution failed"
        );

        // Neither summary nor outcome, and a blank title → title falls back to
        // the id and embedding_text is the title alone.
        let bare = vec![TaskListItemV3 {
            id: "task_bare".to_string(),
            title: "   ".to_string(),
            status: "completed".to_string(),
            completion_summary: None,
            completion_outcome: None,
            updated_at: "2026-06-01T00:00:00Z".to_string(),
            ..base_task_item()
        }];
        let items = map_task_records(&bare, 0);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "task_bare");
        assert_eq!(items[0].digest, "");
        assert_eq!(items[0].embedding_text, "task_bare");
    }

    fn make_create_input(principal: &str, workspace: &str, title: &str) -> CreateTaskInput {
        CreateTaskInput {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            title: title.to_string(),
            description: title.to_string(),
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
            output_mode: TaskOutputMode::Accumulate,
            chat_session_id: None,
            lifecycle: TaskLifecycle::default(),
            sync_mode: TaskSyncMode::default(),
        }
    }

    /// Real read path: seed two terminal tasks + one still-pending task through
    /// the service, then prove `list_changed_since` surfaces only the terminal
    /// ones as `Task` corpus items and honors the watermark.
    #[tokio::test]
    async fn list_changed_since_returns_only_terminal_tasks() {
        let tmp = tempdir().unwrap();
        let service = build_test_artifact_v2_service(tmp.path());
        let (principal, workspace) = ("anonymous", "default");
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );

        let done_a = service
            .create_task(make_create_input(principal, workspace, "Finished analysis"))
            .await
            .expect("create task a");
        let done_b = service
            .create_task(make_create_input(principal, workspace, "Finished digest"))
            .await
            .expect("create task b");
        let pending = service
            .create_task(make_create_input(principal, workspace, "Still going"))
            .await
            .expect("create task c");

        // Drive two tasks to a terminal state; leave the third pending.
        service
            .update_task_status(&scope, &done_a.manifest.task_id, "completed")
            .await
            .expect("complete a");
        service
            .update_task_status(&scope, &done_b.manifest.task_id, "cancelled")
            .await
            .expect("cancel b");

        let source = TaskEpisodeSource::new(service);
        assert_eq!(source.corpus_kind(), "task");

        let items = source
            .list_changed_since(principal, workspace, 0)
            .await
            .expect("list changed");

        assert_eq!(items.len(), 2, "both terminal tasks, not the pending one");
        assert!(items.iter().all(|it| it.source_kind == SourceKind::Task));
        let refs: Vec<&str> = items.iter().map(|it| it.source_ref.as_str()).collect();
        assert!(refs.contains(&done_a.manifest.task_id.as_str()));
        assert!(refs.contains(&done_b.manifest.task_id.as_str()));
        assert!(
            !refs.contains(&pending.manifest.task_id.as_str()),
            "the still-pending task must not resurface"
        );

        // A watermark past both completions filters everything out (strict >).
        let future = chrono::Utc::now().timestamp() + 3600;
        let none = source
            .list_changed_since(principal, workspace, future)
            .await
            .expect("list changed future");
        assert!(none.is_empty(), "nothing is newer than a future watermark");
    }

    /// The adapter is usable behind `Box<dyn ResurfacingSource>` (object-safe),
    /// exposing the stable `"task"` corpus kind.
    #[tokio::test]
    async fn is_object_safe_behind_dyn() {
        let tmp = tempdir().unwrap();
        let service = build_test_artifact_v2_service(tmp.path());
        let source: Box<dyn ResurfacingSource> = Box::new(TaskEpisodeSource::new(service));
        assert_eq!(source.corpus_kind(), "task");
        let items = source
            .list_changed_since("anonymous", "default", 0)
            .await
            .expect("empty scope lists cleanly");
        assert!(items.is_empty(), "no tasks seeded → no corpus items");
    }
}
