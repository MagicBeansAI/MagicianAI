use std::{
    backtrace::Backtrace,
    collections::BTreeMap,
    fs::{File, OpenOptions},
    future::Future,
    sync::{Arc, Weak},
};

use async_trait::async_trait;
use chrono::Utc;
use fs2::FileExt;
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::{
    models::{
        AppRecipeScheduleNodePhase, DelegationRef, ExecutionIndexEntry, ExecutionOutcomeSnapshot,
        ExecutionRecord, ExecutionScheduleRecord, FinalizedOutputs, OutputRef, TaskOutputMode,
        TaskRecord, CONTINUATION_CONTEXT_OUTPUT_ROLE,
    },
    service::{ArtifactV2Error, ExecutionContext, ScopeRef},
    task_writes::TaskWriteReconciler,
    workspace::ArtifactV2Workspace,
};

/// Cross-process recipe claims are deliberately shorter than reviewed node
/// activity. A live worker renews this epoch while it owns physical work;
/// process death stops renewal and makes the exact read retryable without
/// weakening the immutable node/aggregate deadline.
pub(crate) const APP_RECIPE_CLAIM_LEASE_MILLIS: i64 = 15_000;

fn app_recipe_claim_lease_is_live(claim_lease_expires_at_ms: i64, observed_at_ms: i64) -> bool {
    observed_at_ms < claim_lease_expires_at_ms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRecipeStepReducerAdmission {
    Reserved {
        claim_epoch: u64,
        deadline_at_ms: i64,
    },
    ResumeReserved {
        claim_epoch: u64,
        deadline_at_ms: i64,
    },
    ResumeStarted {
        claim_epoch: u64,
        deadline_at_ms: i64,
    },
    OwnedByLiveWorker,
    AlreadyCompleted,
    Cancelled,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRecipeCancellationReducerAdmission {
    RequestedBeforeDispatch,
    AwaitingStartedSettlement { deadline_at_ms: i64 },
    AlreadyRequestedBeforeDispatch,
    AlreadyRequestedAwaitingStartedSettlement { deadline_at_ms: i64 },
    AlreadySettled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRecipeCancellationDueAdmission {
    NotDue { deadline_at_ms: i64 },
    Cancelled,
    OutcomeUncertain,
    ReconcileTerminal { outcome_uncertain: bool },
    AlreadyTerminal,
}

#[async_trait]
pub trait ArtifactV2Reducer: Send + Sync {
    async fn reduce_app_recipe_execution_initialized(
        &self,
        task: &mut TaskRecord,
        execution: &ExecutionRecord,
        schedule: &ExecutionScheduleRecord,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_execution_schedule_snapshot(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution: &ExecutionRecord,
        candidate: &ExecutionScheduleRecord,
    ) -> Result<ExecutionScheduleRecord, ArtifactV2Error>;
    async fn reduce_app_recipe_schedule_prepared(
        &self,
        scope: &ScopeRef,
        schedule: &ExecutionScheduleRecord,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_app_recipe_step_reserved(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        input_encoded_len: u64,
        retry_identity_digest: &str,
        claim_owner_id: &str,
        now_ms: i64,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_step_started(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        claim_owner_id: &str,
        claim_epoch: u64,
        now_ms: i64,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_step_claim_renewed(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        claim_owner_id: &str,
        claim_epoch: u64,
        now_ms: i64,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_step_completed(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        attempt: u16,
        claim_owner_id: &str,
        claim_epoch: u64,
        output_persisted_at_ms: i64,
        completed_at_ms: i64,
        output: &OutputRef,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_step_output_adopted(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        output_claim_owner_id: &str,
        output_claim_epoch: u64,
        output_persisted_at_ms: i64,
        adopted_at_ms: i64,
        output: &OutputRef,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_steps_skipped(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_ids: &[String],
        updated_at: &str,
    ) -> Result<Vec<String>, ArtifactV2Error>;
    async fn reduce_app_recipe_cancellation_requested(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        requested_at: &str,
    ) -> Result<AppRecipeCancellationReducerAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_cancellation_settled(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        settled_at: &str,
    ) -> Result<bool, ArtifactV2Error>;
    async fn reduce_app_recipe_cancellation_due(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        due_at_ms: i64,
        settled_at: &str,
    ) -> Result<AppRecipeCancellationDueAdmission, ArtifactV2Error>;
    async fn reduce_app_recipe_execution_failed(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        outcome_uncertain: bool,
        failed_at: &str,
    ) -> Result<Vec<String>, ArtifactV2Error>;
    async fn reduce_task_created(&self, task: &TaskRecord) -> Result<(), ArtifactV2Error>;
    async fn reduce_execution_initialized(
        &self,
        task: &mut TaskRecord,
        execution: &ExecutionRecord,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_execution_discovered(
        &self,
        scope: &ScopeRef,
        execution: &ExecutionRecord,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_delegation_requested(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        parent_step_id: &str,
        sub_goal: &str,
        budget_iterations: usize,
        depth: usize,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_delegation_outcome(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        parent_step_id: &str,
        sub_goal: &str,
        outcome_type: &str,
        iterations_used: usize,
        duration_ms: u64,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_delegated_child_link(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        parent_execution_id: &str,
        child_execution_id: &str,
        child_agent_id: &str,
        sub_goal: &str,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_execution_nonterminal(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_execution_terminal(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
        outputs: &FinalizedOutputs,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_child_execution_terminal(
        &self,
        scope: &ScopeRef,
        parent_execution_id: &str,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
        execution_output: &OutputRef,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_child_execution_terminal_without_output(
        &self,
        scope: &ScopeRef,
        parent_execution_id: &str,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error>;
    async fn reduce_execution_terminal_without_outputs(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error>;
    /// Attach a deterministic, post-terminal auxiliary output (for example a
    /// continuation-context index) without replaying the terminal reducer or
    /// overwriting task state loaded before another concurrent mutation.
    async fn reduce_task_auxiliary_output(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        output: &OutputRef,
    ) -> Result<(), ArtifactV2Error>;
    /// Atomic Step 1 — flip execution status to terminal + mark
    /// `synthesis_pending = true`, also persist `synthesis_pending = true`
    /// on the task. Single multi-write-journal commit; safe to call
    /// before output synthesis runs (the heavy LLM work in
    /// `finalize_terminal_execution` / its spawned successor).
    ///
    /// After this call, on-disk state is consistent: execution is
    /// terminal, task knows synthesis is in flight, and a crash before
    /// synthesis lands is recoverable by the startup reconciler
    /// (synthesis is just re-spawned).
    async fn reduce_execution_terminal_status_only(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error>;
    /// Atomic write of "synthesis pipeline exhausted retries" — flips
    /// `execution.state.synthesis_pending` to `false`, stores the
    /// `SynthesisFailure` detail on `execution.state.synthesis_failed`,
    /// and surfaces the failed execution id on
    /// `task.state.synthesis_failed_execution_id` so list-side
    /// consumers can spot the failure without walking executions.
    /// Used by the retry path in `finalize_terminal_execution` once
    /// the per-stage retry policy is exhausted (#541). The operator
    /// HITL (also #541) reads `synthesis_failed` to render the
    /// "retry synthesis" affordance.
    async fn reduce_execution_synthesis_failed(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        failure: crate::magician_v2::artifact_v2::models::SynthesisFailure,
    ) -> Result<(), ArtifactV2Error>;
    /// Clear an execution's `synthesis_failed` marker and put it back
    /// into the in-flight registry. Inverse of
    /// `reduce_execution_synthesis_failed`. Called by the
    /// `retry-synthesis` endpoint before re-spawning the synthesis
    /// pipeline so consumers see the task return to `synthesis_pending`
    /// and the retry HITL affordance disappears.
    ///
    /// Returns `true` if THIS call performed the clear (won the race),
    /// `false` if another concurrent retry caller had already cleared
    /// the marker by the time we acquired the write lock. The caller
    /// uses this to decide whether to spawn the synthesis pipeline;
    /// only the race-winner spawns, preventing double-spawn from
    /// double-click / browser-retry scenarios.
    async fn reduce_clear_synthesis_failure(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
    ) -> Result<bool, ArtifactV2Error>;
    /// Write the task record's state + refs back to disk atomically
    /// without touching any execution record. Used by the startup
    /// reconciler's migration-backstop path to repopulate
    /// `synthesis_pending_executions` from pre-v548 on-disk data
    /// without going through the heavier `reduce_execution_terminal*`
    /// reducers (which expect a specific outcome to apply).
    async fn persist_task_state_only(&self, task: &TaskRecord) -> Result<(), ArtifactV2Error>;
    async fn reduce_runtime_signal(
        &self,
        scope: &ScopeRef,
        ctx: &ExecutionContext,
        execution_status: Option<&str>,
        task_status: Option<&str>,
        active_child_execution_ids: Option<&[String]>,
        current_step_id: Option<&str>,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error>;

    /// Reduce a step completion or failure event into execution state.
    ///
    /// Updates `completed_step_ids` / `failed_step_ids` / `current_step_id`
    /// on the execution record. These fields drive the deterministic plan view
    /// projection (V3 canonical event pattern).
    async fn reduce_step_event(
        &self,
        scope: &ScopeRef,
        ctx: &ExecutionContext,
        event_type: super::events::ArtifactV2EventType,
        step_id: &str,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error>;

    /// An action settled with a result — the flat loop's unit of advancement.
    /// See the implementation for why a direct run needs it.
    async fn reduce_action_settled(
        &self,
        scope: &ScopeRef,
        ctx: &ExecutionContext,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error>;

    async fn list_executions(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<Vec<ExecutionIndexEntry>, ArtifactV2Error>;
}

pub struct FilesystemArtifactV2Reducer {
    workspace: ArtifactV2Workspace,
    task_write_locks: std::sync::Mutex<BTreeMap<std::path::PathBuf, Weak<Mutex<()>>>>,
    /// The service's reconciler, shared as the same `Arc`.
    ///
    /// **This reducer is a task-record writer in its own right** — every
    /// execution transition lands `task_state.json`, and the terminal one
    /// flips `task.state.status` — so it needs the same index and Today-cache
    /// reconciliation `ArtifactV2Service` does, and it gets it by committing
    /// through the same type rather than by remembering to call two more
    /// methods afterwards.
    task_writes: Arc<TaskWriteReconciler>,
}

struct CrossProcessTaskGuard {
    file: File,
}

impl FilesystemArtifactV2Reducer {
    /// A reducer cannot be built without a reconciler, because a reducer that
    /// could would be a task writer with no route to the index — which is
    /// exactly the bug this argument exists to make unrepresentable. Tests
    /// that want the no-index behaviour pass a reconciler with nothing wired
    /// into it; that is the production shape too, before `set_list_index`.
    pub fn new(workspace: ArtifactV2Workspace, task_writes: Arc<TaskWriteReconciler>) -> Self {
        Self {
            workspace,
            task_write_locks: std::sync::Mutex::new(BTreeMap::new()),
            task_writes,
        }
    }

    fn task_scope(task: &TaskRecord) -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(
            &task.manifest.principal.clone(),
            &task.manifest.workspace.clone(),
        )
    }

    fn task_record_writes(
        &self,
        task: &TaskRecord,
    ) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>, ArtifactV2Error> {
        let scope = Self::task_scope(task);
        Ok(vec![
            (
                self.workspace.task_manifest_path(
                    &scope.principal(),
                    &scope.workspace(),
                    &task.manifest.task_id,
                ),
                Self::serialize_json_pretty(&task.manifest)?,
            ),
            (
                self.workspace.task_state_path(
                    &scope.principal(),
                    &scope.workspace(),
                    &task.manifest.task_id,
                ),
                Self::serialize_json_pretty(&self.task_state_with_known_completion(
                    &self.workspace.task_state_path(
                        &scope.principal(),
                        &scope.workspace(),
                        &task.manifest.task_id,
                    ),
                    &task.state,
                ))?,
            ),
            (
                self.workspace.task_refs_path(
                    &scope.principal(),
                    &scope.workspace(),
                    &task.manifest.task_id,
                ),
                Self::serialize_json_pretty(&task.refs)?,
            ),
        ])
    }

    /// Writes JUST `task.state` + `task.refs`, NOT `task.manifest`.
    ///
    /// Use this from reducers that mutate state/refs (terminal-execution
    /// reducers, synthesis-failure reducers) to avoid clobbering
    /// concurrent manifest edits (rename, retag, repriority,
    /// reschedule, due-date change) that arrive between the caller's
    /// load and the eventual write. Manifest is owned by other paths
    /// (`UpdateTaskInput` etc.); reducers that don't change it must
    /// not write it.
    fn task_state_and_refs_writes(
        &self,
        task: &TaskRecord,
    ) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>, ArtifactV2Error> {
        let scope = Self::task_scope(task);
        Ok(vec![
            (
                self.workspace.task_state_path(
                    &scope.principal(),
                    &scope.workspace(),
                    &task.manifest.task_id,
                ),
                Self::serialize_json_pretty(&task.state)?,
            ),
            (
                self.workspace.task_refs_path(
                    &scope.principal(),
                    &scope.workspace(),
                    &task.manifest.task_id,
                ),
                Self::serialize_json_pretty(&task.refs)?,
            ),
        ])
    }

    fn execution_record_writes(
        &self,
        scope: &ScopeRef,
        execution: &ExecutionRecord,
    ) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>, ArtifactV2Error> {
        let task_id = &execution.state.task_id;
        let execution_id = &execution.state.execution_id;
        let state_path = self.workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        // **A write that knows less never erases a known completion kind.**
        // Every commit of an execution record funnels through here, and
        // several of them serialize a clone loaded before the terminal flip —
        // the parent's child-output handler, the finalizer's second pass, the
        // settlement replay. The kind is monotonic for a `completed` record:
        // once recorded it does not become unknown again, so a clone that
        // carries `None` is restored from disk rather than allowed to clobber.
        let mut state = execution.state.clone();
        if state.status == "completed" && state.completion_kind.is_none() {
            match self
                .workspace
                .read_json_path_sync::<super::models::ExecutionState, _>(&state_path)
            {
                Ok(on_disk) => {
                    if on_disk.completion_kind.is_some() {
                        state.completion_kind = on_disk.completion_kind;
                        state.open_items = on_disk.open_items;
                    }
                },
                Err(_) => {},
            }
        }
        Ok(vec![
            (state_path, Self::serialize_json_pretty(&state)?),
            (
                self.workspace.execution_refs_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                    execution_id,
                ),
                Self::serialize_json_pretty(&execution.refs)?,
            ),
        ])
    }

    /// The task-level twin of the rule in `execution_record_writes`: a task
    /// clone loaded before its root closed carries no completion kind, and
    /// serializing it must not erase the one the root's terminal wrote.
    fn task_state_with_known_completion(
        &self,
        state_path: &std::path::Path,
        state: &super::models::TaskState,
    ) -> super::models::TaskState {
        let mut state = state.clone();
        if state.status == "completed" && state.completion_kind.is_none() {
            if let Ok(on_disk) = self
                .workspace
                .read_json_path_sync::<super::models::TaskState, _>(state_path)
            {
                if on_disk.completion_kind.is_some() {
                    state.completion_kind = on_disk.completion_kind;
                    state.open_items = on_disk.open_items;
                }
            }
        }
        state
    }

    fn serialize_json_pretty<T: Serialize>(value: &T) -> Result<Vec<u8>, ArtifactV2Error> {
        Ok(serde_json::to_vec_pretty(value)?)
    }

    fn execution_index_entry(execution: &ExecutionRecord) -> ExecutionIndexEntry {
        ExecutionIndexEntry {
            execution_id: execution.state.execution_id.clone(),
            task_id: execution.state.task_id.clone(),
            root_execution_id: execution.state.root_execution_id.clone(),
            parent_execution_id: execution.state.parent_execution_id.clone(),
            agent_id: execution.state.agent_id.clone(),
            relationship_type: execution.state.relationship_type.clone(),
            status: execution.state.status.clone(),
            plan_id: execution.state.plan_id.clone(),
            primary_execution_output_id: execution.state.primary_execution_output_id.clone(),
            active_child_execution_ids: execution.state.active_child_execution_ids.clone(),
            completion_kind: execution.state.completion_kind,
            open_items: execution.state.open_items.clone(),
            started_at: execution.state.started_at.clone(),
            completed_at: execution.state.completed_at.clone(),
            updated_at: execution.state.updated_at.clone(),
        }
    }

    async fn execution_index_write(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        executions: &[&ExecutionRecord],
    ) -> Result<(std::path::PathBuf, Vec<u8>), ArtifactV2Error> {
        let path = self.workspace.task_executions_index_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        );
        // Tolerant on purpose, and this is the one site where it also repairs:
        // the merged set is written straight back, so a record dropped here is
        // dropped from the file permanently rather than blocking every future
        // write to it. Strict, one bad line meant the index could never be
        // updated again for this task.
        let existing = self
            .workspace
            .read_jsonl_path_tolerant::<ExecutionIndexEntry, _>(&path)
            .await?;
        if existing.lost_committed_records() {
            tracing::warn!(
                target: "artifact_v2::reducer",
                path = %path.display(),
                corrupt_records = existing.corrupt,
                "dropping unreadable execution-index records; the rewrite below removes them"
            );
        }
        let mut latest_by_execution = BTreeMap::new();
        for entry in existing.records {
            latest_by_execution.insert(entry.execution_id.clone(), entry);
        }
        for execution in executions {
            latest_by_execution.insert(
                execution.state.execution_id.clone(),
                Self::execution_index_entry(execution),
            );
        }
        let mut content = Vec::new();
        for entry in latest_by_execution.into_values() {
            content.extend(serde_json::to_vec(&entry)?);
            content.push(b'\n');
        }
        Ok((path, content))
    }

    async fn acquire_task_write_guard(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<CrossProcessTaskGuard, ArtifactV2Error> {
        ArtifactV2Workspace::validate_task_id(task_id)?;
        let lock_path =
            self.workspace
                .task_lock_path(&scope.principal(), &scope.workspace(), task_id);
        if let Some(parent) = lock_path.parent() {
            self.workspace.create_dir_all_path(parent).await?;
        }
        let file = magician_core::blocking_admission::spawn_blocking_admitted(
            move || -> Result<File, std::io::Error> {
                let file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .open(&lock_path)?;
                file.lock_exclusive()?;
                Ok(file)
            },
        )
        .await
        .map_err(|err| ArtifactV2Error::Runtime(format!("task lock task panicked: {err}")))??;
        let deletion_marker = self.workspace.task_deletion_marker_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        );
        if self
            .workspace
            .symlink_metadata_path(&deletion_marker)
            .await?
            .is_some()
        {
            return Err(ArtifactV2Error::TaskNotFound(task_id.to_string()));
        }
        Ok(CrossProcessTaskGuard { file })
    }

    /// Take this task's process and file locks, finish any abandoned write,
    /// then run the op. Waiting for one task must not hold up another task's
    /// transitions or lease heartbeats. Same-task waiters queue asynchronously
    /// before occupying a blocking worker for the cross-process file lock.
    ///
    /// The recovery goes through `TaskWriteReconciler` rather than the raw
    /// workspace primitive because **a replay is a task write**: it applies a
    /// persisted write set verbatim, task record included, and so completes a
    /// commit that died between "journal durable" and "writes applied". This
    /// runs on every reducer op, so a replay that reconciled nothing would
    /// have been the likeliest way of all to leave a stale index row behind.
    async fn with_task_write_lock<R, Fut>(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        op: impl FnOnce() -> Fut,
    ) -> Result<R, ArtifactV2Error>
    where
        R: Send,
        Fut: Future<Output = Result<R, ArtifactV2Error>> + Send,
    {
        let lock_path =
            self.workspace
                .task_lock_path(&scope.principal(), &scope.workspace(), task_id);
        let task_lock = {
            let mut locks = self
                .task_write_locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match locks.get(&lock_path).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    locks.retain(|_, lock| lock.strong_count() > 0);
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(lock_path, Arc::downgrade(&lock));
                    lock
                },
            }
        };
        let _guard = task_lock.lock().await;
        let _task_guard = self.acquire_task_write_guard(scope, task_id).await?;
        self.task_writes.recover_task_writes(scope, task_id).await?;
        let result = op().await;
        if result.is_err() {
            // `acquire_task_write_guard` materializes the task directory and
            // its lock file before anyone knows whether a write will follow, so
            // an op that fails leaves a directory containing nothing but the
            // lock — a husk with no state to recover. They accumulated at a few
            // a month and were permanent: nothing deletes a task that was never
            // written, and the planning bootstrap could only skip them.
            //
            // Safe to remove exactly here and nowhere else, because the
            // exclusive guard is still held: no other process can be between
            // creating this directory and writing its first state. Unlinking
            // under a held fd is fine on Unix.
            self.discard_task_dir_if_only_lock(scope, task_id).await;
        }
        result
    }

    /// Remove a task directory that holds nothing but `.artifact_v2.lock`.
    ///
    /// Best-effort and silent on failure: this is hygiene on an error path, and
    /// must never convert a failed write into a second, louder failure. It
    /// cannot destroy data — a directory whose only entry is the lock has none.
    async fn discard_task_dir_if_only_lock(&self, scope: &ScopeRef, task_id: &str) {
        for dir in [
            self.workspace
                .user_visible_task_dir(&scope.principal(), &scope.workspace(), task_id),
            self.workspace
                .internal_task_dir(&scope.principal(), &scope.workspace(), task_id),
        ] {
            match self.workspace.symlink_metadata_path(&dir).await {
                Ok(Some(metadata)) if metadata.is_dir() && !metadata.file_type().is_symlink() => {},
                _ => continue,
            }
            let Ok(entries) = self.workspace.read_dir_path(&dir).await else {
                continue;
            };
            if entries
                .iter()
                .any(|entry| entry.file_name != ".artifact_v2.lock")
            {
                continue;
            }
            if self.workspace.remove_dir_all_path(&dir).await.is_ok() {
                debug!(
                    task_id,
                    "discarded a task directory left holding only its lock after a failed write"
                );
            }
        }
    }

    /// Every write this reducer performs lands here — task records, execution
    /// records and the execution index alike — which is why the index and
    /// Today reconciliation hangs off this one call rather than off the
    /// eighteen `commit_task_writes` call sites above and below it.
    ///
    /// The reconciler decides from the write set whether a task record was
    /// among the paths; an execution-only commit changes no list row and is
    /// left alone.
    ///
    /// Borrows the write set rather than taking it: the commit reads the paths
    /// and bytes and keeps neither, so the same shape runs all the way down to
    /// [`TaskWriteReconciler::commit_task_writes`] and the journal primitive
    /// under it.
    async fn commit_task_writes(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        writes: &[(std::path::PathBuf, Vec<u8>)],
    ) -> Result<(), ArtifactV2Error> {
        self.task_writes
            .commit_task_writes(scope, task_id, writes)
            .await
    }

    async fn load_task_record(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<TaskRecord, ArtifactV2Error> {
        Ok(TaskRecord {
            manifest: self
                .workspace
                .read_json_path(self.workspace.task_manifest_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                ))
                .await?,
            state: self
                .workspace
                .read_json_path(self.workspace.task_state_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                ))
                .await?,
            refs: self
                .workspace
                .read_json_path(self.workspace.task_refs_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                ))
                .await?,
        })
    }

    async fn load_execution_record(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<ExecutionRecord, ArtifactV2Error> {
        Ok(ExecutionRecord {
            state: self
                .workspace
                .read_json_path(self.workspace.execution_state_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                    execution_id,
                ))
                .await?,
            refs: self
                .workspace
                .read_json_path(self.workspace.execution_refs_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                    execution_id,
                ))
                .await?,
        })
    }

    fn upsert_output_ref(output_refs: &mut Vec<OutputRef>, output: OutputRef) {
        if let Some(existing) = output_refs
            .iter_mut()
            .find(|existing| existing.output_id == output.output_id)
        {
            *existing = output;
        } else {
            output_refs.push(output);
        }
    }

    fn prune_stale_primary_task_outputs(
        output_refs: &mut Vec<OutputRef>,
        outputs: &FinalizedOutputs,
    ) {
        if !matches!(outputs.task_output_mode, TaskOutputMode::Overwrite) {
            return;
        }

        let current_task_agent_id = outputs.task_agent_output.output_id.as_str();
        let current_task_user_id = outputs.task_user_output.output_id.as_str();
        let current_continuation_context_id = outputs
            .continuation_context_output
            .as_ref()
            .map(|output| output.output_id.as_str());

        output_refs.retain(|output| match output.role.as_str() {
            "primary_task_agent" => output.output_id == current_task_agent_id,
            "primary_task_user" => output.output_id == current_task_user_id,
            CONTINUATION_CONTEXT_OUTPUT_ROLE => {
                Some(output.output_id.as_str()) == current_continuation_context_id
            },
            _ => true,
        });
    }

    fn remove_child_execution_id(child_ids: &mut Vec<String>, execution_id: &str) {
        child_ids.retain(|existing| existing != execution_id);
    }

    fn task_status_keeps_active_root(status: &str) -> bool {
        // `deferred` is the Artifact task projection of an exact runtime
        // `Sleeping` execution. The execution-bound timer resumes this same
        // root, so clearing its pointer would both lose the generation fence
        // and permit a concurrent replacement root before the timer fires.
        matches!(status, "planning" | "running" | "paused" | "deferred")
    }

    fn execution_status_is_terminal(status: &str) -> bool {
        matches!(status, "completed" | "failed" | "cancelled" | "canceled")
    }

    fn latest_rfc3339(existing: &str, candidate: &str) -> String {
        match (
            chrono::DateTime::parse_from_rfc3339(existing),
            chrono::DateTime::parse_from_rfc3339(candidate),
        ) {
            (Ok(existing_at), Ok(candidate_at)) if existing_at > candidate_at => {
                existing.to_string()
            },
            (Ok(_), Ok(_)) => candidate.to_string(),
            _ if existing > candidate => existing.to_string(),
            _ => candidate.to_string(),
        }
    }

    fn advance_optional_timestamp(existing: Option<&str>, candidate: &str) -> String {
        existing
            .map(|existing| Self::latest_rfc3339(existing, candidate))
            .unwrap_or_else(|| candidate.to_string())
    }

    fn is_root_execution(execution: &ExecutionRecord) -> bool {
        execution.state.parent_execution_id.is_none()
            && execution
                .state
                .root_execution_id
                .as_deref()
                .is_none_or(|root_id| root_id == execution.state.execution_id)
    }

    fn repair_child_root_ownership(task: &mut TaskRecord, child: &ExecutionRecord) -> bool {
        let child_id = child.state.execution_id.as_str();
        let owns_active = task.state.active_root_execution_id.as_deref() == Some(child_id);
        let owns_latest = task.state.latest_root_execution_id.as_deref() == Some(child_id);
        if !owns_active && !owns_latest {
            return false;
        }

        // The child's recorded root is the only lineage fact that can restore
        // an active root. A legacy/corrupt child can lack it (or point to
        // itself); in that case fail closed by removing child ownership. For
        // the historical latest pointer, a distinct completed root remains a
        // safe fallback, but it must never be installed as the active run.
        let recorded_root_id = child
            .state
            .root_execution_id
            .as_deref()
            .filter(|root_id| *root_id != child_id);
        let historical_root_id = task
            .state
            .last_completed_root_execution_id
            .as_deref()
            .filter(|root_id| *root_id != child_id);

        if owns_active {
            task.state.active_root_execution_id = recorded_root_id.map(str::to_string);
        }
        if owns_latest {
            task.state.latest_root_execution_id =
                recorded_root_id.or(historical_root_id).map(str::to_string);
        }
        warn!(
            task_id = %task.manifest.task_id,
            child_execution_id = %child_id,
            restored_root_execution_id = ?recorded_root_id,
            "[REDUCER] repaired delegated child that had replaced task root ownership"
        );
        true
    }

    async fn commit_child_execution_with_root_repair<F>(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        seed_execution: &ExecutionRecord,
        requested_status: Option<String>,
        update: F,
    ) -> Result<ExecutionRecord, ArtifactV2Error>
    where
        F: FnOnce(&mut ExecutionRecord) + Send,
    {
        self.with_task_write_lock(scope, task_id, move || async move {
            // Generic observers can carry a task snapshot loaded before the
            // root advanced. Re-read under the task lock and permit exactly
            // one task mutation: removing/restoring a pointer that currently
            // names this child. The child record is also re-read here: callers
            // often waited on synthesis or another reducer before acquiring
            // this lock, so serializing their old clone would erase newer
            // steps, child links, or output refs.
            let mut fresh_task = self.load_task_record(scope, task_id).await?;
            let state_path = self.workspace.execution_state_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                &seed_execution.state.execution_id,
            );
            let mut fresh_execution = if self
                .workspace
                .symlink_metadata_path(&state_path)
                .await?
                .is_some()
            {
                self.load_execution_record(scope, task_id, &seed_execution.state.execution_id)
                    .await?
            } else {
                seed_execution.clone()
            };
            let durable_status = fresh_execution.state.status.clone();
            let reject_update = Self::execution_status_is_terminal(&durable_status)
                && requested_status
                    .as_deref()
                    .is_some_and(|status| !Self::execution_status_is_terminal(status));
            if reject_update {
                warn!(
                    task_id = %task_id,
                    execution_id = %fresh_execution.state.execution_id,
                    durable_status = %durable_status,
                    rejected_status = ?requested_status,
                    "[REDUCER] ignored stale child update that would regress terminal execution"
                );
            } else {
                update(&mut fresh_execution);
            }
            if !reject_update
                && Self::execution_status_is_terminal(&durable_status)
                && durable_status != fresh_execution.state.status
            {
                warn!(
                    task_id = %task_id,
                    execution_id = %fresh_execution.state.execution_id,
                    durable_status = %durable_status,
                    rejected_status = %fresh_execution.state.status,
                    "[REDUCER] retained first durable terminal child status"
                );
                fresh_execution.state.status = durable_status;
            }
            let repaired = Self::repair_child_root_ownership(&mut fresh_task, &fresh_execution);
            let mut writes = self.execution_record_writes(scope, &fresh_execution)?;
            if repaired {
                writes.extend(self.task_state_and_refs_writes(&fresh_task)?);
            }
            writes.push(
                self.execution_index_write(scope, task_id, &[&fresh_execution])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(fresh_execution)
        })
        .await
    }

    fn upsert_delegation_ref(
        delegations: &mut Vec<DelegationRef>,
        parent_step_id: &str,
        sub_goal: &str,
        child_execution_id: Option<&str>,
        updater: impl FnOnce(Option<DelegationRef>) -> DelegationRef,
    ) {
        let position = delegations.iter().position(|delegation| {
            if let Some(child_execution_id) = child_execution_id {
                delegation.child_execution_id.as_deref() == Some(child_execution_id)
                    || (delegation.child_execution_id.is_none()
                        && delegation.sub_goal == sub_goal
                        && (parent_step_id.is_empty()
                            || delegation.parent_step_id == parent_step_id))
            } else {
                delegation.parent_step_id == parent_step_id && delegation.sub_goal == sub_goal
            }
        });

        let existing = position.map(|index| delegations.remove(index));
        delegations.push(updater(existing));
    }
}

/// Persist-time invariant check for the task root pointers. The reducer
/// repairs a child that replaced the root when it next loads the task, but
/// repair on read never names the writer; this names the write path so the
/// next occurrence points at its source instead of at the repair. `load`
/// resolves an execution id to its record; a missing record is not a finding.
pub(crate) async fn warn_if_child_installed_as_root<F, Fut>(
    task: &TaskRecord,
    write_path: &'static str,
    load: F,
) where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<ExecutionRecord>>,
{
    for (pointer, execution_id) in [
        (
            "active_root_execution_id",
            task.state.active_root_execution_id.as_deref(),
        ),
        (
            "latest_root_execution_id",
            task.state.latest_root_execution_id.as_deref(),
        ),
    ] {
        let Some(execution_id) = execution_id else {
            continue;
        };
        let Some(execution) = load(execution_id.to_owned()).await else {
            continue;
        };
        if execution.state.parent_execution_id.is_some()
            || execution.state.relationship_type != "root"
        {
            warn!(
                task_id = %task.manifest.task_id,
                pointer,
                execution_id,
                parent_execution_id = ?execution.state.parent_execution_id,
                relationship_type = %execution.state.relationship_type,
                write_path,
                "[REDUCER] a child execution is being written as the task root pointer; the reader-side repair will undo it, fix the writer"
            );
        }
    }
}

#[async_trait]
impl ArtifactV2Reducer for FilesystemArtifactV2Reducer {
    async fn reduce_execution_schedule_snapshot(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution: &ExecutionRecord,
        candidate: &ExecutionScheduleRecord,
    ) -> Result<ExecutionScheduleRecord, ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                &execution.state.execution_id,
            );
            if self.workspace.symlink_metadata_path(&path).await?.is_some() {
                let existing: ExecutionScheduleRecord =
                    self.workspace.read_json_path(&path).await?;
                if existing.source_kind == "app_recipe_v1" {
                    if existing.execution_id != execution.state.execution_id
                        || existing.task_id != task_id
                        || existing.plan_id != execution.state.plan_id
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_schedule_substitution".to_owned(),
                        ));
                    }
                    return Ok(existing);
                }
            }
            let current = self
                .load_execution_record(scope, task_id, &execution.state.execution_id)
                .await?;
            if current.state.updated_at != execution.state.updated_at
                || current.state.status != execution.state.status
                || current.state.plan_id != execution.state.plan_id
                || candidate.source_kind == "app_recipe_v1"
                || candidate.execution_id != execution.state.execution_id
                || candidate.task_id != task_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "execution_schedule_snapshot_stale".to_owned(),
                ));
            }
            self.commit_task_writes(
                scope,
                task_id,
                &[(path, Self::serialize_json_pretty(candidate)?)],
            )
            .await?;
            Ok(candidate.clone())
        })
        .await
    }

    async fn reduce_app_recipe_execution_initialized(
        &self,
        task: &mut TaskRecord,
        execution: &ExecutionRecord,
        schedule: &ExecutionScheduleRecord,
    ) -> Result<(), ArtifactV2Error> {
        if schedule.source_kind != "app_recipe_v1"
            || schedule.task_id != task.manifest.task_id
            || schedule.execution_id != execution.state.execution_id
            || schedule.plan_id.is_none()
            || schedule.plan_id != execution.state.plan_id
            || schedule.source_plan_relative_path.is_none()
            || schedule.steps.is_empty()
            || schedule.steps.iter().enumerate().any(|(order, step)| {
                step.order != order
                    || step.step_id.is_empty()
                    || step.recipe.as_ref().is_none_or(|recipe| {
                        recipe.phase != AppRecipeScheduleNodePhase::Pending
                            || recipe.attempt != 0
                            || recipe.claim_epoch != 0
                            || recipe.claim_owner_id.is_some()
                            || recipe.claim_lease_expires_at_ms.is_some()
                            || recipe.active_deadline_at_ms.is_some()
                            || recipe.max_active_millis == 0
                            || recipe.cancellation_acknowledgement_timeout_millis == 0
                            || recipe.cancellation_acknowledgement_timeout_millis
                                > recipe.max_active_millis
                            || recipe.aggregate_deadline_at_ms <= 0
                            || recipe.input_digest.is_some()
                            || recipe.output.is_some()
                            || recipe.cancellation_requested_at.is_some()
                            || recipe.cancellation_ack_deadline_at_ms.is_some()
                    })
            })
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_atomic_start_invalid".to_owned(),
            ));
        }
        let scope = Self::task_scope(task);
        let task_id = task.manifest.task_id.clone();
        let requested_schedule_fire_count = task.state.schedule_fire_count;
        let fresh = self
            .with_task_write_lock(&scope, &task_id, || async {
                let mut fresh = self.load_task_record(&scope, &task_id).await?;
                if matches!(fresh.state.status.as_str(), "cancelled" | "canceled") {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "task_cancelled:{task_id}"
                    )));
                }
                if let Some(active) = fresh.state.active_root_execution_id.as_deref() {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "task_execution_in_progress:{active}"
                    )));
                }
                let schedule_path = self.workspace.execution_schedule_path(
                    &scope.principal(),
                    &scope.workspace(),
                    &task_id,
                    &execution.state.execution_id,
                );
                if self
                    .workspace
                    .symlink_metadata_path(&schedule_path)
                    .await?
                    .is_some()
                {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "app_recipe_schedule_preexists_root".to_owned(),
                    ));
                }
                fresh.state.status = "running".to_owned();
                fresh.state.active_root_execution_id = Some(execution.state.execution_id.clone());
                fresh.state.latest_root_execution_id = Some(execution.state.execution_id.clone());
                fresh.state.schedule_fire_count = fresh
                    .state
                    .schedule_fire_count
                    .max(requested_schedule_fire_count);
                fresh.state.last_progress_at = Some(execution.state.updated_at.clone());
                fresh.state.updated_at = execution.state.updated_at.clone();
                let mut writes = self.task_state_and_refs_writes(&fresh)?;
                writes.extend(self.execution_record_writes(&scope, execution)?);
                writes.push((schedule_path, Self::serialize_json_pretty(schedule)?));
                writes.push(
                    self.execution_index_write(&scope, &task_id, &[execution])
                        .await?,
                );
                self.commit_task_writes(&scope, &task_id, &writes).await?;
                Ok(fresh)
            })
            .await?;
        *task = fresh;
        Ok(())
    }

    async fn reduce_app_recipe_schedule_prepared(
        &self,
        scope: &ScopeRef,
        schedule: &ExecutionScheduleRecord,
    ) -> Result<(), ArtifactV2Error> {
        if schedule.source_kind != "app_recipe_v1"
            || schedule.execution_id.is_empty()
            || schedule.task_id.is_empty()
            || schedule.plan_id.is_none()
            || schedule.source_plan_relative_path.is_none()
            || schedule.steps.is_empty()
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_schedule_invalid".to_owned(),
            ));
        }
        let mut step_ids = std::collections::BTreeSet::new();
        let mut orders = std::collections::BTreeSet::new();
        let mut node_execution_ids = std::collections::BTreeSet::new();
        for step in &schedule.steps {
            let Some(recipe) = step.recipe.as_ref() else {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_binding_missing".to_owned(),
                ));
            };
            if step.step_id.is_empty()
                || recipe.node_execution_id.is_empty()
                || recipe.binding_digest.is_empty()
                || recipe.input_schema_ref.is_empty()
                || recipe.output_schema_ref.is_empty()
                || !step_ids.insert(step.step_id.as_str())
                || !orders.insert(step.order)
                || !node_execution_ids.insert(recipe.node_execution_id.as_str())
                || step.order >= schedule.steps.len()
                || recipe.phase != AppRecipeScheduleNodePhase::Pending
                || recipe.attempt != 0
                || recipe.claim_epoch != 0
                || recipe.claim_owner_id.is_some()
                || recipe.claim_lease_expires_at_ms.is_some()
                || recipe.active_deadline_at_ms.is_some()
                || recipe.max_active_millis == 0
                || recipe.cancellation_acknowledgement_timeout_millis == 0
                || recipe.cancellation_acknowledgement_timeout_millis > recipe.max_active_millis
                || recipe.aggregate_deadline_at_ms <= 0
                || recipe.input_digest.is_some()
                || recipe.input_encoded_len.is_some()
                || recipe.retry_identity_digest.is_some()
                || recipe.output.is_some()
                || recipe.cancellation_requested_at.is_some()
                || recipe.cancellation_ack_deadline_at_ms.is_some()
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_not_pristine".to_owned(),
                ));
            }
        }
        if orders.len() != schedule.steps.len()
            || !orders.iter().copied().eq(0..schedule.steps.len())
            || schedule
                .steps
                .iter()
                .filter(|step| {
                    step.recipe.as_ref().is_some_and(|recipe| {
                        recipe.parent_node_execution_id == schedule.execution_id
                    })
                })
                .count()
                != 1
            || schedule
                .steps
                .iter()
                .find(|step| step.order == 0)
                .is_none_or(|step| {
                    step.recipe.as_ref().is_none_or(|recipe| {
                        recipe.parent_node_execution_id != schedule.execution_id
                            || !step.depends_on_step_ids.is_empty()
                    })
                })
            || schedule.steps.iter().any(|step| {
                let Some(recipe) = step.recipe.as_ref() else {
                    return true;
                };
                recipe.parent_node_execution_id != schedule.execution_id
                    && !schedule.steps.iter().any(|candidate| {
                        candidate.order < step.order
                            && candidate.recipe.as_ref().is_some_and(|candidate_recipe| {
                                candidate_recipe.node_execution_id
                                    == recipe.parent_node_execution_id
                            })
                    })
                    || step.depends_on_step_ids.iter().any(|dependency| {
                        !schedule.steps.iter().any(|candidate| {
                            candidate.order < step.order && candidate.step_id == *dependency
                        })
                    })
            })
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_schedule_topology_invalid".to_owned(),
            ));
        }
        self.with_task_write_lock(scope, &schedule.task_id, || async {
            let task = self.load_task_record(scope, &schedule.task_id).await?;
            let mut execution = self
                .load_execution_record(scope, &schedule.task_id, &schedule.execution_id)
                .await?;
            if task.state.status != "running"
                || task.state.active_root_execution_id.as_deref()
                    != Some(schedule.execution_id.as_str())
                || execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref()
                    != Some(schedule.execution_id.as_str())
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_root_not_running".to_owned(),
                ));
            }
            let schedule_path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                &schedule.task_id,
                &schedule.execution_id,
            );
            if self
                .workspace
                .symlink_metadata_path(&schedule_path)
                .await?
                .is_some()
            {
                let existing: ExecutionScheduleRecord =
                    self.workspace.read_json_path(&schedule_path).await?;
                let pristine_runtime_seed = existing.source_kind == "runtime_context"
                    && existing.execution_id == schedule.execution_id
                    && existing.task_id == schedule.task_id
                    && existing.steps.len() == 1
                    && existing.steps[0].step_id == "step_execute_task"
                    && existing.steps[0].recipe.is_none()
                    && execution.state.completed_step_ids.is_empty()
                    && execution.state.failed_step_ids.is_empty()
                    && execution.state.current_step_id.is_none();
                let same_immutable_identity = existing.execution_id == schedule.execution_id
                    && existing.task_id == schedule.task_id
                    && existing.plan_id == schedule.plan_id
                    && existing.source_plan_relative_path == schedule.source_plan_relative_path
                    && existing.source_kind == schedule.source_kind
                    && existing.steps.len() == schedule.steps.len()
                    && existing
                        .steps
                        .iter()
                        .zip(&schedule.steps)
                        .all(|(left, right)| {
                            let recipe_matches = match (&left.recipe, &right.recipe) {
                                (Some(left), Some(right)) => {
                                    left.node_execution_id == right.node_execution_id
                                        && left.parent_node_execution_id
                                            == right.parent_node_execution_id
                                        && left.binding_digest == right.binding_digest
                                        && left.input_schema_ref == right.input_schema_ref
                                        && left.output_schema_ref == right.output_schema_ref
                                        && left.uncertainty == right.uncertainty
                                        && left.max_active_millis == right.max_active_millis
                                        && left.cancellation_acknowledgement_timeout_millis
                                            == right.cancellation_acknowledgement_timeout_millis
                                        && left.aggregate_deadline_at_ms
                                            == right.aggregate_deadline_at_ms
                                },
                                _ => false,
                            };
                            left.step_id == right.step_id
                                && left.title == right.title
                                && left.order == right.order
                                && left.depends_on_step_ids == right.depends_on_step_ids
                                && left.capability == right.capability
                                && left.delegate_agent_id == right.delegate_agent_id
                                && left.sub_step_labels == right.sub_step_labels
                                && recipe_matches
                        });
                if !same_immutable_identity && !pristine_runtime_seed {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "app_recipe_schedule_substitution".to_owned(),
                    ));
                }
                if same_immutable_identity {
                    let plan_ref_matches = schedule
                        .plan_id
                        .as_ref()
                        .zip(schedule.source_plan_relative_path.as_ref())
                        .is_some_and(|(plan_id, relative_path)| {
                            execution.state.plan_id.as_ref() == Some(plan_id)
                                && execution.refs.plan_refs.iter().any(|reference| {
                                    &reference.plan_id == plan_id
                                        && &reference.relative_path == relative_path
                                })
                        });
                    if !plan_ref_matches {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_execution_plan_substitution".to_owned(),
                        ));
                    }
                    return Ok(());
                }
            }
            execution.state.plan_id = schedule.plan_id.clone();
            let plan_id = schedule.plan_id.clone().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_plan_id_missing".to_owned())
            })?;
            let relative_path = schedule.source_plan_relative_path.clone().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_plan_path_missing".to_owned())
            })?;
            if !execution
                .refs
                .plan_refs
                .iter()
                .any(|reference| reference.plan_id == plan_id)
            {
                execution.refs.plan_refs.push(super::models::PlanRef {
                    plan_id,
                    relative_path,
                    created_at: schedule.updated_at.clone(),
                });
            }
            execution.refs.updated_at = Some(schedule.updated_at.clone());
            let mut writes = self.execution_record_writes(scope, &execution)?;
            writes.push((schedule_path, Self::serialize_json_pretty(schedule)?));
            writes.push(
                self.execution_index_write(scope, &schedule.task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, &schedule.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_app_recipe_step_reserved(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        input_encoded_len: u64,
        retry_identity_digest: &str,
        claim_owner_id: &str,
        now_ms: i64,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error> {
        if claim_owner_id.is_empty()
            || claim_owner_id.len() > 160
            || now_ms <= 0
            || claim_owner_id.chars().any(char::is_control)
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_claim_invalid".to_owned(),
            ));
        }
        self.with_task_write_lock(scope, task_id, || async {
            // The caller timestamp starts the reviewed active window, while
            // this fresh sample prevents task-lock wait from minting an
            // already-expired physical permit.
            let now_ms = Utc::now().timestamp_millis().max(now_ms);
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Ok(
                    if matches!(execution.state.status.as_str(), "cancelled" | "canceled") {
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    },
                );
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let step_index = schedule
                .steps
                .iter()
                .position(|step| step.step_id == step_id)
                .ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_missing".to_owned())
                })?;
            let step_snapshot = schedule.steps[step_index].clone();
            let recipe_snapshot = step_snapshot.recipe.as_ref().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
            })?;
            if recipe_snapshot.binding_digest != binding_digest {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_step_binding_substitution".to_owned(),
                ));
            }
            if matches!(
                recipe_snapshot.phase,
                AppRecipeScheduleNodePhase::Pending
                    | AppRecipeScheduleNodePhase::DispatchReserved
                    | AppRecipeScheduleNodePhase::Started
            ) {
                let dependencies_settled =
                    step_snapshot.depends_on_step_ids.iter().all(|dependency| {
                        schedule.steps.iter().any(|candidate| {
                            candidate.step_id == *dependency
                                && candidate.recipe.as_ref().is_some_and(|recipe| {
                                    recipe.phase == AppRecipeScheduleNodePhase::Completed
                                })
                        })
                    });
                let parent_started = recipe_snapshot.parent_node_execution_id == execution_id
                    || schedule.steps.iter().any(|candidate| {
                        candidate.recipe.as_ref().is_some_and(|recipe| {
                            recipe.node_execution_id == recipe_snapshot.parent_node_execution_id
                                && recipe.phase == AppRecipeScheduleNodePhase::Started
                        })
                    });
                if !dependencies_settled || !parent_started {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "app_recipe_step_not_ready".to_owned(),
                    ));
                }
            }
            let step = &mut schedule.steps[step_index];
            let recipe = step.recipe.as_mut().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
            })?;
            let was_started = recipe.phase == AppRecipeScheduleNodePhase::Started;
            let decision = match recipe.phase {
                AppRecipeScheduleNodePhase::Pending => {
                    if recipe.cancellation_requested_at.is_some() {
                        recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        let node_deadline = now_ms
                            .checked_add(i64::try_from(recipe.max_active_millis).map_err(|_| {
                                ArtifactV2Error::InvalidRequest(
                                    "app_recipe_deadline_overflow".to_owned(),
                                )
                            })?)
                            .ok_or_else(|| {
                                ArtifactV2Error::InvalidRequest(
                                    "app_recipe_deadline_overflow".to_owned(),
                                )
                            })?;
                        let deadline_at_ms = node_deadline.min(recipe.aggregate_deadline_at_ms);
                        if deadline_at_ms <= now_ms {
                            recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                            step.taskplan_status = "outcome_uncertain".to_owned();
                            AppRecipeStepReducerAdmission::OutcomeUncertain
                        } else {
                            let claim_lease_expires_at_ms = now_ms
                                .checked_add(APP_RECIPE_CLAIM_LEASE_MILLIS)
                                .ok_or_else(|| {
                                    ArtifactV2Error::InvalidRequest(
                                        "app_recipe_claim_lease_overflow".to_owned(),
                                    )
                                })?
                                .min(deadline_at_ms);
                            recipe.attempt = 1;
                            recipe.claim_epoch = 1;
                            recipe.claim_owner_id = Some(claim_owner_id.to_owned());
                            recipe.claim_lease_expires_at_ms = Some(claim_lease_expires_at_ms);
                            recipe.active_deadline_at_ms = Some(deadline_at_ms);
                            recipe.input_digest = Some(input_digest.to_owned());
                            recipe.input_encoded_len = Some(input_encoded_len);
                            recipe.retry_identity_digest = Some(retry_identity_digest.to_owned());
                            recipe.phase = AppRecipeScheduleNodePhase::DispatchReserved;
                            step.taskplan_status = "reserved".to_owned();
                            step.taskplan_progress = "0/1".to_owned();
                            AppRecipeStepReducerAdmission::Reserved {
                                claim_epoch: 1,
                                deadline_at_ms,
                            }
                        }
                    }
                },
                AppRecipeScheduleNodePhase::DispatchReserved
                | AppRecipeScheduleNodePhase::Started => {
                    if recipe.attempt != 1
                        || recipe.input_digest.as_deref() != Some(input_digest)
                        || recipe.input_encoded_len != Some(input_encoded_len)
                        || recipe.retry_identity_digest.as_deref() != Some(retry_identity_digest)
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_step_input_substitution".to_owned(),
                        ));
                    }
                    let claim_lease_expires_at_ms =
                        recipe.claim_lease_expires_at_ms.ok_or_else(|| {
                            ArtifactV2Error::InvalidRequest("app_recipe_claim_missing".to_owned())
                        })?;
                    let active_deadline_at_ms = recipe.active_deadline_at_ms.ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest("app_recipe_deadline_missing".to_owned())
                    })?;
                    if active_deadline_at_ms <= now_ms {
                        recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                        step.taskplan_status = "outcome_uncertain".to_owned();
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    } else if recipe.cancellation_requested_at.is_some() {
                        if recipe.phase == AppRecipeScheduleNodePhase::Started {
                            AppRecipeStepReducerAdmission::OwnedByLiveWorker
                        } else {
                            recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                            step.taskplan_status = "cancelled".to_owned();
                            AppRecipeStepReducerAdmission::Cancelled
                        }
                    } else if recipe.claim_owner_id.as_deref() == Some(claim_owner_id) {
                        if claim_lease_expires_at_ms <= now_ms {
                            recipe.claim_lease_expires_at_ms = Some(
                                now_ms
                                    .checked_add(APP_RECIPE_CLAIM_LEASE_MILLIS)
                                    .ok_or_else(|| {
                                        ArtifactV2Error::InvalidRequest(
                                            "app_recipe_claim_lease_overflow".to_owned(),
                                        )
                                    })?
                                    .min(active_deadline_at_ms),
                            );
                        }
                        if recipe.phase == AppRecipeScheduleNodePhase::Started {
                            AppRecipeStepReducerAdmission::ResumeStarted {
                                claim_epoch: recipe.claim_epoch,
                                deadline_at_ms: active_deadline_at_ms,
                            }
                        } else {
                            AppRecipeStepReducerAdmission::ResumeReserved {
                                claim_epoch: recipe.claim_epoch,
                                deadline_at_ms: active_deadline_at_ms,
                            }
                        }
                    } else if claim_lease_expires_at_ms > now_ms {
                        AppRecipeStepReducerAdmission::OwnedByLiveWorker
                    } else {
                        let claim_epoch = recipe.claim_epoch.checked_add(1).ok_or_else(|| {
                            ArtifactV2Error::InvalidRequest(
                                "app_recipe_claim_epoch_overflow".to_owned(),
                            )
                        })?;
                        let claim_lease_expires_at_ms = now_ms
                            .checked_add(APP_RECIPE_CLAIM_LEASE_MILLIS)
                            .ok_or_else(|| {
                                ArtifactV2Error::InvalidRequest(
                                    "app_recipe_claim_lease_overflow".to_owned(),
                                )
                            })?
                            .min(active_deadline_at_ms);
                        if claim_lease_expires_at_ms <= now_ms {
                            recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                            step.taskplan_status = "outcome_uncertain".to_owned();
                            AppRecipeStepReducerAdmission::OutcomeUncertain
                        } else {
                            recipe.claim_epoch = claim_epoch;
                            recipe.claim_owner_id = Some(claim_owner_id.to_owned());
                            recipe.claim_lease_expires_at_ms = Some(claim_lease_expires_at_ms);
                            recipe.phase = AppRecipeScheduleNodePhase::DispatchReserved;
                            step.taskplan_status = "reserved".to_owned();
                            AppRecipeStepReducerAdmission::Reserved {
                                claim_epoch,
                                deadline_at_ms: active_deadline_at_ms,
                            }
                        }
                    }
                },
                AppRecipeScheduleNodePhase::Completed => {
                    AppRecipeStepReducerAdmission::AlreadyCompleted
                },
                AppRecipeScheduleNodePhase::Cancelled | AppRecipeScheduleNodePhase::Skipped => {
                    AppRecipeStepReducerAdmission::Cancelled
                },
                AppRecipeScheduleNodePhase::Failed
                | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                    AppRecipeStepReducerAdmission::OutcomeUncertain
                },
            };
            schedule.updated_at = updated_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            if was_started && matches!(decision, AppRecipeStepReducerAdmission::OutcomeUncertain) {
                if execution.state.current_step_id.as_deref() == Some(step_id) {
                    execution.state.current_step_id = None;
                }
                execution.state.updated_at = updated_at.to_owned();
                task.state.last_progress_at = Some(updated_at.to_owned());
                task.state.updated_at = updated_at.to_owned();
                writes.extend(self.execution_record_writes(scope, &execution)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, task_id, &[&execution])
                        .await?,
                );
            }
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(decision)
        })
        .await
    }

    async fn reduce_app_recipe_step_started(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        claim_owner_id: &str,
        claim_epoch: u64,
        now_ms: i64,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let now_ms = Utc::now().timestamp_millis().max(now_ms);
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Ok(
                    if matches!(execution.state.status.as_str(), "cancelled" | "canceled") {
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    },
                );
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let step = schedule
                .steps
                .iter_mut()
                .find(|step| step.step_id == step_id)
                .ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_missing".to_owned())
                })?;
            let recipe = step.recipe.as_mut().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
            })?;
            let was_started = recipe.phase == AppRecipeScheduleNodePhase::Started;
            let active_deadline_at_ms = recipe.active_deadline_at_ms.ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_deadline_missing".to_owned())
            })?;
            let stale_claim = recipe.claim_owner_id.as_deref() != Some(claim_owner_id)
                || recipe.claim_epoch != claim_epoch
                || recipe
                    .claim_lease_expires_at_ms
                    .is_none_or(|deadline| deadline <= now_ms);
            let decision = if active_deadline_at_ms <= now_ms {
                recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                step.taskplan_status = "outcome_uncertain".to_owned();
                AppRecipeStepReducerAdmission::OutcomeUncertain
            } else if recipe.cancellation_requested_at.is_some() {
                recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                step.taskplan_status = "cancelled".to_owned();
                AppRecipeStepReducerAdmission::Cancelled
            } else if stale_claim {
                if recipe
                    .claim_lease_expires_at_ms
                    .is_some_and(|deadline| deadline <= now_ms)
                {
                    recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                    step.taskplan_status = "outcome_uncertain".to_owned();
                    AppRecipeStepReducerAdmission::OutcomeUncertain
                } else {
                    AppRecipeStepReducerAdmission::OwnedByLiveWorker
                }
            } else {
                match recipe.phase {
                    AppRecipeScheduleNodePhase::DispatchReserved => {
                        recipe.phase = AppRecipeScheduleNodePhase::Started;
                        step.taskplan_status = "running".to_owned();
                        execution.state.current_step_id = Some(step_id.to_owned());
                        execution.state.updated_at = updated_at.to_owned();
                        task.state.last_progress_at = Some(updated_at.to_owned());
                        task.state.updated_at = updated_at.to_owned();
                        AppRecipeStepReducerAdmission::Reserved {
                            claim_epoch,
                            deadline_at_ms: active_deadline_at_ms,
                        }
                    },
                    AppRecipeScheduleNodePhase::Started => {
                        AppRecipeStepReducerAdmission::ResumeStarted {
                            claim_epoch,
                            deadline_at_ms: active_deadline_at_ms,
                        }
                    },
                    AppRecipeScheduleNodePhase::Completed => {
                        AppRecipeStepReducerAdmission::AlreadyCompleted
                    },
                    AppRecipeScheduleNodePhase::Cancelled | AppRecipeScheduleNodePhase::Skipped => {
                        AppRecipeStepReducerAdmission::Cancelled
                    },
                    AppRecipeScheduleNodePhase::Failed
                    | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    },
                    AppRecipeScheduleNodePhase::Pending => {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_step_not_reserved".to_owned(),
                        ));
                    },
                }
            };
            schedule.updated_at = updated_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            if matches!(decision, AppRecipeStepReducerAdmission::Reserved { .. })
                || (was_started
                    && matches!(decision, AppRecipeStepReducerAdmission::OutcomeUncertain))
            {
                if was_started
                    && matches!(decision, AppRecipeStepReducerAdmission::OutcomeUncertain)
                    && execution.state.current_step_id.as_deref() == Some(step_id)
                {
                    execution.state.current_step_id = None;
                    execution.state.updated_at = updated_at.to_owned();
                    task.state.last_progress_at = Some(updated_at.to_owned());
                    task.state.updated_at = updated_at.to_owned();
                }
                writes.extend(self.execution_record_writes(scope, &execution)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, task_id, &[&execution])
                        .await?,
                );
            }
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(decision)
        })
        .await
    }

    async fn reduce_app_recipe_step_claim_renewed(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        claim_owner_id: &str,
        claim_epoch: u64,
        now_ms: i64,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error> {
        if now_ms <= 0 || claim_owner_id.is_empty() || claim_epoch == 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_claim_renewal_invalid".to_owned(),
            ));
        }
        // Observe liveness before waiting behind this task's effect/receipt
        // transaction. That transaction can outlast the short recovery lease.
        // The exact owner and epoch are still checked under the lock below, so
        // a reclaim/cancel that wins the lock cannot be undone by this waiter.
        let renewal_requested_at_ms = Utc::now().timestamp_millis().max(now_ms);
        self.with_task_write_lock(scope, task_id, || async {
            let now_ms = Utc::now().timestamp_millis().max(renewal_requested_at_ms);
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Ok(
                    if matches!(execution.state.status.as_str(), "cancelled" | "canceled") {
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    },
                );
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let step = schedule
                .steps
                .iter_mut()
                .find(|step| step.step_id == step_id)
                .ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_missing".to_owned())
                })?;
            let recipe = step.recipe.as_mut().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
            })?;
            if recipe.binding_digest != binding_digest
                || recipe.input_digest.as_deref() != Some(input_digest)
                || recipe.claim_owner_id.as_deref() != Some(claim_owner_id)
                || recipe.claim_epoch != claim_epoch
            {
                return Ok(AppRecipeStepReducerAdmission::OwnedByLiveWorker);
            }
            let active_deadline_at_ms = recipe.active_deadline_at_ms.ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_deadline_missing".to_owned())
            })?;
            let decision = match recipe.phase {
                AppRecipeScheduleNodePhase::Started => {
                    let claim_lease_expires_at_ms =
                        recipe.claim_lease_expires_at_ms.ok_or_else(|| {
                            ArtifactV2Error::InvalidRequest("app_recipe_claim_missing".to_owned())
                        })?;
                    if now_ms >= active_deadline_at_ms
                        || !app_recipe_claim_lease_is_live(
                            claim_lease_expires_at_ms,
                            renewal_requested_at_ms,
                        )
                    {
                        recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                        step.taskplan_status = "outcome_uncertain".to_owned();
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    } else if recipe.cancellation_requested_at.is_some() {
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        recipe.claim_lease_expires_at_ms = Some(
                            now_ms
                                .checked_add(APP_RECIPE_CLAIM_LEASE_MILLIS)
                                .ok_or_else(|| {
                                    ArtifactV2Error::InvalidRequest(
                                        "app_recipe_claim_lease_overflow".to_owned(),
                                    )
                                })?
                                .min(active_deadline_at_ms),
                        );
                        AppRecipeStepReducerAdmission::Reserved {
                            claim_epoch,
                            deadline_at_ms: active_deadline_at_ms,
                        }
                    }
                },
                AppRecipeScheduleNodePhase::Completed => {
                    AppRecipeStepReducerAdmission::AlreadyCompleted
                },
                AppRecipeScheduleNodePhase::Cancelled | AppRecipeScheduleNodePhase::Skipped => {
                    AppRecipeStepReducerAdmission::Cancelled
                },
                AppRecipeScheduleNodePhase::Failed
                | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                    AppRecipeStepReducerAdmission::OutcomeUncertain
                },
                AppRecipeScheduleNodePhase::Pending
                | AppRecipeScheduleNodePhase::DispatchReserved => {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "app_recipe_claim_not_started".to_owned(),
                    ));
                },
            };
            let transitioned_uncertain =
                matches!(decision, AppRecipeStepReducerAdmission::OutcomeUncertain)
                    && recipe.phase == AppRecipeScheduleNodePhase::OutcomeUncertain;
            schedule.updated_at = updated_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            if transitioned_uncertain {
                if execution.state.current_step_id.as_deref() == Some(step_id) {
                    execution.state.current_step_id = None;
                }
                execution.state.updated_at = updated_at.to_owned();
                task.state.last_progress_at = Some(updated_at.to_owned());
                task.state.updated_at = updated_at.to_owned();
                writes.extend(self.execution_record_writes(scope, &execution)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, task_id, &[&execution])
                        .await?,
                );
            }
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(decision)
        })
        .await
    }

    async fn reduce_app_recipe_step_completed(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        attempt: u16,
        claim_owner_id: &str,
        claim_epoch: u64,
        output_persisted_at_ms: i64,
        completed_at_ms: i64,
        output: &OutputRef,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error> {
        if output_persisted_at_ms <= 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_output_persistence_time_invalid".to_owned(),
            ));
        }
        self.with_task_write_lock(scope, task_id, || async {
            let completed_at_ms = Utc::now().timestamp_millis().max(completed_at_ms);
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Ok(
                    if matches!(execution.state.status.as_str(), "cancelled" | "canceled") {
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    },
                );
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let step = schedule
                .steps
                .iter_mut()
                .find(|step| step.step_id == step_id)
                .ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_missing".to_owned())
                })?;
            let recipe = step.recipe.as_mut().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
            })?;
            if recipe.binding_digest != binding_digest
                || recipe.input_digest.as_deref() != Some(input_digest)
                || recipe.attempt != attempt
                || recipe.claim_owner_id.as_deref() != Some(claim_owner_id)
                || recipe.claim_epoch != claim_epoch
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_step_permit_substitution".to_owned(),
                ));
            }
            let was_started = recipe.phase == AppRecipeScheduleNodePhase::Started;
            let decision = match recipe.phase {
                AppRecipeScheduleNodePhase::Completed => {
                    let retained = recipe.output.as_ref().ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest(
                            "app_recipe_completed_output_missing".to_owned(),
                        )
                    })?;
                    // The lifecycle evidence owns the value identity, while
                    // `created_at` is schedule-local metadata sampled by the
                    // original reducer winner. Recovery must therefore accept
                    // the same sealed output without requiring it to recreate
                    // that incidental timestamp.
                    if retained.output_id != output.output_id
                        || retained.scope != output.scope
                        || retained.audience != output.audience
                        || retained.role != output.role
                        || retained.relative_path != output.relative_path
                        || retained.media_type != output.media_type
                        || retained.source_execution_id != output.source_execution_id
                        || retained.source_plan_id != output.source_plan_id
                        || retained.source_output_ids != output.source_output_ids
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_step_output_substitution".to_owned(),
                        ));
                    }
                    AppRecipeStepReducerAdmission::AlreadyCompleted
                },
                AppRecipeScheduleNodePhase::Started => {
                    let claim_lease_expires_at_ms =
                        recipe.claim_lease_expires_at_ms.ok_or_else(|| {
                            ArtifactV2Error::InvalidRequest("app_recipe_claim_missing".to_owned())
                        })?;
                    let cancellation_missed_ack = recipe.cancellation_requested_at.is_some()
                        && recipe
                            .cancellation_ack_deadline_at_ms
                            .is_none_or(|deadline| completed_at_ms >= deadline);
                    if cancellation_missed_ack
                        || output_persisted_at_ms > completed_at_ms
                        || !app_recipe_claim_lease_is_live(
                            claim_lease_expires_at_ms,
                            output_persisted_at_ms,
                        )
                        || !app_recipe_claim_lease_is_live(
                            claim_lease_expires_at_ms,
                            completed_at_ms,
                        )
                        || recipe.active_deadline_at_ms.is_none_or(|deadline| {
                            output_persisted_at_ms >= deadline || completed_at_ms >= deadline
                        })
                    {
                        recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                        step.taskplan_status = "outcome_uncertain".to_owned();
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    } else if recipe.cancellation_requested_at.is_some() {
                        // The worker acknowledged the persisted cancellation
                        // before its reviewed deadline. Do not publish the
                        // raced output: the canonical cancellation owner will
                        // settle the root from this durable node fact.
                        recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                        step.taskplan_status = "cancelled".to_owned();
                        recipe.claim_lease_expires_at_ms = None;
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        recipe.output = Some(output.clone());
                        recipe.phase = AppRecipeScheduleNodePhase::Completed;
                        step.taskplan_status = "completed".to_owned();
                        step.taskplan_progress = "1/1".to_owned();
                        if !execution
                            .state
                            .completed_step_ids
                            .iter()
                            .any(|id| id == step_id)
                        {
                            execution.state.completed_step_ids.push(step_id.to_owned());
                        }
                        if execution.state.current_step_id.as_deref() == Some(step_id) {
                            execution.state.current_step_id = None;
                        }
                        execution.state.updated_at = updated_at.to_owned();
                        task.state.last_progress_at = Some(updated_at.to_owned());
                        task.state.updated_at = updated_at.to_owned();
                        recipe.claim_lease_expires_at_ms = None;
                        AppRecipeStepReducerAdmission::Reserved {
                            claim_epoch,
                            deadline_at_ms: recipe.active_deadline_at_ms.unwrap_or(completed_at_ms),
                        }
                    }
                },
                AppRecipeScheduleNodePhase::Cancelled | AppRecipeScheduleNodePhase::Skipped => {
                    AppRecipeStepReducerAdmission::Cancelled
                },
                AppRecipeScheduleNodePhase::Failed
                | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                    AppRecipeStepReducerAdmission::OutcomeUncertain
                },
                AppRecipeScheduleNodePhase::Pending
                | AppRecipeScheduleNodePhase::DispatchReserved => {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "app_recipe_step_not_started".to_owned(),
                    ));
                },
            };
            if was_started {
                if execution.state.current_step_id.as_deref() == Some(step_id) {
                    execution.state.current_step_id = None;
                }
                execution.state.updated_at = updated_at.to_owned();
                task.state.last_progress_at = Some(updated_at.to_owned());
                task.state.updated_at = updated_at.to_owned();
            }
            schedule.updated_at = updated_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            if was_started {
                writes.extend(self.execution_record_writes(scope, &execution)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, task_id, &[&execution])
                        .await?,
                );
            }
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(decision)
        })
        .await
    }

    async fn reduce_app_recipe_step_output_adopted(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_id: &str,
        binding_digest: &str,
        input_digest: &str,
        output_claim_owner_id: &str,
        output_claim_epoch: u64,
        output_persisted_at_ms: i64,
        adopted_at_ms: i64,
        output: &OutputRef,
        updated_at: &str,
    ) -> Result<AppRecipeStepReducerAdmission, ArtifactV2Error> {
        if output_claim_owner_id.is_empty()
            || output_claim_epoch == 0
            || output_persisted_at_ms <= 0
            || adopted_at_ms <= 0
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_output_adoption_time_invalid".to_owned(),
            ));
        }
        self.with_task_write_lock(scope, task_id, || async {
            let adopted_at_ms = Utc::now().timestamp_millis().max(adopted_at_ms);
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Ok(
                    if matches!(execution.state.status.as_str(), "cancelled" | "canceled") {
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    },
                );
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let step = schedule
                .steps
                .iter_mut()
                .find(|step| step.step_id == step_id)
                .ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_missing".to_owned())
                })?;
            let recipe = step.recipe.as_mut().ok_or_else(|| {
                ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
            })?;
            if recipe.binding_digest != binding_digest
                || recipe.input_digest.as_deref() != Some(input_digest)
                || recipe.attempt != 1
                || recipe.claim_owner_id.as_deref() != Some(output_claim_owner_id)
                || recipe.claim_epoch != output_claim_epoch
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_output_adoption_substitution".to_owned(),
                ));
            }
            let decision = match recipe.phase {
                AppRecipeScheduleNodePhase::Completed => {
                    let retained = recipe.output.as_ref().ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest(
                            "app_recipe_completed_output_missing".to_owned(),
                        )
                    })?;
                    // `created_at` is schedule-local metadata sampled by the
                    // original reducer winner. Recovery proves the sealed
                    // value/path/source identity without attempting to
                    // recreate that incidental timestamp.
                    if retained.output_id != output.output_id
                        || retained.scope != output.scope
                        || retained.audience != output.audience
                        || retained.role != output.role
                        || retained.relative_path != output.relative_path
                        || retained.media_type != output.media_type
                        || retained.source_execution_id != output.source_execution_id
                        || retained.source_plan_id != output.source_plan_id
                        || retained.source_output_ids != output.source_output_ids
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_step_output_substitution".to_owned(),
                        ));
                    }
                    AppRecipeStepReducerAdmission::AlreadyCompleted
                },
                AppRecipeScheduleNodePhase::Started => {
                    let lease_deadline = recipe.claim_lease_expires_at_ms.ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest("app_recipe_claim_missing".to_owned())
                    })?;
                    let active_deadline = recipe.active_deadline_at_ms.ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest("app_recipe_deadline_missing".to_owned())
                    })?;
                    if lease_deadline > adopted_at_ms {
                        AppRecipeStepReducerAdmission::OwnedByLiveWorker
                    } else if adopted_at_ms > active_deadline
                        || output_persisted_at_ms > active_deadline
                    {
                        recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                        step.taskplan_status = "outcome_uncertain".to_owned();
                        AppRecipeStepReducerAdmission::OutcomeUncertain
                    } else if recipe.cancellation_requested_at.is_some() {
                        recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                        step.taskplan_status = "cancelled".to_owned();
                        AppRecipeStepReducerAdmission::Cancelled
                    } else {
                        recipe.output = Some(output.clone());
                        recipe.phase = AppRecipeScheduleNodePhase::Completed;
                        recipe.claim_lease_expires_at_ms = None;
                        step.taskplan_status = "completed".to_owned();
                        step.taskplan_progress = "1/1".to_owned();
                        if !execution
                            .state
                            .completed_step_ids
                            .iter()
                            .any(|id| id == step_id)
                        {
                            execution.state.completed_step_ids.push(step_id.to_owned());
                        }
                        AppRecipeStepReducerAdmission::Reserved {
                            claim_epoch: recipe.claim_epoch,
                            deadline_at_ms: active_deadline,
                        }
                    }
                },
                AppRecipeScheduleNodePhase::Cancelled | AppRecipeScheduleNodePhase::Skipped => {
                    AppRecipeStepReducerAdmission::Cancelled
                },
                AppRecipeScheduleNodePhase::Failed
                | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                    AppRecipeStepReducerAdmission::OutcomeUncertain
                },
                AppRecipeScheduleNodePhase::Pending
                | AppRecipeScheduleNodePhase::DispatchReserved => {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "app_recipe_output_without_started_owner".to_owned(),
                    ));
                },
            };
            if !matches!(
                decision,
                AppRecipeStepReducerAdmission::AlreadyCompleted
                    | AppRecipeStepReducerAdmission::OwnedByLiveWorker
            ) {
                if execution.state.current_step_id.as_deref() == Some(step_id) {
                    execution.state.current_step_id = None;
                }
                execution.state.updated_at = updated_at.to_owned();
                task.state.last_progress_at = Some(updated_at.to_owned());
                task.state.updated_at = updated_at.to_owned();
            }
            schedule.updated_at = updated_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            if !matches!(
                decision,
                AppRecipeStepReducerAdmission::AlreadyCompleted
                    | AppRecipeStepReducerAdmission::OwnedByLiveWorker
            ) {
                writes.extend(self.execution_record_writes(scope, &execution)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, task_id, &[&execution])
                        .await?,
                );
            }
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(decision)
        })
        .await
    }

    async fn reduce_app_recipe_steps_skipped(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        step_ids: &[String],
        updated_at: &str,
    ) -> Result<Vec<String>, ArtifactV2Error> {
        if step_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.with_task_write_lock(scope, task_id, || async {
            let execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_root_not_running".to_owned(),
                ));
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let requested = step_ids.iter().collect::<std::collections::BTreeSet<_>>();
            if requested.len() != step_ids.len() {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_skip_set_invalid".to_owned(),
                ));
            }
            let mut transitioned = Vec::new();
            for step_id in step_ids {
                let step = schedule
                    .steps
                    .iter_mut()
                    .find(|step| step.step_id == *step_id)
                    .ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest("app_recipe_step_missing".to_owned())
                    })?;
                let recipe = step.recipe.as_mut().ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
                })?;
                match recipe.phase {
                    AppRecipeScheduleNodePhase::Pending => {
                        recipe.phase = AppRecipeScheduleNodePhase::Skipped;
                        step.taskplan_status = "skipped".to_owned();
                        step.taskplan_progress = "0/1".to_owned();
                        transitioned.push(step_id.clone());
                    },
                    AppRecipeScheduleNodePhase::Skipped => {},
                    _ => {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_skip_after_dispatch".to_owned(),
                        ));
                    },
                }
            }
            schedule.updated_at = updated_at.to_owned();
            self.commit_task_writes(
                scope,
                task_id,
                &[(path, Self::serialize_json_pretty(&schedule)?)],
            )
            .await?;
            Ok(transitioned)
        })
        .await
    }

    async fn reduce_app_recipe_cancellation_requested(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        requested_at: &str,
    ) -> Result<AppRecipeCancellationReducerAdmission, ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let task = self.load_task_record(scope, task_id).await?;
            let execution = self.load_execution_record(scope, task_id, execution_id).await?;
            if task.state.status != "running"
                || task.state.active_root_execution_id.as_deref() != Some(execution_id)
                || execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_root_not_running".to_owned(),
                ));
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord = self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let root_phase = schedule
                .steps
                .iter()
                .find(|step| step.order == 0)
                .and_then(|step| step.recipe.as_ref())
                .map(|recipe| recipe.phase)
                .ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest(
                        "app_recipe_root_step_missing".to_owned(),
                    )
                })?;
            if matches!(
                root_phase,
                AppRecipeScheduleNodePhase::Completed
                    | AppRecipeScheduleNodePhase::Failed
                    | AppRecipeScheduleNodePhase::OutcomeUncertain
            ) {
                return Ok(AppRecipeCancellationReducerAdmission::AlreadySettled);
            }
            let mut has_started = false;
            let already_requested = schedule.steps.iter().all(|step| {
                step.recipe
                    .as_ref()
                    .is_some_and(|recipe| recipe.cancellation_requested_at.is_some())
            });
            if already_requested {
                let maximum_due_at_ms = schedule.steps.iter().try_fold(
                    None::<i64>,
                    |maximum, step| {
                        let recipe = step.recipe.as_ref().ok_or_else(|| {
                            ArtifactV2Error::InvalidRequest(
                                "app_recipe_step_binding_missing".to_owned(),
                            )
                        })?;
                        if recipe.phase != AppRecipeScheduleNodePhase::Started {
                            return Ok(maximum);
                        }
                        let deadline = recipe.cancellation_ack_deadline_at_ms.ok_or_else(|| {
                            ArtifactV2Error::InvalidRequest(
                                "app_recipe_cancellation_deadline_missing".to_owned(),
                            )
                        })?;
                        Ok::<_, ArtifactV2Error>(Some(
                            maximum.map_or(deadline, |current| current.max(deadline)),
                        ))
                    },
                )?;
                return Ok(if let Some(deadline_at_ms) = maximum_due_at_ms {
                    AppRecipeCancellationReducerAdmission::AlreadyRequestedAwaitingStartedSettlement {
                        deadline_at_ms,
                    }
                } else {
                    AppRecipeCancellationReducerAdmission::AlreadyRequestedBeforeDispatch
                });
            }
            let requested_at_ms = chrono::DateTime::parse_from_rfc3339(requested_at)
                .map_err(|_| ArtifactV2Error::InvalidRequest(
                    "app_recipe_cancellation_time_invalid".to_owned(),
                ))?
                .timestamp_millis();
            let mut maximum_due_at_ms = None::<i64>;
            for step in &mut schedule.steps {
                let recipe = step.recipe.as_mut().ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
                })?;
                if recipe.cancellation_requested_at.is_none() {
                    recipe.cancellation_requested_at = Some(requested_at.to_owned());
                }
                match recipe.phase {
                    AppRecipeScheduleNodePhase::Started => {
                        has_started = true;
                        let deadline_at_ms = requested_at_ms
                                .checked_add(i64::try_from(
                                    recipe.cancellation_acknowledgement_timeout_millis,
                                ).map_err(|_| ArtifactV2Error::InvalidRequest(
                                    "app_recipe_cancellation_deadline_overflow".to_owned(),
                                ))?)
                                .ok_or_else(|| ArtifactV2Error::InvalidRequest(
                                    "app_recipe_cancellation_deadline_overflow".to_owned(),
                                ))?;
                        recipe.cancellation_ack_deadline_at_ms = Some(deadline_at_ms);
                        maximum_due_at_ms = Some(
                            maximum_due_at_ms
                                .map_or(deadline_at_ms, |current| current.max(deadline_at_ms)),
                        );
                    },
                    AppRecipeScheduleNodePhase::Pending
                    | AppRecipeScheduleNodePhase::DispatchReserved => {
                        recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                        step.taskplan_status = "cancelled".to_owned();
                    },
                    _ => {},
                }
            }
            schedule.updated_at = requested_at.to_owned();
            self.commit_task_writes(
                scope,
                task_id,
                &[(path, Self::serialize_json_pretty(&schedule)?)],
            )
            .await?;
            Ok(if has_started {
                AppRecipeCancellationReducerAdmission::AwaitingStartedSettlement {
                    deadline_at_ms: maximum_due_at_ms.ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest(
                            "app_recipe_cancellation_deadline_missing".to_owned(),
                        )
                    })?,
                }
            } else {
                AppRecipeCancellationReducerAdmission::RequestedBeforeDispatch
            })
        })
        .await
    }

    async fn reduce_app_recipe_cancellation_settled(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        settled_at: &str,
    ) -> Result<bool, ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_root_not_running".to_owned(),
                ));
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
                || schedule.steps.iter().any(|step| {
                    step.recipe
                        .as_ref()
                        .is_none_or(|recipe| recipe.cancellation_requested_at.is_none())
                })
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_cancellation_not_requested".to_owned(),
                ));
            }
            let settled_at_ms = chrono::DateTime::parse_from_rfc3339(settled_at)
                .map_err(|_| {
                    ArtifactV2Error::InvalidRequest(
                        "app_recipe_cancellation_time_invalid".to_owned(),
                    )
                })?
                .timestamp_millis()
                .max(Utc::now().timestamp_millis());
            let mut outcome_uncertain = false;
            for step in &mut schedule.steps {
                let recipe = step.recipe.as_mut().ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
                })?;
                match recipe.phase {
                    AppRecipeScheduleNodePhase::Completed
                    | AppRecipeScheduleNodePhase::Skipped
                    | AppRecipeScheduleNodePhase::Cancelled => {},
                    AppRecipeScheduleNodePhase::Pending
                    | AppRecipeScheduleNodePhase::DispatchReserved
                    | AppRecipeScheduleNodePhase::Started => {
                        if recipe.phase == AppRecipeScheduleNodePhase::Started
                            && recipe
                                .cancellation_ack_deadline_at_ms
                                .is_none_or(|deadline| settled_at_ms >= deadline)
                        {
                            recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                            step.taskplan_status = "outcome_uncertain".to_owned();
                            outcome_uncertain = true;
                        } else {
                            recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                            step.taskplan_status = "cancelled".to_owned();
                        }
                    },
                    AppRecipeScheduleNodePhase::Failed
                    | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "app_recipe_cancellation_outcome_uncertain".to_owned(),
                        ));
                    },
                }
            }
            schedule.updated_at = settled_at.to_owned();
            execution.state.current_step_id = None;
            execution.state.updated_at = settled_at.to_owned();
            task.state.last_progress_at = Some(settled_at.to_owned());
            task.state.updated_at = settled_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            writes.extend(self.execution_record_writes(scope, &execution)?);
            writes.extend(self.task_state_and_refs_writes(&task)?);
            writes.push(
                self.execution_index_write(scope, task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(outcome_uncertain)
        })
        .await
    }

    async fn reduce_app_recipe_cancellation_due(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        due_at_ms: i64,
        settled_at: &str,
    ) -> Result<AppRecipeCancellationDueAdmission, ArtifactV2Error> {
        if due_at_ms <= 0 {
            return Err(ArtifactV2Error::InvalidRequest(
                "app_recipe_cancellation_due_invalid".to_owned(),
            ));
        }
        self.with_task_write_lock(scope, task_id, || async {
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running" {
                return Ok(AppRecipeCancellationDueAdmission::AlreadyTerminal);
            }
            if execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_root_not_running".to_owned(),
                ));
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
                || schedule.steps.iter().any(|step| {
                    step.recipe
                        .as_ref()
                        .is_none_or(|recipe| recipe.cancellation_requested_at.is_none())
                })
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_cancellation_not_requested".to_owned(),
                ));
            }
            // `due_at_ms` selects the durable due item; it is not evidence
            // that wall time has reached it. A duplicated or prematurely
            // delivered timer must never force an early cancellation
            // settlement.
            let now_ms = Utc::now().timestamp_millis();
            let mut maximum_due_at_ms = None::<i64>;
            for step in &schedule.steps {
                let recipe = step.recipe.as_ref().ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
                })?;
                if recipe.phase == AppRecipeScheduleNodePhase::Started {
                    let deadline = recipe.cancellation_ack_deadline_at_ms.ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest(
                            "app_recipe_cancellation_deadline_missing".to_owned(),
                        )
                    })?;
                    maximum_due_at_ms =
                        Some(maximum_due_at_ms.map_or(deadline, |current| current.max(deadline)));
                }
            }
            if maximum_due_at_ms.is_some_and(|deadline| now_ms < deadline) {
                return Ok(AppRecipeCancellationDueAdmission::NotDue {
                    deadline_at_ms: maximum_due_at_ms.unwrap_or(now_ms),
                });
            }
            if maximum_due_at_ms.is_none()
                && schedule.steps.iter().all(|step| {
                    step.recipe.as_ref().is_some_and(|recipe| {
                        matches!(
                            recipe.phase,
                            AppRecipeScheduleNodePhase::Completed
                                | AppRecipeScheduleNodePhase::Failed
                                | AppRecipeScheduleNodePhase::Cancelled
                                | AppRecipeScheduleNodePhase::Skipped
                                | AppRecipeScheduleNodePhase::OutcomeUncertain
                        )
                    })
                })
            {
                // Another timer/recovery worker already won the reducer
                // transition. Return only the retained terminal class so this
                // caller may idempotently close a root/event projection gap;
                // it receives no new node-transition authority.
                return Ok(AppRecipeCancellationDueAdmission::ReconcileTerminal {
                    outcome_uncertain: schedule.steps.iter().any(|step| {
                        step.recipe.as_ref().is_some_and(|recipe| {
                            matches!(
                                recipe.phase,
                                AppRecipeScheduleNodePhase::Failed
                                    | AppRecipeScheduleNodePhase::OutcomeUncertain
                            )
                        })
                    }),
                });
            }
            let mut outcome_uncertain = false;
            for step in &mut schedule.steps {
                let recipe = step.recipe.as_mut().ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
                })?;
                match recipe.phase {
                    AppRecipeScheduleNodePhase::Started => {
                        recipe.phase = AppRecipeScheduleNodePhase::OutcomeUncertain;
                        recipe.claim_lease_expires_at_ms = None;
                        step.taskplan_status = "outcome_uncertain".to_owned();
                        outcome_uncertain = true;
                    },
                    AppRecipeScheduleNodePhase::Pending
                    | AppRecipeScheduleNodePhase::DispatchReserved => {
                        recipe.phase = AppRecipeScheduleNodePhase::Cancelled;
                        recipe.claim_lease_expires_at_ms = None;
                        step.taskplan_status = "cancelled".to_owned();
                    },
                    AppRecipeScheduleNodePhase::Failed
                    | AppRecipeScheduleNodePhase::OutcomeUncertain => {
                        outcome_uncertain = true;
                    },
                    AppRecipeScheduleNodePhase::Completed
                    | AppRecipeScheduleNodePhase::Cancelled
                    | AppRecipeScheduleNodePhase::Skipped => {},
                }
            }
            schedule.updated_at = settled_at.to_owned();
            execution.state.current_step_id = None;
            execution.state.updated_at = settled_at.to_owned();
            task.state.last_progress_at = Some(settled_at.to_owned());
            task.state.updated_at = settled_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            writes.extend(self.execution_record_writes(scope, &execution)?);
            writes.extend(self.task_state_and_refs_writes(&task)?);
            writes.push(
                self.execution_index_write(scope, task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(if outcome_uncertain {
                AppRecipeCancellationDueAdmission::OutcomeUncertain
            } else {
                AppRecipeCancellationDueAdmission::Cancelled
            })
        })
        .await
    }

    async fn reduce_app_recipe_execution_failed(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        outcome_uncertain: bool,
        failed_at: &str,
    ) -> Result<Vec<String>, ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let mut task = self.load_task_record(scope, task_id).await?;
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            if execution.state.status != "running"
                || execution.state.parent_execution_id.is_some()
                || execution.state.root_execution_id.as_deref() != Some(execution_id)
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_root_not_running".to_owned(),
                ));
            }
            let path = self.workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            );
            let mut schedule: ExecutionScheduleRecord =
                self.workspace.read_json_path(&path).await?;
            if schedule.source_kind != "app_recipe_v1"
                || schedule.task_id != task_id
                || schedule.execution_id != execution_id
            {
                return Err(ArtifactV2Error::InvalidRequest(
                    "app_recipe_schedule_substitution".to_owned(),
                ));
            }
            let mut failed_steps = Vec::new();
            for step in &mut schedule.steps {
                let recipe = step.recipe.as_mut().ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest("app_recipe_step_binding_missing".to_owned())
                })?;
                match recipe.phase {
                    AppRecipeScheduleNodePhase::Started
                    | AppRecipeScheduleNodePhase::DispatchReserved => {
                        recipe.phase = if outcome_uncertain {
                            AppRecipeScheduleNodePhase::OutcomeUncertain
                        } else {
                            AppRecipeScheduleNodePhase::Failed
                        };
                        step.taskplan_status = if outcome_uncertain {
                            "outcome_uncertain".to_owned()
                        } else {
                            "failed".to_owned()
                        };
                        failed_steps.push(step.step_id.clone());
                    },
                    AppRecipeScheduleNodePhase::Pending => {
                        if step.order == 0 {
                            recipe.phase = if outcome_uncertain {
                                AppRecipeScheduleNodePhase::OutcomeUncertain
                            } else {
                                AppRecipeScheduleNodePhase::Failed
                            };
                            step.taskplan_status = if outcome_uncertain {
                                "outcome_uncertain".to_owned()
                            } else {
                                "failed".to_owned()
                            };
                            failed_steps.push(step.step_id.clone());
                        } else {
                            recipe.phase = AppRecipeScheduleNodePhase::Skipped;
                            step.taskplan_status = "skipped".to_owned();
                        }
                    },
                    AppRecipeScheduleNodePhase::Completed
                    | AppRecipeScheduleNodePhase::Failed
                    | AppRecipeScheduleNodePhase::Cancelled
                    | AppRecipeScheduleNodePhase::Skipped
                    | AppRecipeScheduleNodePhase::OutcomeUncertain => {},
                }
            }
            schedule.updated_at = failed_at.to_owned();
            for step_id in &failed_steps {
                if !execution
                    .state
                    .failed_step_ids
                    .iter()
                    .any(|id| id == step_id)
                {
                    execution.state.failed_step_ids.push(step_id.clone());
                }
                if execution.state.current_step_id.as_deref() == Some(step_id) {
                    execution.state.current_step_id = None;
                }
            }
            execution.state.current_step_id = None;
            execution.state.updated_at = failed_at.to_owned();
            task.state.last_progress_at = Some(failed_at.to_owned());
            task.state.updated_at = failed_at.to_owned();
            let mut writes = vec![(path, Self::serialize_json_pretty(&schedule)?)];
            writes.extend(self.execution_record_writes(scope, &execution)?);
            writes.extend(self.task_state_and_refs_writes(&task)?);
            writes.push(
                self.execution_index_write(scope, task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await?;
            Ok(failed_steps)
        })
        .await
    }

    async fn reduce_task_created(&self, task: &TaskRecord) -> Result<(), ArtifactV2Error> {
        let scope = Self::task_scope(task);
        self.with_task_write_lock(&scope, &task.manifest.task_id, || async {
            self.commit_task_writes(
                &scope,
                &task.manifest.task_id,
                &self.task_record_writes(task)?,
            )
            .await
        })
        .await
    }

    async fn reduce_execution_initialized(
        &self,
        task: &mut TaskRecord,
        execution: &ExecutionRecord,
    ) -> Result<(), ArtifactV2Error> {
        let is_root_execution = Self::is_root_execution(execution);
        if !is_root_execution {
            let scope = Self::task_scope(task);
            self.commit_child_execution_with_root_repair(
                &scope,
                &task.manifest.task_id,
                execution,
                None,
                |_| {},
            )
            .await?;
            return Ok(());
        }
        let scope = Self::task_scope(task);
        let task_id = task.manifest.task_id.clone();
        let requested_schedule_fire_count = task.state.schedule_fire_count;
        let fresh = self
            .with_task_write_lock(&scope, &task_id, || async {
                // The caller's task is an admission snapshot, not commit authority.
                // Re-read while holding the cross-process task lock so cancellation
                // or a competing root which committed after that snapshot cannot be
                // overwritten by this reducer.
                let mut fresh = self.load_task_record(&scope, &task_id).await?;
                if matches!(fresh.state.status.as_str(), "cancelled" | "canceled") {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "task_cancelled:{task_id}"
                    )));
                }
                if let Some(active_execution_id) = fresh.state.active_root_execution_id.as_deref() {
                    return Err(ArtifactV2Error::InvalidRequest(format!(
                        "task_execution_in_progress:{active_execution_id}"
                    )));
                }
                fresh.state.status = "running".to_string();
                fresh.state.active_root_execution_id = Some(execution.state.execution_id.clone());
                fresh.state.latest_root_execution_id = Some(execution.state.execution_id.clone());
                // Scheduled admission increments the caller snapshot immediately
                // before this reducer. Merge only that monotonic counter while all
                // other task fields come from the fresh locked record.
                fresh.state.schedule_fire_count = fresh
                    .state
                    .schedule_fire_count
                    .max(requested_schedule_fire_count);
                // A step started: initialization emits
                // `StepStarted{step_execute_task}`. Stamping progress here is what
                // lets a run wedged inside its first step be reported as stalled.
                fresh.state.last_progress_at = Some(execution.state.updated_at.clone());
                fresh.state.updated_at = execution.state.updated_at.clone();
                // reduce_execution_initialized only mutates task.state —
                // not manifest. Use the manifest-preserving helper so
                // concurrent manifest edits (rename, retag, repriority)
                // landing during execution start aren't clobbered.
                let mut writes = self.task_state_and_refs_writes(&fresh)?;
                writes.extend(self.execution_record_writes(&scope, execution)?);
                writes.push(
                    self.execution_index_write(&scope, &task_id, &[execution])
                        .await?,
                );
                self.commit_task_writes(&scope, &task_id, &writes).await?;
                Ok(fresh)
            })
            .await?;
        *task = fresh;
        Ok(())
    }

    async fn reduce_execution_discovered(
        &self,
        scope: &ScopeRef,
        execution: &ExecutionRecord,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, &execution.state.task_id, || async {
            let mut writes = self.execution_record_writes(scope, execution)?;
            writes.push(
                self.execution_index_write(scope, &execution.state.task_id, &[execution])
                    .await?,
            );
            self.commit_task_writes(scope, &execution.state.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_delegation_requested(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        parent_step_id: &str,
        sub_goal: &str,
        budget_iterations: usize,
        depth: usize,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            execution.state.updated_at = updated_at.to_string();
            Self::upsert_delegation_ref(
                &mut execution.refs.delegations,
                parent_step_id,
                sub_goal,
                None,
                |existing| {
                    let mut delegation = existing.unwrap_or_else(|| DelegationRef {
                        parent_step_id: parent_step_id.to_string(),
                        sub_goal: sub_goal.to_string(),
                        requested_at: updated_at.to_string(),
                        ..Default::default()
                    });
                    delegation.parent_step_id = parent_step_id.to_string();
                    delegation.sub_goal = sub_goal.to_string();
                    delegation.status = "requested".to_string();
                    delegation.budget_iterations = Some(budget_iterations);
                    delegation.depth = Some(depth);
                    delegation.updated_at = updated_at.to_string();
                    if delegation.requested_at.is_empty() {
                        delegation.requested_at = updated_at.to_string();
                    }
                    delegation
                },
            );
            execution.refs.updated_at = Some(updated_at.to_string());
            let mut writes = self.execution_record_writes(scope, &execution)?;
            writes.push(
                self.execution_index_write(scope, task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await
        })
        .await
    }

    async fn reduce_delegation_outcome(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
        parent_step_id: &str,
        sub_goal: &str,
        outcome_type: &str,
        iterations_used: usize,
        duration_ms: u64,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let mut execution = self
                .load_execution_record(scope, task_id, execution_id)
                .await?;
            execution.state.updated_at = updated_at.to_string();
            Self::upsert_delegation_ref(
                &mut execution.refs.delegations,
                parent_step_id,
                sub_goal,
                None,
                |existing| {
                    let mut delegation = existing.unwrap_or_else(|| DelegationRef {
                        parent_step_id: parent_step_id.to_string(),
                        sub_goal: sub_goal.to_string(),
                        requested_at: updated_at.to_string(),
                        ..Default::default()
                    });
                    delegation.parent_step_id = parent_step_id.to_string();
                    delegation.sub_goal = sub_goal.to_string();
                    delegation.status = if matches!(
                        outcome_type,
                        "success"
                            | "failed"
                            | "error"
                            | "cannot_proceed"
                            | "loop_detected"
                            | "max_iterations_reached"
                            | "budget_exhausted"
                    ) {
                        "terminal".to_string()
                    } else {
                        "blocked".to_string()
                    };
                    delegation.outcome_type = Some(outcome_type.to_string());
                    delegation.iterations_used = Some(iterations_used);
                    delegation.duration_ms = Some(duration_ms);
                    delegation.updated_at = updated_at.to_string();
                    if delegation.requested_at.is_empty() {
                        delegation.requested_at = updated_at.to_string();
                    }
                    delegation
                },
            );
            execution.refs.updated_at = Some(updated_at.to_string());
            let mut writes = self.execution_record_writes(scope, &execution)?;
            writes.push(
                self.execution_index_write(scope, task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await
        })
        .await
    }

    async fn reduce_delegated_child_link(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        parent_execution_id: &str,
        child_execution_id: &str,
        child_agent_id: &str,
        sub_goal: &str,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let mut parent = self
                .load_execution_record(scope, task_id, parent_execution_id)
                .await?;
            parent.state.updated_at = updated_at.to_string();
            Self::upsert_delegation_ref(
                &mut parent.refs.delegations,
                "",
                sub_goal,
                Some(child_execution_id),
                |existing| {
                    let mut delegation = existing.unwrap_or_else(|| DelegationRef {
                        parent_step_id: String::new(),
                        sub_goal: sub_goal.to_string(),
                        requested_at: updated_at.to_string(),
                        ..Default::default()
                    });
                    if delegation.sub_goal.is_empty() {
                        delegation.sub_goal = sub_goal.to_string();
                    }
                    delegation.child_execution_id = Some(child_execution_id.to_string());
                    delegation.child_agent_id = Some(child_agent_id.to_string());
                    if delegation.status.is_empty() || delegation.status == "requested" {
                        delegation.status = "discovered".to_string();
                    }
                    delegation.updated_at = updated_at.to_string();
                    if delegation.requested_at.is_empty() {
                        delegation.requested_at = updated_at.to_string();
                    }
                    delegation
                },
            );
            parent.refs.updated_at = Some(updated_at.to_string());
            let mut writes = self.execution_record_writes(scope, &parent)?;
            writes.push(
                self.execution_index_write(scope, task_id, &[&parent])
                    .await?,
            );
            self.commit_task_writes(scope, task_id, &writes).await
        })
        .await
    }

    async fn reduce_execution_nonterminal(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error> {
        // ─── Defensive guard for the early-"completed" projection bug ───────
        //
        // A non-terminal outcome must never carry a terminal task_status.
        // If it does and we let it through, the projection adapter sees
        // `task.state.status = "completed"` and emits `task.status_changed:
        // completed` BEFORE `finalize_terminal_execution` synthesises any
        // outputs — chat fan-outs tear down on that signal and miss the
        // subsequent `output.available` events, leaving the user staring
        // at an "in-progress" activity card that never shows the result.
        //
        // The proper terminal write happens in `reduce_execution_terminal`
        // (which takes `&FinalizedOutputs` and runs AFTER synthesis).
        // Refuse the whole malformed write and capture a backtrace so
        // the upstream caller that assembled the bad snapshot can be
        // tracked down. Once that caller is fixed this guard becomes a
        // no-op.
        if matches!(
            outcome.task_status.as_str(),
            "completed" | "failed" | "cancelled" | "canceled"
        ) {
            let backtrace = Backtrace::force_capture();
            warn!(
                task_id = %task.manifest.task_id,
                execution_id = %execution.state.execution_id,
                outcome_task_status = %outcome.task_status,
                outcome_execution_status = %outcome.execution_status,
                outcome_outcome_type = %outcome.outcome_type,
                is_terminal = outcome.is_terminal,
                prior_task_status = %task.state.status,
                backtrace = ?backtrace,
                "[REDUCER] reduce_execution_nonterminal received a \
                 terminal-shaped task_status on a non-terminal outcome; \
                 refusing the whole write and keeping prior state. This \
                 is the early `task.status_changed: completed` bug — the \
                 upstream caller is assembling a malformed snapshot. \
                 Stack capture above. See SDA delegation investigation."
            );
            return Ok(());
        }
        execution.state.status = outcome.execution_status.clone();
        let is_root_execution = Self::is_root_execution(execution);
        if !is_root_execution {
            let scope = Self::task_scope(task);
            let status = outcome.execution_status.clone();
            let updated_at = execution.state.updated_at.clone();
            let committed = self
                .commit_child_execution_with_root_repair(
                    &scope,
                    &task.manifest.task_id,
                    execution,
                    Some(status.clone()),
                    move |fresh| {
                        fresh.state.status = status;
                        fresh.state.updated_at =
                            Self::latest_rfc3339(&fresh.state.updated_at, &updated_at);
                    },
                )
                .await?;
            *execution = committed;
            return Ok(());
        }
        task.state.status = outcome.task_status.clone();
        if outcome.completion_kind.is_some() {
            task.state.completion_kind = outcome.completion_kind;
            task.state.open_items = outcome.open_items.clone();
        }
        task.state.latest_root_execution_id = Some(execution.state.execution_id.clone());
        task.state.active_root_execution_id =
            Self::task_status_keeps_active_root(&outcome.task_status)
                .then(|| execution.state.execution_id.clone());
        task.state.updated_at = execution.state.updated_at.clone();

        let scope = Self::task_scope(task);
        self.with_task_write_lock(&scope, &task.manifest.task_id, || async {
            let mut writes = self.execution_record_writes(&scope, execution)?;
            writes.extend(self.task_state_and_refs_writes(task)?);
            writes.push(
                self.execution_index_write(&scope, &task.manifest.task_id, &[execution])
                    .await?,
            );
            self.commit_task_writes(&scope, &task.manifest.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_execution_terminal(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
        outputs: &FinalizedOutputs,
    ) -> Result<(), ArtifactV2Error> {
        execution.state.status = outcome.execution_status.clone();
        if outcome.completion_kind.is_some() {
            execution.state.completion_kind = outcome.completion_kind;
            execution.state.open_items = outcome.open_items.clone();
        }
        execution.state.primary_execution_output_id =
            Some(outputs.execution_output.output_id.clone());
        // Synthesis succeeded — clear the in-flight marker set by Step 1
        // (`reduce_execution_terminal_status_only`). Downstream consumers
        // gated on `synthesis_pending` can now consume the outputs.
        execution.state.synthesis_pending = false;
        execution.state.synthesis_failed = None;
        Self::upsert_output_ref(
            &mut execution.refs.output_refs,
            outputs.execution_output.clone(),
        );
        execution.refs.updated_at = Some(execution.state.updated_at.clone());
        let is_root_execution = Self::is_root_execution(execution);
        if !is_root_execution {
            let scope = Self::task_scope(task);
            let status = outcome.execution_status.clone();
            let updated_at = execution.state.updated_at.clone();
            let completed_at = execution.state.completed_at.clone();
            let execution_output = outputs.execution_output.clone();
            let completion_kind = outcome.completion_kind;
            let open_items = outcome.open_items.clone();
            let committed = self
                .commit_child_execution_with_root_repair(
                    &scope,
                    &task.manifest.task_id,
                    execution,
                    Some(status.clone()),
                    move |fresh| {
                        fresh.state.status = status;
                        if completion_kind.is_some() {
                            fresh.state.completion_kind = completion_kind;
                            fresh.state.open_items = open_items;
                        }
                        fresh.state.primary_execution_output_id =
                            Some(execution_output.output_id.clone());
                        fresh.state.synthesis_pending = false;
                        fresh.state.synthesis_failed = None;
                        if completed_at.is_some() {
                            fresh.state.completed_at = completed_at;
                        }
                        fresh.state.updated_at =
                            Self::latest_rfc3339(&fresh.state.updated_at, &updated_at);
                        Self::upsert_output_ref(&mut fresh.refs.output_refs, execution_output);
                        fresh.refs.updated_at = Some(Self::advance_optional_timestamp(
                            fresh.refs.updated_at.as_deref(),
                            &updated_at,
                        ));
                    },
                )
                .await?;
            *execution = committed;
            return Ok(());
        }

        // Task-level mutations are scoped to "only if this execution
        // is still the task's active root." If a newer execution has
        // taken over (scheduler fired a follow-up run before our
        // spawn's Step 2 landed; user manually re-ran the task; etc.)
        // then `active_root_execution_id` no longer points at us — we
        // must not clobber the newer execution's task state. We still
        // refresh per-execution outputs and the cumulative
        // `last_completed_root_execution_id` since those are
        // execution-scoped facts that don't conflict with the active
        // run.
        let active_root = task.state.active_root_execution_id.clone();
        let this_execution_id = execution.state.execution_id.as_str();
        let is_active_root = active_root.as_deref() == Some(this_execution_id);
        // Defense in depth against the V3-resume double-run / re-open race: a first
        // terminal pass for THIS root nulls `active_root_execution_id`; a second
        // terminal pass for the SAME root then saw `active_root != this` and skipped
        // the task-status finalization, stranding the task at "running" with the
        // root execution already terminal. Also finalize when NO execution holds the
        // active root AND this execution is the task's latest root — but NEVER when a
        // DIFFERENT (newer) execution has taken over (that newer run owns the task
        // state). See docs/plans/2026-06-17-vibedev-v3-resume-double-run.md.
        let finalize_as_terminal_root = is_root_execution
            && (is_active_root
                || (active_root.is_none()
                    && task.state.latest_root_execution_id.as_deref() == Some(this_execution_id)));
        if finalize_as_terminal_root {
            task.state.status = outcome.task_status.clone();
            if outcome.completion_kind.is_some() {
                task.state.completion_kind = outcome.completion_kind;
                task.state.open_items = outcome.open_items.clone();
            }
            task.state.active_root_execution_id = None;
            task.state.latest_root_execution_id = Some(execution.state.execution_id.clone());
            task.state.synthesis_failed_execution_id = None;
            task.state.default_task_agent_output_id =
                Some(outputs.task_agent_output.output_id.clone());
            task.state.primary_user_output_id = Some(outputs.task_user_output.output_id.clone());
            task.refs.default_task_agent_output_id =
                Some(outputs.task_agent_output.output_id.clone());
            task.refs.primary_user_output_id = Some(outputs.task_user_output.output_id.clone());
        }
        // Remove this execution's id from the per-task in-flight
        // synthesis registry — safe regardless of active_root scoping
        // because we only ever pop OUR own id. If another execution
        // also has synthesis in flight, its id stays in the vec, and
        // `task.synthesis_pending()` continues to report `true`.
        if is_root_execution {
            task.state
                .synthesis_pending_executions
                .retain(|id| id != execution.state.execution_id.as_str());
        }
        // `last_completed_root_execution_id` is the cumulative high-
        // water mark for successful completions — safe to update
        // regardless of whether a newer run has overtaken us.
        if is_root_execution && outcome.execution_status == "completed" {
            task.state.last_completed_root_execution_id =
                Some(execution.state.execution_id.clone());
        }
        if is_root_execution {
            task.state.updated_at = execution.state.updated_at.clone();
            Self::prune_stale_primary_task_outputs(&mut task.refs.outputs, outputs);
            Self::upsert_output_ref(&mut task.refs.outputs, outputs.task_agent_output.clone());
            Self::upsert_output_ref(&mut task.refs.outputs, outputs.task_user_output.clone());
            if let Some(output) = outputs.continuation_context_output.as_ref() {
                Self::upsert_output_ref(&mut task.refs.outputs, output.clone());
            }
            // Produced media outputs (image/video/audio) — surfaced as
            // user-audience outputs so a headline media deliverable renders inline.
            for media_output in &outputs.media_outputs {
                Self::upsert_output_ref(&mut task.refs.outputs, media_output.clone());
            }
            task.refs.updated_at = Some(execution.state.updated_at.clone());
        }

        let scope = Self::task_scope(task);
        self.with_task_write_lock(&scope, &task.manifest.task_id, || async {
            let mut writes = self.execution_record_writes(&scope, execution)?;
            writes.extend(self.task_state_and_refs_writes(task)?);
            writes.push(
                self.execution_index_write(&scope, &task.manifest.task_id, &[execution])
                    .await?,
            );
            self.commit_task_writes(&scope, &task.manifest.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_child_execution_terminal(
        &self,
        scope: &ScopeRef,
        parent_execution_id: &str,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
        execution_output: &OutputRef,
    ) -> Result<(), ArtifactV2Error> {
        let task_id = execution.state.task_id.clone();
        let child_execution_id = execution.state.execution_id.clone();
        let terminal_updated_at = execution.state.updated_at.clone();
        let terminal_completed_at = execution.state.completed_at.clone();
        let terminal_status = outcome.execution_status.clone();
        let outcome_type = outcome.outcome_type.clone();
        let execution_output = execution_output.clone();
        let committed_execution = self
            .with_task_write_lock(scope, &task_id, || async {
                let mut task = self.load_task_record(scope, &task_id).await?;
                let mut parent = self
                    .load_execution_record(scope, &task_id, parent_execution_id)
                    .await?;
                let mut fresh_execution = self
                    .load_execution_record(scope, &task_id, &child_execution_id)
                    .await?;

                let terminal_status_conflict =
                    Self::execution_status_is_terminal(&fresh_execution.state.status)
                        && fresh_execution.state.status != terminal_status;
                let committed_terminal_status = if terminal_status_conflict {
                    warn!(
                        task_id = %task_id,
                        execution_id = %child_execution_id,
                        durable_status = %fresh_execution.state.status,
                        rejected_status = %terminal_status,
                        "[REDUCER] retained first durable terminal child status"
                    );
                    fresh_execution.state.status.clone()
                } else {
                    terminal_status.clone()
                };
                fresh_execution.state.status = committed_terminal_status.clone();
                fresh_execution.state.primary_execution_output_id =
                    Some(execution_output.output_id.clone());
                fresh_execution.state.active_child_execution_ids.clear();
                if terminal_completed_at.is_some() {
                    fresh_execution.state.completed_at = terminal_completed_at.clone();
                }
                fresh_execution.state.updated_at =
                    Self::latest_rfc3339(&fresh_execution.state.updated_at, &terminal_updated_at);
                Self::upsert_output_ref(
                    &mut fresh_execution.refs.output_refs,
                    execution_output.clone(),
                );
                fresh_execution.refs.updated_at = Some(Self::advance_optional_timestamp(
                    fresh_execution.refs.updated_at.as_deref(),
                    &terminal_updated_at,
                ));
                Self::repair_child_root_ownership(&mut task, &fresh_execution);

                Self::remove_child_execution_id(
                    &mut parent.state.active_child_execution_ids,
                    &child_execution_id,
                );
                parent.state.updated_at =
                    Self::latest_rfc3339(&parent.state.updated_at, &terminal_updated_at);
                Self::upsert_output_ref(
                    &mut parent.refs.child_output_refs,
                    execution_output.clone(),
                );
                if let Some(delegation) = parent.refs.delegations.iter_mut().find(|delegation| {
                    delegation.child_execution_id.as_deref() == Some(child_execution_id.as_str())
                }) {
                    delegation.status = committed_terminal_status.clone();
                    if !terminal_status_conflict {
                        delegation.outcome_type = Some(outcome_type.clone());
                    }
                    delegation.output_id = Some(execution_output.output_id.clone());
                    delegation.updated_at =
                        Self::latest_rfc3339(&delegation.updated_at, &terminal_updated_at);
                }
                parent.refs.updated_at = Some(Self::advance_optional_timestamp(
                    parent.refs.updated_at.as_deref(),
                    &terminal_updated_at,
                ));

                task.state.updated_at =
                    Self::latest_rfc3339(&task.state.updated_at, &terminal_updated_at);

                let mut writes = self.execution_record_writes(scope, &fresh_execution)?;
                writes.extend(self.execution_record_writes(scope, &parent)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, &task_id, &[&fresh_execution, &parent])
                        .await?,
                );
                self.commit_task_writes(scope, &task_id, &writes).await?;
                Ok(fresh_execution)
            })
            .await?;
        *execution = committed_execution;
        Ok(())
    }

    async fn reduce_child_execution_terminal_without_output(
        &self,
        scope: &ScopeRef,
        parent_execution_id: &str,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error> {
        let task_id = execution.state.task_id.clone();
        let child_execution_id = execution.state.execution_id.clone();
        let terminal_updated_at = execution.state.updated_at.clone();
        let terminal_completed_at = execution.state.completed_at.clone();
        let terminal_status = outcome.execution_status.clone();
        let outcome_type = outcome.outcome_type.clone();
        let committed_execution = self
            .with_task_write_lock(scope, &task_id, || async {
                let mut task = self.load_task_record(scope, &task_id).await?;
                let mut parent = self
                    .load_execution_record(scope, &task_id, parent_execution_id)
                    .await?;
                let mut fresh_execution = self
                    .load_execution_record(scope, &task_id, &child_execution_id)
                    .await?;

                let terminal_status_conflict =
                    Self::execution_status_is_terminal(&fresh_execution.state.status)
                        && fresh_execution.state.status != terminal_status;
                let committed_terminal_status = if terminal_status_conflict {
                    warn!(
                        task_id = %task_id,
                        execution_id = %child_execution_id,
                        durable_status = %fresh_execution.state.status,
                        rejected_status = %terminal_status,
                        "[REDUCER] retained first durable terminal child status"
                    );
                    fresh_execution.state.status.clone()
                } else {
                    terminal_status.clone()
                };
                fresh_execution.state.status = committed_terminal_status.clone();
                fresh_execution.state.active_child_execution_ids.clear();
                if terminal_completed_at.is_some() {
                    fresh_execution.state.completed_at = terminal_completed_at.clone();
                }
                fresh_execution.state.updated_at =
                    Self::latest_rfc3339(&fresh_execution.state.updated_at, &terminal_updated_at);
                fresh_execution.refs.updated_at = Some(Self::advance_optional_timestamp(
                    fresh_execution.refs.updated_at.as_deref(),
                    &terminal_updated_at,
                ));
                Self::repair_child_root_ownership(&mut task, &fresh_execution);

                Self::remove_child_execution_id(
                    &mut parent.state.active_child_execution_ids,
                    &child_execution_id,
                );
                if let Some(delegation) = parent.refs.delegations.iter_mut().find(|delegation| {
                    delegation.child_execution_id.as_deref() == Some(child_execution_id.as_str())
                }) {
                    delegation.status = committed_terminal_status;
                    if !terminal_status_conflict {
                        delegation.outcome_type = Some(outcome_type.clone());
                    }
                    delegation.updated_at =
                        Self::latest_rfc3339(&delegation.updated_at, &terminal_updated_at);
                }
                parent.state.updated_at =
                    Self::latest_rfc3339(&parent.state.updated_at, &terminal_updated_at);
                parent.refs.updated_at = Some(Self::advance_optional_timestamp(
                    parent.refs.updated_at.as_deref(),
                    &terminal_updated_at,
                ));

                task.state.updated_at =
                    Self::latest_rfc3339(&task.state.updated_at, &terminal_updated_at);

                let mut writes = self.execution_record_writes(scope, &fresh_execution)?;
                writes.extend(self.execution_record_writes(scope, &parent)?);
                writes.extend(self.task_state_and_refs_writes(&task)?);
                writes.push(
                    self.execution_index_write(scope, &task_id, &[&fresh_execution, &parent])
                        .await?,
                );
                self.commit_task_writes(scope, &task_id, &writes).await?;
                Ok(fresh_execution)
            })
            .await?;
        *execution = committed_execution;
        Ok(())
    }

    async fn reduce_execution_terminal_without_outputs(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error> {
        execution.state.status = outcome.execution_status.clone();
        // This is the terminal degraded/abandoned-synthesis path. Leaving the
        // execution flag true while removing only the task-level registry made
        // startup recovery respawn synthesis forever (especially for cancelled
        // executions, where no output should be generated at all).
        execution.state.synthesis_pending = false;
        let is_root_execution = Self::is_root_execution(execution);
        if !is_root_execution {
            let scope = Self::task_scope(task);
            let status = outcome.execution_status.clone();
            let updated_at = execution.state.updated_at.clone();
            let completed_at = execution.state.completed_at.clone();
            let completion_kind = outcome.completion_kind;
            let open_items = outcome.open_items.clone();
            let committed = self
                .commit_child_execution_with_root_repair(
                    &scope,
                    &task.manifest.task_id,
                    execution,
                    Some(status.clone()),
                    move |fresh| {
                        fresh.state.status = status;
                        if completion_kind.is_some() {
                            fresh.state.completion_kind = completion_kind;
                            fresh.state.open_items = open_items;
                        }
                        fresh.state.synthesis_pending = false;
                        if completed_at.is_some() {
                            fresh.state.completed_at = completed_at;
                        }
                        fresh.state.updated_at =
                            Self::latest_rfc3339(&fresh.state.updated_at, &updated_at);
                    },
                )
                .await?;
            *execution = committed;
            return Ok(());
        }
        // Task-level mutations scoped to "this execution is still the
        // active root" — same reasoning as `reduce_execution_terminal`.
        // A late call (after a newer execution has overtaken) must not
        // clobber the newer execution's active_root + status.
        let active_root = task.state.active_root_execution_id.clone();
        let this_execution_id = execution.state.execution_id.as_str();
        let is_active_root = active_root.as_deref() == Some(this_execution_id);
        // Defense in depth against the V3-resume double-run / re-open race: a first
        // terminal pass for THIS root nulls `active_root_execution_id`; a second
        // terminal pass for the SAME root then saw `active_root != this` and skipped
        // the task-status finalization, stranding the task at "running" with the
        // root execution already terminal. Also finalize when NO execution holds the
        // active root AND this execution is the task's latest root — but NEVER when a
        // DIFFERENT (newer) execution has taken over (that newer run owns the task
        // state). See docs/plans/2026-06-17-vibedev-v3-resume-double-run.md.
        let finalize_as_terminal_root = is_root_execution
            && (is_active_root
                || (active_root.is_none()
                    && task.state.latest_root_execution_id.as_deref() == Some(this_execution_id)));
        if finalize_as_terminal_root {
            task.state.status = outcome.task_status.clone();
            if outcome.completion_kind.is_some() {
                task.state.completion_kind = outcome.completion_kind;
                task.state.open_items = outcome.open_items.clone();
            }
            task.state.active_root_execution_id = None;
            task.state.latest_root_execution_id = Some(execution.state.execution_id.clone());
        }
        // Remove from the in-flight synthesis registry — this is the
        // degraded-settle path so synthesis is "done" (just without
        // outputs). Other in-flight executions stay in the vec.
        if is_root_execution {
            task.state
                .synthesis_pending_executions
                .retain(|id| id != execution.state.execution_id.as_str());
            task.state.updated_at = execution.state.updated_at.clone();
        }

        let scope = Self::task_scope(task);
        self.with_task_write_lock(&scope, &task.manifest.task_id, || async {
            let mut writes = self.execution_record_writes(&scope, execution)?;
            writes.extend(self.task_state_and_refs_writes(task)?);
            writes.push(
                self.execution_index_write(&scope, &task.manifest.task_id, &[execution])
                    .await?,
            );
            self.commit_task_writes(&scope, &task.manifest.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_task_auxiliary_output(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        output: &OutputRef,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, task_id, || async {
            let mut task = self.load_task_record(scope, task_id).await?;
            Self::upsert_output_ref(&mut task.refs.outputs, output.clone());
            task.refs.updated_at = Some(output.created_at.clone());
            self.commit_task_writes(scope, task_id, &self.task_state_and_refs_writes(&task)?)
                .await
        })
        .await
    }

    async fn reduce_execution_terminal_status_only(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        outcome: &ExecutionOutcomeSnapshot,
    ) -> Result<(), ArtifactV2Error> {
        // Atomic Step 1 (see trait doc): "status is structural truth,
        // synthesis is content". Flip BOTH execution AND task to the
        // terminal status immediately so list endpoints render
        // "completed / finalizing…" the moment the execution ends —
        // we don't wait for synthesis (which may take 10s+ of LLM
        // round-trips). `synthesis_pending = true` signals to
        // downstream consumers that the output artifacts are still in
        // flight; the dependency scheduler honours this and blocks
        // dependents, and the UI shows a synthesizing pill.
        //
        // `reduce_execution_terminal` (Step 2, post-synthesis) is then
        // idempotent on status fields but still sets up outputs and
        // clears `synthesis_pending`. If synthesis instead exhausts
        // retries, `reduce_execution_synthesis_failed` clears
        // `synthesis_pending` and stamps `synthesis_failed` — task
        // status stays in its Step 1 terminal value, which is the
        // honest structural truth (the execution IS over; the
        // synthesized outputs are just missing).
        execution.state.status = outcome.execution_status.clone();
        if outcome.completion_kind.is_some() {
            execution.state.completion_kind = outcome.completion_kind;
            execution.state.open_items = outcome.open_items.clone();
        }
        execution.state.synthesis_pending = true;
        execution.state.synthesis_failed = None;
        execution.refs.updated_at = Some(execution.state.updated_at.clone());

        let is_root_execution = Self::is_root_execution(execution);
        if !is_root_execution {
            let scope = Self::task_scope(task);
            let status = outcome.execution_status.clone();
            let updated_at = execution.state.updated_at.clone();
            let completed_at = execution.state.completed_at.clone();
            let completion_kind = outcome.completion_kind;
            let open_items = outcome.open_items.clone();
            let committed = self
                .commit_child_execution_with_root_repair(
                    &scope,
                    &task.manifest.task_id,
                    execution,
                    Some(status.clone()),
                    move |fresh| {
                        fresh.state.status = status;
                        if completion_kind.is_some() {
                            fresh.state.completion_kind = completion_kind;
                            fresh.state.open_items = open_items;
                        }
                        fresh.state.synthesis_pending = true;
                        fresh.state.synthesis_failed = None;
                        if completed_at.is_some() {
                            fresh.state.completed_at = completed_at;
                        }
                        fresh.state.updated_at =
                            Self::latest_rfc3339(&fresh.state.updated_at, &updated_at);
                        fresh.refs.updated_at = Some(Self::advance_optional_timestamp(
                            fresh.refs.updated_at.as_deref(),
                            &updated_at,
                        ));
                    },
                )
                .await?;
            *execution = committed;
            return Ok(());
        }
        task.state.status = outcome.task_status.clone();
        if outcome.completion_kind.is_some() {
            task.state.completion_kind = outcome.completion_kind;
            task.state.open_items = outcome.open_items.clone();
        }
        task.state.latest_root_execution_id = Some(execution.state.execution_id.clone());
        if outcome.execution_status == "completed" {
            task.state.last_completed_root_execution_id =
                Some(execution.state.execution_id.clone());
        }
        // Register this root execution as in-flight synthesis. Child
        // synthesis is execution-scoped and must not own task readiness.
        let exec_id = execution.state.execution_id.clone();
        if !task.state.synthesis_pending_executions.contains(&exec_id) {
            task.state.synthesis_pending_executions.push(exec_id);
        }
        task.state.synthesis_failed_execution_id = None;
        task.state.updated_at = execution.state.updated_at.clone();

        let scope = Self::task_scope(task);
        self.with_task_write_lock(&scope, &task.manifest.task_id, || async {
            let mut writes = self.execution_record_writes(&scope, execution)?;
            writes.extend(self.task_state_and_refs_writes(task)?);
            writes.push(
                self.execution_index_write(&scope, &task.manifest.task_id, &[execution])
                    .await?,
            );
            self.commit_task_writes(&scope, &task.manifest.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_execution_synthesis_failed(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
        failure: crate::magician_v2::artifact_v2::models::SynthesisFailure,
    ) -> Result<(), ArtifactV2Error> {
        let now = chrono::Utc::now().to_rfc3339();
        execution.state.synthesis_pending = false;
        execution.state.synthesis_failed = Some(failure);
        execution.state.updated_at = now.clone();
        execution.refs.updated_at = Some(now.clone());
        if !Self::is_root_execution(execution) {
            let scope = Self::task_scope(task);
            let failure = execution.state.synthesis_failed.clone();
            let committed = self
                .commit_child_execution_with_root_repair(
                    &scope,
                    &task.manifest.task_id,
                    execution,
                    None,
                    move |fresh| {
                        fresh.state.synthesis_pending = false;
                        fresh.state.synthesis_failed = failure;
                        fresh.state.updated_at = now.clone();
                        fresh.refs.updated_at = Some(now);
                    },
                )
                .await?;
            *execution = committed;
            return Ok(());
        }

        // Always remove this execution from the in-flight registry —
        // its synthesis is done (failed). Other executions in flight
        // stay in the vec; `task.synthesis_pending()` continues to
        // report `true` while any remain.
        task.state
            .synthesis_pending_executions
            .retain(|id| id != execution.state.execution_id.as_str());

        // `synthesis_failed_execution_id` is last-failure-wins surfaced
        // to the UI for the retry-HITL affordance. Only point it at
        // THIS execution if no newer execution has already taken over
        // — otherwise the operator should retry the newer one first.
        let is_latest_root = task.state.latest_root_execution_id.as_deref()
            == Some(execution.state.execution_id.as_str())
            || task.state.latest_root_execution_id.is_none();
        if is_latest_root {
            task.state.synthesis_failed_execution_id = Some(execution.state.execution_id.clone());
        }
        task.state.updated_at = now;

        let scope = Self::task_scope(task);
        self.with_task_write_lock(&scope, &task.manifest.task_id, || async {
            let mut writes = self.execution_record_writes(&scope, execution)?;
            writes.extend(self.task_state_and_refs_writes(task)?);
            writes.push(
                self.execution_index_write(&scope, &task.manifest.task_id, &[execution])
                    .await?,
            );
            self.commit_task_writes(&scope, &task.manifest.task_id, &writes)
                .await
        })
        .await
    }

    async fn reduce_clear_synthesis_failure(
        &self,
        task: &mut TaskRecord,
        execution: &mut ExecutionRecord,
    ) -> Result<bool, ArtifactV2Error> {
        let scope = Self::task_scope(task);
        let task_id = task.manifest.task_id.clone();
        let exec_id = execution.state.execution_id.clone();
        self.with_task_write_lock(&scope, &task_id, || async {
            // CRITICAL: re-read fresh state inside the lock. The caller
            // passed in clones loaded BEFORE the lock was acquired; a
            // concurrent retry attempt might have already cleared the
            // failure marker. If we naively wrote the in-memory clone
            // back to disk we'd (a) double-spawn synthesis from two
            // retry callers, and (b) clobber any other concurrent task
            // edit (manifest rename, retag) that landed between the
            // caller's load and now.
            let mut fresh_task = self.load_task_record(&scope, &task_id).await?;
            let mut fresh_execution = self
                .load_execution_record(&scope, &task_id, &exec_id)
                .await?;
            if fresh_execution.state.synthesis_failed.is_none() {
                // Race lost — another caller already cleared the marker.
                // Return false so the upstream caller knows not to spawn.
                return Ok(false);
            }
            let now = chrono::Utc::now().to_rfc3339();
            fresh_execution.state.synthesis_failed = None;
            fresh_execution.state.synthesis_pending = true;
            fresh_execution.state.updated_at = now.clone();
            fresh_execution.refs.updated_at = Some(now.clone());

            let is_root_execution = Self::is_root_execution(&fresh_execution);
            let repaired_child_ownership = if is_root_execution {
                false
            } else {
                Self::repair_child_root_ownership(&mut fresh_task, &fresh_execution)
            };
            if is_root_execution {
                if fresh_task.state.synthesis_failed_execution_id.as_deref() == Some(&exec_id) {
                    fresh_task.state.synthesis_failed_execution_id = None;
                }
                if !fresh_task
                    .state
                    .synthesis_pending_executions
                    .contains(&exec_id)
                {
                    fresh_task
                        .state
                        .synthesis_pending_executions
                        .push(exec_id.clone());
                }
                fresh_task.state.updated_at = now;
            }

            let mut writes = self.execution_record_writes(&scope, &fresh_execution)?;
            if is_root_execution || repaired_child_ownership {
                writes.extend(self.task_state_and_refs_writes(&fresh_task)?);
            }
            writes.push(
                self.execution_index_write(&scope, &task_id, &[&fresh_execution])
                    .await?,
            );
            self.commit_task_writes(&scope, &task_id, &writes).await?;

            // Reflect the new state into the caller's clones so they
            // can pass them through to the spawn helper without doing
            // another disk read. (The spawn re-reads anyway per Bug 2's
            // fix, so this is just for consistency.)
            *task = fresh_task;
            *execution = fresh_execution;
            Ok(true)
        })
        .await
    }

    async fn persist_task_state_only(&self, task: &TaskRecord) -> Result<(), ArtifactV2Error> {
        let scope = Self::task_scope(task);
        let task_id = task.manifest.task_id.clone();
        let new_state = task.state.clone();
        self.with_task_write_lock(&scope, &task_id, || async {
            // Re-read fresh manifest + refs from disk inside the lock
            // so concurrent edits to those fields (rename, retag,
            // repriority, output_ref additions) aren't clobbered by
            // the caller's stale clone. We only overwrite the
            // `state` slice — that's the migration-backfill's actual
            // intent.
            let fresh = self.load_task_record(&scope, &task_id).await?;
            let merged = TaskRecord {
                manifest: fresh.manifest,
                state: new_state,
                refs: fresh.refs,
            };
            let scope_ref = &scope;
            let task_id_ref = task_id.as_str();
            warn_if_child_installed_as_root(
                &merged,
                "persist_task_state_only",
                |id: String| async move {
                    self.load_execution_record(scope_ref, task_id_ref, &id)
                        .await
                        .ok()
                },
            )
            .await;
            let writes = self.task_record_writes(&merged)?;
            self.commit_task_writes(&scope, &task_id, &writes).await
        })
        .await
    }

    async fn reduce_runtime_signal(
        &self,
        scope: &ScopeRef,
        ctx: &ExecutionContext,
        execution_status: Option<&str>,
        task_status: Option<&str>,
        active_child_execution_ids: Option<&[String]>,
        current_step_id: Option<&str>,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, &ctx.task_id, || async {
            let mut task = self.load_task_record(scope, &ctx.task_id).await?;
            let mut execution = self
                .load_execution_record(scope, &ctx.task_id, &ctx.execution_id)
                .await?;
            let is_root_execution = Self::is_root_execution(&execution);
            let was_terminal = Self::execution_status_is_terminal(&execution.state.status);

            if Self::execution_status_is_terminal(&execution.state.status)
                && execution_status
                    .is_some_and(|status| !Self::execution_status_is_terminal(status))
            {
                warn!(
                    task_id = %ctx.task_id,
                    execution_id = %ctx.execution_id,
                    durable_status = %execution.state.status,
                    rejected_status = ?execution_status,
                    "[REDUCER] ignored late runtime signal that would reopen terminal execution"
                );
                return Ok(());
            }
            if is_root_execution
                && matches!(
                    task.state.status.as_str(),
                    "completed" | "failed" | "cancelled" | "canceled"
                )
                && task_status.is_some_and(Self::task_status_keeps_active_root)
            {
                warn!(
                    task_id = %ctx.task_id,
                    execution_id = %ctx.execution_id,
                    durable_task_status = %task.state.status,
                    rejected_task_status = ?task_status,
                    "[REDUCER] ignored late runtime signal that would reopen terminal task"
                );
                return Ok(());
            }

            execution.state.updated_at = updated_at.to_string();
            if let Some(status) = execution_status {
                execution.state.status = status.to_string();
            }
            // **The runtime status signal is the write that flips an
            // execution to `completed`** (the terminal reducers append events
            // and outputs; the status itself arrives here from the runtime's
            // transition). The signal carries no deliverable, so the kind is
            // read from the agentic summary record the loop persisted just
            // before it — the same record the outcome snapshot reads.
            if execution.state.status == "completed" && execution.state.completion_kind.is_none() {
                let summary_path = self
                    .workspace
                    .execution_dir(
                        &scope.principal(),
                        &scope.workspace(),
                        &ctx.task_id,
                        &ctx.execution_id,
                    )
                    .join("execution")
                    .join("latest_summary.json");
                if let Ok(record) = self
                    .workspace
                    .read_json_path_sync::<crate::magician_v2::execution::execution_summary::AgenticExecutionSummaryRecord, _>(
                        &summary_path,
                    )
                {
                    let (completion_kind, open_items) = record.completion();
                    execution.state.completion_kind = completion_kind;
                    execution.state.open_items = open_items;
                }
            }
            if let Some(child_execution_ids) = active_child_execution_ids {
                execution.state.active_child_execution_ids = child_execution_ids.to_vec();
            }
            if let Some(step_id) = current_step_id {
                execution.state.current_step_id = Some(step_id.to_string());
            }

            // Readiness has to mean readable. This signal is the write that
            // flips an execution to `completed`; its deliverable is attached
            // by the terminal reducers that follow. Registering the in-flight
            // synthesis marker in the same commit closes the window in which a
            // consumer polling "terminal and nothing pending" reads an answer
            // that has not been written yet. Step 1 of the terminal pipeline
            // registers the same marker and is idempotent, the terminal
            // reducers clear it, and startup recovery re-drives synthesis for
            // an execution still carrying it after a crash — so nothing here
            // can park a task behind a marker nobody clears. Only on the
            // transition INTO the status: a late duplicate signal naming a
            // status the execution already holds must not re-open a settled
            // task.
            if is_root_execution && !was_terminal && execution.state.status == "completed" {
                execution.state.synthesis_pending = true;
                if !task
                    .state
                    .synthesis_pending_executions
                    .contains(&ctx.execution_id)
                {
                    task.state
                        .synthesis_pending_executions
                        .push(ctx.execution_id.clone());
                }
            }

            if is_root_execution {
                task.state.updated_at = updated_at.to_string();
                task.state.latest_root_execution_id = Some(ctx.execution_id.clone());
                match task_status {
                    Some(status) if Self::task_status_keeps_active_root(status) => {
                        task.state.active_root_execution_id = Some(ctx.execution_id.clone());
                    },
                    Some(_) => {
                        task.state.active_root_execution_id = None;
                        if matches!(task_status, Some("completed")) {
                            task.state.last_completed_root_execution_id =
                                Some(ctx.execution_id.clone());
                        }
                    },
                    None => {
                        task.state.active_root_execution_id = Some(ctx.execution_id.clone());
                    },
                }
            }
            if is_root_execution {
                if let Some(status) = task_status {
                    // ─── Diagnostic — catches the early-"completed" bug on
                    // the runtime-signal reducer path. Allows the write
                    // (unlike `reduce_execution_nonterminal` which refuses)
                    // because this path is also driven by genuinely
                    // terminal signals from the orchestrator. The warn
                    // only fires when the task has no output refs yet,
                    // which is the SDA-bug fingerprint: status flipped to
                    // "completed" before `finalize_terminal_execution`
                    // registered an output. Stack capture lets us trace
                    // the caller.
                    if matches!(status, "completed" | "failed" | "cancelled")
                        && task.refs.outputs.is_empty()
                        && task.state.primary_user_output_id.is_none()
                        && task.state.default_task_agent_output_id.is_none()
                    {
                        let backtrace = Backtrace::force_capture();
                        warn!(
                            task_id = %ctx.task_id,
                            execution_id = %ctx.execution_id,
                            new_task_status = %status,
                            execution_status = ?execution_status,
                            prior_task_status = %task.state.status,
                            backtrace = ?backtrace,
                            "[REDUCER] reduce_runtime_signal writing \
                             terminal task_status with no output refs \
                             registered. Likely premature — chat fan-outs \
                             tearing down on `task.status_changed: \
                             completed` will miss subsequent \
                             `output.available` events. Write IS being \
                             applied (this path is also used by genuine \
                             terminal signals); stack capture identifies \
                             the upstream caller. See SDA delegation \
                             investigation."
                        );
                    }
                    task.state.status = status.to_string();
                }
            }
            let repaired_child_ownership =
                !is_root_execution && Self::repair_child_root_ownership(&mut task, &execution);

            let mut writes = self.execution_record_writes(scope, &execution)?;
            if is_root_execution || repaired_child_ownership {
                writes.extend(self.task_state_and_refs_writes(&task)?);
            }
            writes.push(
                self.execution_index_write(scope, &ctx.task_id, &[&execution])
                    .await?,
            );
            self.commit_task_writes(scope, &ctx.task_id, &writes).await
        })
        .await
    }

    async fn reduce_step_event(
        &self,
        scope: &ScopeRef,
        ctx: &ExecutionContext,
        event_type: super::events::ArtifactV2EventType,
        step_id: &str,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, &ctx.task_id, || async {
            let mut execution = self
                .load_execution_record(scope, &ctx.task_id, &ctx.execution_id)
                .await?;

            match event_type {
                super::events::ArtifactV2EventType::StepCompleted => {
                    if !execution
                        .state
                        .completed_step_ids
                        .contains(&step_id.to_string())
                    {
                        execution.state.completed_step_ids.push(step_id.to_string());
                    }
                    // Clear current_step_id if it was this step
                    if execution.state.current_step_id.as_deref() == Some(step_id) {
                        execution.state.current_step_id = None;
                    }
                },
                super::events::ArtifactV2EventType::StepFailed => {
                    if !execution
                        .state
                        .failed_step_ids
                        .contains(&step_id.to_string())
                    {
                        execution.state.failed_step_ids.push(step_id.to_string());
                    }
                    if execution.state.current_step_id.as_deref() == Some(step_id) {
                        execution.state.current_step_id = None;
                    }
                },
                _ => {},
            }

            execution.state.updated_at = updated_at.to_string();

            let mut writes = self.execution_record_writes(scope, &execution)?;

            // A step reaching a terminal outcome IS the run advancing, so this
            // is one of the three sites allowed to move `last_progress_at`.
            // `updated_at` is deliberately left alone: it already moves on every
            // mirrored runtime event, which is exactly why it cannot answer
            // "did the run advance". Written with the manifest-preserving helper
            // so a concurrent rename/retag landing mid-step isn't clobbered.
            if matches!(
                event_type,
                super::events::ArtifactV2EventType::StepCompleted
                    | super::events::ArtifactV2EventType::StepFailed
            ) {
                let mut task = self.load_task_record(scope, &ctx.task_id).await?;
                task.state.last_progress_at = Some(updated_at.to_string());
                writes.extend(self.task_state_and_refs_writes(&task)?);
            }

            self.commit_task_writes(scope, &ctx.task_id, &writes).await
        })
        .await
    }

    /// An action settled with a result — the flat loop's unit of advancement.
    /// A direct run has no taskplan steps, so without this a run making a
    /// tool call every few seconds still reads as having made no progress
    /// since its first step, and the panel calls it stalled at minute five.
    async fn reduce_action_settled(
        &self,
        scope: &ScopeRef,
        ctx: &ExecutionContext,
        updated_at: &str,
    ) -> Result<(), ArtifactV2Error> {
        self.with_task_write_lock(scope, &ctx.task_id, || async {
            let mut task = self.load_task_record(scope, &ctx.task_id).await?;
            task.state.last_progress_at = Some(updated_at.to_string());
            task.state.updated_at = updated_at.to_string();
            let writes = self.task_state_and_refs_writes(&task)?;
            self.commit_task_writes(scope, &ctx.task_id, &writes).await
        })
        .await
    }

    async fn list_executions(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<Vec<ExecutionIndexEntry>, ArtifactV2Error> {
        let entries = self
            .with_task_write_lock(scope, task_id, || async {
                let path = self.workspace.task_executions_index_path(
                    &scope.principal(),
                    &scope.workspace(),
                    task_id,
                );
                let read = self
                    .workspace
                    .read_jsonl_path_tolerant::<ExecutionIndexEntry, _>(&path)
                    .await?;
                if read.lost_committed_records() {
                    tracing::warn!(
                        target: "artifact_v2::reducer",
                        path = %path.display(),
                        corrupt_records = read.corrupt,
                        "execution listing is missing unreadable index records"
                    );
                }
                Ok(read.records)
            })
            .await?;
        let mut latest: Vec<_> = entries;
        latest.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(latest)
    }
}

pub type SharedArtifactV2Reducer = Arc<dyn ArtifactV2Reducer>;

impl Drop for CrossProcessTaskGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::io::read_json;
    use crate::magician_v2::artifact_v2::models::{
        AppRecipeScheduleStepState, ExecutionRefs, ExecutionScheduleRecord, ExecutionScheduleStep,
        ExecutionState, PlanRef, TaskManifest, TaskOutputMode, TaskRefs, TaskState,
        RELATIONSHIP_TYPE_DELEGATE,
    };
    use tempfile::tempdir;

    #[test]
    fn recipe_claim_lease_is_live_only_before_its_exact_expiry() {
        assert!(app_recipe_claim_lease_is_live(101, 100));
        assert!(!app_recipe_claim_lease_is_live(101, 101));
        assert!(!app_recipe_claim_lease_is_live(101, 102));
    }

    #[test]
    fn deferred_exact_execution_retains_the_active_root_fence() {
        assert!(FilesystemArtifactV2Reducer::task_status_keeps_active_root(
            "deferred"
        ));
    }

    /// A reducer wired to a reconciler with nothing in it.
    ///
    /// That is the production shape before `set_list_index` fills the slot, and
    /// what these tests want — they assert on the files the reducer writes, not
    /// on the index it reconciles. The argument is not optional, though: a
    /// reducer that could be built without a reconciler is a task-record writer
    /// with no route to the list index, which is the bug this type exists to
    /// make unrepresentable.
    fn test_reducer(workspace: ArtifactV2Workspace) -> FilesystemArtifactV2Reducer {
        let task_writes = Arc::new(TaskWriteReconciler::new(workspace.clone()));
        FilesystemArtifactV2Reducer::new(workspace, task_writes)
    }

    fn test_scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(
            &"principal".to_string(),
            &"workspace".to_string(),
        )
    }

    fn task_record(task_id: &str, updated_at: &str) -> TaskRecord {
        TaskRecord {
            manifest: TaskManifest {
                task_id: task_id.to_string(),
                principal: "principal".to_string(),
                workspace: "workspace".to_string(),
                title: "Task".to_string(),
                description: "Description".to_string(),
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
                output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
                monitor_spec: None,
                monitor_revision: 0,
                created_at: updated_at.to_string(),
                updated_at: updated_at.to_string(),
            },
            state: TaskState {
                task_id: task_id.to_string(),
                status: "pending".to_string(),
                completion_kind: None,
                open_items: Vec::new(),
                active_root_execution_id: None,
                latest_root_execution_id: None,
                last_completed_root_execution_id: None,
                default_task_agent_output_id: None,
                primary_user_output_id: None,
                schedule_fire_count: 0,
                synthesis_pending_executions: Vec::new(),
                synthesis_failed_execution_id: None,
                monitor_cursor: None,
                last_progress_at: None,
                updated_at: updated_at.to_string(),
            },
            refs: TaskRefs {
                task_id: task_id.to_string(),
                outputs: Vec::new(),
                default_task_agent_output_id: None,
                primary_user_output_id: None,
                updated_at: Some(updated_at.to_string()),
            },
        }
    }

    fn execution_record(
        task_id: &str,
        execution_id: &str,
        status: &str,
        updated_at: &str,
    ) -> ExecutionRecord {
        ExecutionRecord {
            state: ExecutionState {
                execution_id: execution_id.to_string(),
                task_id: task_id.to_string(),
                root_execution_id: Some(execution_id.to_string()),
                parent_execution_id: None,
                agent_id: "personal-assistant".to_string(),
                relationship_type: "root".to_string(),
                status: status.to_string(),
                completion_kind: None,
                open_items: Vec::new(),
                plan_id: None,
                primary_execution_output_id: None,
                active_child_execution_ids: Vec::new(),
                started_at: updated_at.to_string(),
                completed_at: None,
                updated_at: updated_at.to_string(),
                completed_step_ids: Vec::new(),
                failed_step_ids: Vec::new(),
                current_step_id: None,
                task_output_mode:
                    crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
                refinement: None,
                synthesis_pending: false,
                synthesis_failed: None,
            },
            refs: ExecutionRefs {
                execution_id: execution_id.to_string(),
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn stale_root_initializer_cannot_replace_the_fresh_active_root() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace);
        let scope = test_scope();
        let task_id = "task-root-compare-and-commit";
        let mut winner_task = task_record(task_id, "2026-08-22T00:00:00Z");
        reducer
            .reduce_task_created(&winner_task)
            .await
            .expect("persist task");
        let mut delayed_loser_task = winner_task.clone();
        let winner = execution_record(task_id, "exec-winner", "running", "2026-08-22T00:00:01Z");
        reducer
            .reduce_execution_initialized(&mut winner_task, &winner)
            .await
            .expect("winner initializes");

        let loser = execution_record(task_id, "exec-loser", "running", "2026-08-22T00:00:02Z");
        let error = reducer
            .reduce_execution_initialized(&mut delayed_loser_task, &loser)
            .await
            .expect_err("stale loser must not replace the committed root");
        assert!(matches!(
            error,
            ArtifactV2Error::InvalidRequest(ref message)
                if message == "task_execution_in_progress:exec-winner"
        ));
        let retained = reducer
            .load_task_record(&scope, task_id)
            .await
            .expect("read retained winner");
        assert_eq!(
            retained.state.active_root_execution_id.as_deref(),
            Some("exec-winner")
        );
    }

    fn child_execution_record(
        task_id: &str,
        root_execution_id: &str,
        parent_execution_id: &str,
        child_execution_id: &str,
        status: &str,
        updated_at: &str,
    ) -> ExecutionRecord {
        let mut child = execution_record(task_id, child_execution_id, status, updated_at);
        child.state.root_execution_id = Some(root_execution_id.to_string());
        child.state.parent_execution_id = Some(parent_execution_id.to_string());
        child.state.relationship_type = RELATIONSHIP_TYPE_DELEGATE.to_string();
        child.state.agent_id = "web-researcher".to_string();
        child
    }

    async fn persisted_execution(
        workspace: &ArtifactV2Workspace,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> ExecutionRecord {
        ExecutionRecord {
            state: read_json::<ExecutionState>(&workspace.execution_state_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            ))
            .await
            .expect("execution state"),
            refs: read_json::<ExecutionRefs>(&workspace.execution_refs_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            ))
            .await
            .expect("execution refs"),
        }
    }

    #[tokio::test]
    async fn child_terminal_merge_preserves_state_committed_after_caller_snapshot() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-merge";
        let root_id = "exec-root";
        let child_id = "exec-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        let mut parent = execution_record(task_id, root_id, "running", "2026-08-04T00:00:00Z");
        parent
            .state
            .active_child_execution_ids
            .push(child_id.to_string());
        let child = child_execution_record(
            task_id,
            root_id,
            root_id,
            child_id,
            "running",
            "2026-08-04T00:00:30Z",
        );

        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &parent)
            .await
            .expect("parent");
        reducer
            .reduce_execution_discovered(&scope, &parent)
            .await
            .expect("parent child link");
        reducer
            .reduce_execution_initialized(&mut task, &child)
            .await
            .expect("child");

        // The terminal worker retains this pre-interleaving clone while a
        // different reducer commits newer child-local progress and refs.
        let mut stale_terminal_candidate = child.clone();
        stale_terminal_candidate.state.updated_at = "2026-08-04T00:03:00Z".to_string();
        stale_terminal_candidate.state.completed_at = Some("2026-08-04T00:03:00Z".to_string());
        let mut fresher_child = child.clone();
        fresher_child.state.updated_at = "2026-08-04T00:04:00Z".to_string();
        fresher_child.state.current_step_id = Some("step-research".to_string());
        fresher_child
            .state
            .completed_step_ids
            .push("step-open-source".to_string());
        fresher_child.refs.plan_refs.push(PlanRef {
            plan_id: "plan-fresh".to_string(),
            relative_path: "plans/plan-fresh.json".to_string(),
            created_at: "2026-08-04T00:02:00Z".to_string(),
        });
        reducer
            .reduce_execution_discovered(&scope, &fresher_child)
            .await
            .expect("fresh child progress");
        let mut fresher_parent = parent.clone();
        fresher_parent.state.updated_at = "2026-08-04T00:05:00Z".to_string();
        reducer
            .reduce_execution_discovered(&scope, &fresher_parent)
            .await
            .expect("fresh parent progress");
        task.state.updated_at = "2026-08-04T00:06:00Z".to_string();
        reducer
            .reduce_task_created(&task)
            .await
            .expect("fresh task progress");

        let output = OutputRef {
            output_id: "out-child".to_string(),
            scope: "execution".to_string(),
            audience: "agent".to_string(),
            role: "primary_execution".to_string(),
            relative_path: "outputs/child.md".to_string(),
            media_type: "text/markdown".to_string(),
            created_at: "2026-08-04T00:03:00Z".to_string(),
            source_execution_id: Some(child_id.to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "running".to_string(),
            outcome_type: "success".to_string(),
            outcome_summary: "research complete".to_string(),
            iterations_used: Some(2),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        reducer
            .reduce_child_execution_terminal(
                &scope,
                root_id,
                &mut stale_terminal_candidate,
                &outcome,
                &output,
            )
            .await
            .expect("child terminal merge");

        let committed = persisted_execution(&workspace, &scope, task_id, child_id).await;
        assert_eq!(committed.state.status, "completed");
        assert_eq!(
            committed.state.completed_at.as_deref(),
            Some("2026-08-04T00:03:00Z")
        );
        assert_eq!(committed.state.updated_at, "2026-08-04T00:04:00Z");
        assert_eq!(
            committed.state.current_step_id.as_deref(),
            Some("step-research")
        );
        assert!(committed
            .state
            .completed_step_ids
            .contains(&"step-open-source".to_string()));
        assert!(committed
            .refs
            .plan_refs
            .iter()
            .any(|plan| plan.plan_id == "plan-fresh"));
        assert!(committed
            .refs
            .output_refs
            .iter()
            .any(|candidate| candidate.output_id == "out-child"));
        assert_eq!(
            stale_terminal_candidate.state.current_step_id,
            committed.state.current_step_id
        );

        let committed_parent = persisted_execution(&workspace, &scope, task_id, root_id).await;
        assert_eq!(committed_parent.state.updated_at, "2026-08-04T00:05:00Z");
        assert!(!committed_parent
            .state
            .active_child_execution_ids
            .contains(&child_id.to_string()));
        let committed_task = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(committed_task.updated_at, "2026-08-04T00:06:00Z");

        let conflicting_terminal = ExecutionOutcomeSnapshot {
            execution_status: "failed".to_string(),
            task_status: "running".to_string(),
            outcome_type: "late_failure".to_string(),
            outcome_summary: "late observer disagreed".to_string(),
            iterations_used: Some(2),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        reducer
            .reduce_child_execution_terminal_without_output(
                &scope,
                root_id,
                &mut stale_terminal_candidate,
                &conflicting_terminal,
            )
            .await
            .expect("conflicting duplicate terminal observation is idempotent");
        let still_committed = persisted_execution(&workspace, &scope, task_id, child_id).await;
        assert_eq!(
            still_committed.state.status, "completed",
            "a later terminal observer cannot replace the first durable outcome"
        );
        assert!(still_committed
            .refs
            .output_refs
            .iter()
            .any(|candidate| candidate.output_id == "out-child"));
    }

    #[tokio::test]
    async fn late_runtime_signal_cannot_reopen_a_terminal_child() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-terminal-signal";
        let root_id = "exec-root";
        let child_id = "exec-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        let parent = execution_record(task_id, root_id, "running", "2026-08-04T00:00:00Z");
        let child = child_execution_record(
            task_id,
            root_id,
            root_id,
            child_id,
            "completed",
            "2026-08-04T00:02:00Z",
        );
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &parent)
            .await
            .expect("parent");
        reducer
            .reduce_execution_initialized(&mut task, &child)
            .await
            .expect("child");

        let late_children = vec!["late-grandchild".to_string()];
        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: child_id.to_string(),
                },
                Some("running"),
                None,
                Some(&late_children),
                Some("late-step"),
                "2026-08-04T00:04:00Z",
            )
            .await
            .expect("late runtime signal is ignored");

        let committed = persisted_execution(&workspace, &scope, task_id, child_id).await;
        assert_eq!(committed.state.status, "completed");
        assert!(committed.state.active_child_execution_ids.is_empty());
        assert!(committed.state.current_step_id.is_none());
        assert_eq!(committed.state.updated_at, "2026-08-04T00:02:00Z");
    }

    #[tokio::test]
    async fn delegated_child_nonterminal_projection_cannot_replace_task_root_ownership() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-nonterminal";
        let root_id = "exec-root";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        task.state.status = "running".to_string();
        task.state.active_root_execution_id = Some(root_id.to_string());
        task.state.latest_root_execution_id = Some(root_id.to_string());
        reducer.reduce_task_created(&task).await.expect("task");
        let mut child = child_execution_record(
            task_id,
            root_id,
            root_id,
            "exec-child",
            "pending",
            "2026-08-04T00:01:00Z",
        );
        reducer
            .reduce_execution_discovered(&scope, &child)
            .await
            .expect("child discovered");
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "running".to_string(),
            task_status: "running".to_string(),
            outcome_type: "in_progress".to_string(),
            outcome_summary: "child researching".to_string(),
            iterations_used: None,
            is_terminal: false,
            completion_kind: None,
            open_items: Vec::new(),
        };

        reducer
            .reduce_execution_nonterminal(&mut task, &mut child, &outcome)
            .await
            .expect("child nonterminal reduction");

        let persisted = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(persisted.active_root_execution_id.as_deref(), Some(root_id));
        assert_eq!(persisted.latest_root_execution_id.as_deref(), Some(root_id));
    }

    #[tokio::test]
    async fn delegated_child_nonterminal_projection_repairs_persisted_child_ownership() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-repair";
        let root_id = "exec-root";
        let child_id = "exec-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        task.state.status = "running".to_string();
        task.state.active_root_execution_id = Some(child_id.to_string());
        task.state.latest_root_execution_id = Some(child_id.to_string());
        reducer.reduce_task_created(&task).await.expect("task");
        let mut child = child_execution_record(
            task_id,
            root_id,
            root_id,
            child_id,
            "pending",
            "2026-08-04T00:01:00Z",
        );
        reducer
            .reduce_execution_discovered(&scope, &child)
            .await
            .expect("child discovered");
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "running".to_string(),
            task_status: "running".to_string(),
            outcome_type: "in_progress".to_string(),
            outcome_summary: "child researching".to_string(),
            iterations_used: None,
            is_terminal: false,
            completion_kind: None,
            open_items: Vec::new(),
        };

        reducer
            .reduce_execution_nonterminal(&mut task, &mut child, &outcome)
            .await
            .expect("child nonterminal repair");

        let persisted = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(persisted.active_root_execution_id.as_deref(), Some(root_id));
        assert_eq!(persisted.latest_root_execution_id.as_deref(), Some(root_id));
    }

    #[tokio::test]
    async fn delegated_child_terminal_status_cannot_own_task_readiness() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-terminal";
        let root_id = "exec-root";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        task.state.status = "running".to_string();
        task.state.active_root_execution_id = Some(root_id.to_string());
        task.state.latest_root_execution_id = Some(root_id.to_string());
        reducer.reduce_task_created(&task).await.expect("task");
        let mut child = child_execution_record(
            task_id,
            root_id,
            root_id,
            "exec-child",
            "running",
            "2026-08-04T00:01:00Z",
        );
        reducer
            .reduce_execution_discovered(&scope, &child)
            .await
            .expect("child discovered");
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "success".to_string(),
            outcome_summary: "child done".to_string(),
            iterations_used: Some(2),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };

        reducer
            .reduce_execution_terminal_status_only(&mut task, &mut child, &outcome)
            .await
            .expect("child terminal status reduction");

        let persisted = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(persisted.status, "running");
        assert_eq!(persisted.active_root_execution_id.as_deref(), Some(root_id));
        assert!(persisted.synthesis_pending_executions.is_empty());
    }

    #[tokio::test]
    async fn delegated_child_terminal_without_outputs_clears_persisted_pending_state() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-cancelled";
        let root_id = "exec-root";
        let child_id = "exec-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        task.state.status = "running".to_string();
        task.state.active_root_execution_id = Some(root_id.to_string());
        task.state.latest_root_execution_id = Some(root_id.to_string());
        reducer.reduce_task_created(&task).await.expect("task");
        let mut child = child_execution_record(
            task_id,
            root_id,
            root_id,
            child_id,
            "running",
            "2026-08-04T00:01:00Z",
        );
        child.state.synthesis_pending = true;
        reducer
            .reduce_execution_discovered(&scope, &child)
            .await
            .expect("child discovered");
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "cancelled".to_string(),
            task_status: "cancelled".to_string(),
            outcome_type: "cancelled".to_string(),
            outcome_summary: "child cancelled".to_string(),
            iterations_used: None,
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };

        reducer
            .reduce_execution_terminal_without_outputs(&mut task, &mut child, &outcome)
            .await
            .expect("child cancellation settles");

        let persisted_child = persisted_execution(&workspace, &scope, task_id, child_id).await;
        let persisted_task = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert!(!persisted_child.state.synthesis_pending);
        assert_eq!(persisted_child.state.status, "cancelled");
        assert_eq!(persisted_task.status, "running");
        assert_eq!(
            persisted_task.active_root_execution_id.as_deref(),
            Some(root_id)
        );
    }

    #[tokio::test]
    async fn delegated_child_initialization_cannot_replace_the_task_root() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-init-root-guard";
        let root_id = "exec-root";
        let child_id = "exec-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        let root = execution_record(task_id, root_id, "running", "2026-08-04T00:00:01Z");
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &root)
            .await
            .expect("root initialized");

        let child = child_execution_record(
            task_id,
            root_id,
            root_id,
            child_id,
            "running",
            "2026-08-04T00:00:02Z",
        );
        reducer
            .reduce_execution_initialized(&mut task, &child)
            .await
            .expect("child initialization is execution-scoped");

        let persisted = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(persisted.active_root_execution_id.as_deref(), Some(root_id));
        assert_eq!(persisted.latest_root_execution_id.as_deref(), Some(root_id));
    }

    #[tokio::test]
    async fn delegated_child_runtime_signal_cannot_replace_a_newer_root() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-runtime-root-guard";
        let old_root_id = "exec-old-root";
        let new_root_id = "exec-new-root";
        let child_id = "exec-old-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        reducer.reduce_task_created(&task).await.expect("task");
        let old_root = execution_record(task_id, old_root_id, "running", "2026-08-04T00:00:01Z");
        reducer
            .reduce_execution_initialized(&mut task, &old_root)
            .await
            .expect("old root initialized");
        let child = child_execution_record(
            task_id,
            old_root_id,
            old_root_id,
            child_id,
            "running",
            "2026-08-04T00:00:02Z",
        );
        reducer
            .reduce_execution_discovered(&scope, &child)
            .await
            .expect("child discovered");
        let new_root = execution_record(task_id, new_root_id, "running", "2026-08-04T00:00:03Z");
        reducer
            .reduce_execution_discovered(&scope, &new_root)
            .await
            .expect("new root record discovered");
        // The single-root admission guard correctly refuses overlapping root
        // initialization. Model the durable state after the old root settled
        // and a newer root took ownership; this assertion is specifically
        // about a late child signal not replacing that newer ownership.
        task.state.active_root_execution_id = Some(new_root_id.to_owned());
        task.state.latest_root_execution_id = Some(new_root_id.to_owned());
        task.state.status = "running".to_owned();
        reducer
            .persist_task_state_only(&task)
            .await
            .expect("new root ownership persisted");

        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: child_id.to_string(),
                },
                Some("completed"),
                Some("completed"),
                None,
                None,
                "2026-08-04T00:00:04Z",
            )
            .await
            .expect("child runtime signal");

        let persisted = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(
            persisted.active_root_execution_id.as_deref(),
            Some(new_root_id)
        );
        assert_eq!(
            persisted.latest_root_execution_id.as_deref(),
            Some(new_root_id)
        );
        assert_eq!(persisted.status, "running");
    }

    #[tokio::test]
    async fn delegated_child_synthesis_retry_stays_execution_scoped() {
        use super::super::models::{SynthesisFailure, SynthesisStage};

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-child-synthesis-root-guard";
        let root_id = "exec-root";
        let child_id = "exec-child";
        let mut task = task_record(task_id, "2026-08-04T00:00:00Z");
        reducer.reduce_task_created(&task).await.expect("task");
        let root = execution_record(task_id, root_id, "running", "2026-08-04T00:00:01Z");
        reducer
            .reduce_execution_initialized(&mut task, &root)
            .await
            .expect("root initialized");
        let mut child = child_execution_record(
            task_id,
            root_id,
            root_id,
            child_id,
            "completed",
            "2026-08-04T00:00:02Z",
        );
        reducer
            .reduce_execution_discovered(&scope, &child)
            .await
            .expect("child discovered");
        reducer
            .reduce_execution_synthesis_failed(
                &mut task,
                &mut child,
                SynthesisFailure {
                    stage: SynthesisStage::ExecutionOutput,
                    last_error: "child projection failed".to_string(),
                    attempts: 1,
                    failed_at: "2026-08-04T00:00:03Z".to_string(),
                },
            )
            .await
            .expect("child synthesis failure persisted");
        let won = reducer
            .reduce_clear_synthesis_failure(&mut task, &mut child)
            .await
            .expect("child synthesis retry");
        assert!(won);

        let persisted = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert_eq!(persisted.active_root_execution_id.as_deref(), Some(root_id));
        assert_eq!(persisted.latest_root_execution_id.as_deref(), Some(root_id));
        assert!(persisted.synthesis_pending_executions.is_empty());
        assert!(persisted.synthesis_failed_execution_id.is_none());
    }

    #[test]
    fn child_terminal_repair_restores_a_corrupted_root_pointer() {
        let root_id = "exec-root";
        let child = child_execution_record(
            "task-repair",
            root_id,
            root_id,
            "exec-child",
            "completed",
            "2026-08-04T00:01:00Z",
        );
        let mut task = task_record("task-repair", "2026-08-04T00:00:00Z");
        task.state.active_root_execution_id = Some("exec-child".to_string());
        task.state.latest_root_execution_id = Some("exec-child".to_string());

        assert!(FilesystemArtifactV2Reducer::repair_child_root_ownership(
            &mut task, &child
        ));
        assert_eq!(
            task.state.active_root_execution_id.as_deref(),
            Some(root_id)
        );
        assert_eq!(
            task.state.latest_root_execution_id.as_deref(),
            Some(root_id)
        );
    }

    #[test]
    fn child_terminal_repair_clears_ownership_when_root_lineage_is_missing() {
        let mut child = child_execution_record(
            "task-repair-missing-lineage",
            "exec-root",
            "exec-root",
            "exec-child",
            "completed",
            "2026-08-04T00:01:00Z",
        );
        child.state.root_execution_id = None;
        let mut task = task_record("task-repair-missing-lineage", "2026-08-04T00:00:00Z");
        task.state.active_root_execution_id = Some("exec-child".to_string());
        task.state.latest_root_execution_id = Some("exec-child".to_string());

        assert!(FilesystemArtifactV2Reducer::repair_child_root_ownership(
            &mut task, &child
        ));
        assert!(task.state.active_root_execution_id.is_none());
        assert!(task.state.latest_root_execution_id.is_none());
    }

    #[tokio::test]
    async fn execution_index_keeps_latest_entry_per_execution() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        let execution = execution_record(task_id, "exec-1", "running", "2026-03-30T10:00:00Z");

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("initial execution record should persist");

        let updated = execution_record(task_id, "exec-1", "paused", "2026-03-30T10:05:00Z");
        reducer
            .reduce_execution_discovered(&scope, &updated)
            .await
            .expect("updated execution record should replace index entry");

        let index_path =
            workspace.task_executions_index_path(&scope.principal(), &scope.workspace(), task_id);
        let lines: Vec<_> = tokio::fs::read_to_string(index_path)
            .await
            .expect("index file should exist")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 1, "index should keep only one latest entry");

        let entries = reducer
            .list_executions(&scope, task_id)
            .await
            .expect("execution list should load");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].execution_id, "exec-1");
        assert_eq!(entries[0].status, "paused");
        assert_eq!(entries[0].updated_at, "2026-03-30T10:05:00Z");
    }

    #[tokio::test]
    async fn auxiliary_outputs_merge_against_fresh_task_state_and_are_idempotent() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-auxiliary";
        let task = task_record(task_id, "2026-08-04T00:00:00Z");
        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");

        let output = |id: &str, role: &str| OutputRef {
            output_id: id.to_string(),
            scope: "task".to_string(),
            audience: "agent".to_string(),
            role: role.to_string(),
            relative_path: format!("outputs/{id}.json"),
            media_type: "application/json".to_string(),
            created_at: "2026-08-04T00:01:00Z".to_string(),
            source_execution_id: Some("exec-auxiliary".to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let continuation = output("out-continuation", CONTINUATION_CONTEXT_OUTPUT_ROLE);
        let media = output("out-media", "user_media");

        reducer
            .reduce_task_auxiliary_output(&scope, task_id, &continuation)
            .await
            .expect("continuation should attach");
        reducer
            .reduce_task_auxiliary_output(&scope, task_id, &media)
            .await
            .expect("media should merge with continuation");
        reducer
            .reduce_task_auxiliary_output(&scope, task_id, &continuation)
            .await
            .expect("replayed continuation should upsert");

        let refs = read_json::<TaskRefs>(&workspace.task_refs_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task refs should load");
        assert_eq!(refs.outputs.len(), 2, "replays must not duplicate refs");
        assert!(refs
            .outputs
            .iter()
            .any(|candidate| candidate.output_id == continuation.output_id));
        assert!(refs
            .outputs
            .iter()
            .any(|candidate| candidate.output_id == media.output_id));
    }

    #[tokio::test]
    async fn reduce_runtime_signal_clears_active_root_for_terminal_root_execution() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        let execution = execution_record(task_id, "exec-1", "running", "2026-03-30T10:00:00Z");

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("initial execution record should persist");

        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: "exec-1".to_string(),
                },
                Some("cancelled"),
                Some("cancelled"),
                None,
                None,
                "2026-03-30T10:05:00Z",
            )
            .await
            .expect("terminal runtime signal should persist");

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load");
        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            "exec-1",
        ))
        .await
        .expect("execution state should load");

        assert_eq!(task_state.status, "cancelled");
        assert!(task_state.active_root_execution_id.is_none());
        assert_eq!(
            task_state.latest_root_execution_id.as_deref(),
            Some("exec-1")
        );
        assert_eq!(execution_state.status, "cancelled");
    }

    /// Readiness has to mean readable.
    ///
    /// The runtime status signal is the write that flips an execution to
    /// `completed`; the synthesis pipeline attaches the deliverable in a later
    /// reduction. A consumer that treats "execution terminal and nothing
    /// pending" as ready therefore had a window in which it read an answer
    /// that had not been written yet. The marker must exist from the first
    /// moment the execution looks terminal.
    #[tokio::test]
    async fn a_root_execution_that_looks_completed_already_carries_its_synthesis_marker() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-readiness";
        let scope = test_scope();
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        let execution = execution_record(task_id, "exec-1", "running", "2026-03-30T10:00:00Z");

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("initial execution record should persist");

        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: "exec-1".to_string(),
                },
                Some("completed"),
                None,
                None,
                None,
                "2026-03-30T10:05:00Z",
            )
            .await
            .expect("completed runtime signal should persist");

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load");
        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            "exec-1",
        ))
        .await
        .expect("execution state should load");

        assert_eq!(execution_state.status, "completed");
        assert!(
            execution_state.synthesis_pending,
            "the execution that just went completed still owes a deliverable"
        );
        assert_eq!(
            task_state.synthesis_pending_executions,
            vec!["exec-1".to_string()],
            "a reader polling the task must see the deliverable is still in flight"
        );
    }

    /// The marker is registered on the transition INTO a terminal status, not
    /// on every signal that names one. A duplicate or late `completed` signal
    /// arriving after synthesis already settled must not re-open the task —
    /// nothing would clear the marker a second time.
    #[tokio::test]
    async fn a_late_completed_signal_does_not_reopen_a_settled_execution() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-late-signal";
        let scope = test_scope();
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        let settled = execution_record(task_id, "exec-1", "completed", "2026-03-30T10:00:00Z");

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &settled)
            .await
            .expect("settled execution record should persist");

        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: "exec-1".to_string(),
                },
                Some("completed"),
                None,
                None,
                None,
                "2026-03-30T10:05:00Z",
            )
            .await
            .expect("late runtime signal should persist");

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load");
        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            "exec-1",
        ))
        .await
        .expect("execution state should load");

        assert!(
            !execution_state.synthesis_pending,
            "a settled execution owes nothing"
        );
        assert!(
            task_state.synthesis_pending_executions.is_empty(),
            "a late signal must not park the task behind a marker nobody clears"
        );
    }

    #[tokio::test]
    async fn reduce_runtime_signal_without_task_status_does_not_overwrite_task_status() {
        // End-to-end contract for the terminal-ordering fix: the realtime-event
        // bridge now suppresses the terminal TASK status hint (passes `None`) while
        // still advancing the EXECUTION status. The runtime-signal reducer must
        // advance the execution but leave `task.state.status` untouched, so the
        // authoritative 2-step terminal reducer (post-synthesis) owns the terminal
        // task write and the task is never marked completed before its outputs land.
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        let execution = execution_record(task_id, "exec-1", "running", "2026-03-30T10:00:00Z");

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("initial execution record should persist");

        let prior_task_status = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load")
        .status;

        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: "exec-1".to_string(),
                },
                Some("completed"),
                None,
                None,
                None,
                "2026-03-30T10:05:00Z",
            )
            .await
            .expect("runtime signal should persist");

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load");
        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            "exec-1",
        ))
        .await
        .expect("execution state should load");

        assert_eq!(
            execution_state.status, "completed",
            "execution status still advances on the runtime signal"
        );
        assert_ne!(
            task_state.status, "completed",
            "task status must NOT be flipped terminal by the realtime-signal path"
        );
        assert_eq!(
            task_state.status, prior_task_status,
            "task status is left exactly as the authoritative path last set it"
        );
    }

    #[tokio::test]
    async fn reduce_runtime_signal_clears_active_root_for_ready_root_execution() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        let execution = execution_record(task_id, "exec-1", "running", "2026-03-30T10:00:00Z");

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("initial execution record should persist");

        reducer
            .reduce_runtime_signal(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: "exec-1".to_string(),
                },
                Some("ready"),
                Some("ready"),
                None,
                None,
                "2026-03-30T10:05:00Z",
            )
            .await
            .expect("ready runtime signal should persist");

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load");
        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            "exec-1",
        ))
        .await
        .expect("execution state should load");

        assert_eq!(task_state.status, "ready");
        assert!(task_state.active_root_execution_id.is_none());
        assert_eq!(
            task_state.latest_root_execution_id.as_deref(),
            Some("exec-1")
        );
        assert_eq!(execution_state.status, "ready");
    }

    #[tokio::test]
    async fn reduce_execution_terminal_overwrite_prunes_stale_primary_task_outputs() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace);
        let task_id = "task-1";
        let mut task = task_record(task_id, "2026-03-30T10:00:00Z");
        task.refs.outputs = vec![
            OutputRef {
                output_id: "out_task_agent_exec-old".to_string(),
                scope: "task".to_string(),
                audience: "agent".to_string(),
                role: "primary_task_agent".to_string(),
                relative_path: "outputs/old-agent.md".to_string(),
                media_type: "text/markdown".to_string(),
                created_at: "2026-03-30T09:00:00Z".to_string(),
                source_execution_id: Some("exec-old".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            OutputRef {
                output_id: "out_task_user_exec-old".to_string(),
                scope: "task".to_string(),
                audience: "user".to_string(),
                role: "primary_task_user".to_string(),
                relative_path: "outputs/old-user.md".to_string(),
                media_type: "text/markdown".to_string(),
                created_at: "2026-03-30T09:00:00Z".to_string(),
                source_execution_id: Some("exec-old".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            OutputRef {
                output_id: "out_task_continuation_exec-old".to_string(),
                scope: "task".to_string(),
                audience: "agent".to_string(),
                role: CONTINUATION_CONTEXT_OUTPUT_ROLE.to_string(),
                relative_path: "outputs/old-continuation.json".to_string(),
                media_type: "application/json".to_string(),
                created_at: "2026-03-30T09:00:00Z".to_string(),
                source_execution_id: Some("exec-old".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            OutputRef {
                output_id: "out_other_attachment".to_string(),
                scope: "task".to_string(),
                audience: "user".to_string(),
                role: "attachment".to_string(),
                relative_path: "outputs/attachment.txt".to_string(),
                media_type: "text/plain".to_string(),
                created_at: "2026-03-30T09:00:00Z".to_string(),
                source_execution_id: Some("exec-old".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
        ];
        let execution = execution_record(task_id, "exec-1", "running", "2026-03-30T10:00:00Z");
        let mut execution = execution;
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "success".to_string(),
            outcome_summary: "done".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        let outputs = FinalizedOutputs {
            execution_output: OutputRef {
                output_id: "out_exec_1".to_string(),
                scope: "execution".to_string(),
                audience: "agent".to_string(),
                role: "primary_execution".to_string(),
                relative_path: "outputs/execution.md".to_string(),
                media_type: "text/markdown".to_string(),
                created_at: "2026-03-30T10:00:00Z".to_string(),
                source_execution_id: Some("exec-1".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            task_agent_output: OutputRef {
                output_id: "out_task_agent_task-1".to_string(),
                scope: "task".to_string(),
                audience: "agent".to_string(),
                role: "primary_task_agent".to_string(),
                relative_path: "outputs/task-agent.md".to_string(),
                media_type: "text/markdown".to_string(),
                created_at: "2026-03-30T10:00:00Z".to_string(),
                source_execution_id: Some("exec-1".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            task_user_output: OutputRef {
                output_id: "out_task_user_task-1".to_string(),
                scope: "task".to_string(),
                audience: "user".to_string(),
                role: "primary_task_user".to_string(),
                relative_path: "outputs/task-user.md".to_string(),
                media_type: "text/markdown".to_string(),
                created_at: "2026-03-30T10:00:00Z".to_string(),
                source_execution_id: Some("exec-1".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            continuation_context_output: Some(OutputRef {
                output_id: "out_task_continuation_task-1".to_string(),
                scope: "task".to_string(),
                audience: "agent".to_string(),
                role: CONTINUATION_CONTEXT_OUTPUT_ROLE.to_string(),
                relative_path: "outputs/task-continuation.json".to_string(),
                media_type: "application/json".to_string(),
                created_at: "2026-03-30T10:00:00Z".to_string(),
                source_execution_id: Some("exec-1".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            }),
            media_outputs: Vec::new(),
            task_output_mode: TaskOutputMode::Overwrite,
        };

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution should persist");
        reducer
            .reduce_execution_terminal(&mut task, &mut execution, &outcome, &outputs)
            .await
            .expect("terminal reduction should succeed");

        let output_ids: Vec<_> = task
            .refs
            .outputs
            .iter()
            .map(|output| output.output_id.as_str())
            .collect();
        assert!(output_ids.contains(&"out_task_agent_task-1"));
        assert!(output_ids.contains(&"out_task_user_task-1"));
        assert!(output_ids.contains(&"out_task_continuation_task-1"));
        assert!(output_ids.contains(&"out_other_attachment"));
        assert!(!output_ids.contains(&"out_task_agent_exec-old"));
        assert!(!output_ids.contains(&"out_task_user_exec-old"));
        assert!(!output_ids.contains(&"out_task_continuation_exec-old"));
    }

    /// Step 1 atomic write: status flips + synthesis_pending=true; no
    /// task-state changes beyond marking synthesis_pending; outputs
    /// stay untouched.
    #[tokio::test]
    async fn reduce_execution_terminal_status_only_sets_synthesis_pending() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-step1";
        let execution_id = "exec-step1";

        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        let mut execution =
            execution_record(task_id, execution_id, "running", "2026-05-24T00:00:00Z");
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution init");

        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "completed".to_string(),
            outcome_summary: "ok".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        execution.state.updated_at = "2026-05-24T00:01:00Z".to_string();
        execution.state.completed_at = Some("2026-05-24T00:01:00Z".to_string());

        reducer
            .reduce_execution_terminal_status_only(&mut task, &mut execution, &outcome)
            .await
            .expect("step 1 atomic write");

        // Execution disk record reflects terminal + synthesis_pending.
        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        ))
        .await
        .expect("execution state");
        assert_eq!(execution_state.status, "completed");
        assert!(
            execution_state.synthesis_pending,
            "synthesis_pending must be true after Step 1"
        );
        assert!(execution_state.synthesis_failed.is_none());

        // Task surfaces synthesis_pending; active_root NOT cleared (Step 2
        // owns that — `reduce_execution_terminal` clears it after synthesis).
        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert!(
            task_state.synthesis_pending(),
            "task.synthesis_pending() must be true after Step 1"
        );
        assert!(
            task_state
                .synthesis_pending_executions
                .contains(&execution_id.to_string()),
            "synthesis_pending_executions must contain this execution"
        );
        assert!(task_state.synthesis_failed_execution_id.is_none());
        assert_eq!(
            task_state.active_root_execution_id.as_deref(),
            Some(execution_id),
            "Step 1 must leave active_root in place so Step 2 can attach primary output ids"
        );
    }

    /// Permanent-failure path: synthesis exhausted retries; reducer
    /// clears synthesis_pending, stamps the failure on the execution,
    /// and surfaces the execution_id on the task.
    #[tokio::test]
    async fn reduce_execution_synthesis_failed_persists_failure() {
        use super::super::models::{SynthesisFailure, SynthesisStage};

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-synth-fail";
        let execution_id = "exec-synth-fail";

        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        let mut execution =
            execution_record(task_id, execution_id, "completed", "2026-05-24T00:01:00Z");
        execution.state.synthesis_pending = true;
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution init");

        let failure = SynthesisFailure {
            stage: SynthesisStage::TaskAgentOutput,
            last_error: "openai 429 rate-limited 3x".to_string(),
            attempts: 3,
            failed_at: "2026-05-24T00:05:00Z".to_string(),
        };
        reducer
            .reduce_execution_synthesis_failed(&mut task, &mut execution, failure.clone())
            .await
            .expect("synthesis failure write");

        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        ))
        .await
        .expect("execution state");
        assert!(!execution_state.synthesis_pending);
        assert_eq!(execution_state.synthesis_failed.as_ref(), Some(&failure));

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert!(!task_state.synthesis_pending());
        assert!(
            !task_state
                .synthesis_pending_executions
                .contains(&execution_id.to_string()),
            "this execution must be removed from the in-flight registry on failure"
        );
        assert_eq!(
            task_state.synthesis_failed_execution_id.as_deref(),
            Some(execution_id)
        );
    }

    /// `reduce_execution_terminal` (the post-synthesis reducer) clears
    /// `synthesis_pending` on both the execution and the task, so the
    /// final on-disk state reflects "synthesis is done".
    #[tokio::test]
    async fn reduce_execution_terminal_clears_synthesis_pending() {
        use crate::magician_v2::artifact_v2::models::{FinalizedOutputs, OutputRef};

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-step2";
        let execution_id = "exec-step2";

        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        let mut execution =
            execution_record(task_id, execution_id, "completed", "2026-05-24T00:01:00Z");
        execution.state.synthesis_pending = true;
        task.state
            .synthesis_pending_executions
            .push(execution_id.to_string());
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution init");

        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "completed".to_string(),
            outcome_summary: "ok".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        let make_ref = |id: &str, audience: &str| OutputRef {
            output_id: id.to_string(),
            scope: "execution".to_string(),
            audience: audience.to_string(),
            role: "primary".to_string(),
            relative_path: format!("outputs/{id}.txt"),
            media_type: "text/plain".to_string(),
            created_at: "2026-05-24T00:02:00Z".to_string(),
            source_execution_id: Some(execution_id.to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let outputs = FinalizedOutputs {
            execution_output: make_ref("out_exec", "agent"),
            task_agent_output: make_ref("out_task_agent", "agent"),
            task_user_output: make_ref("out_task_user", "user"),
            continuation_context_output: None,
            media_outputs: Vec::new(),
            task_output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
        };

        reducer
            .reduce_execution_terminal(&mut task, &mut execution, &outcome, &outputs)
            .await
            .expect("terminal reducer");

        let execution_state = read_json::<ExecutionState>(&workspace.execution_state_path(
            &test_scope().principal(),
            &test_scope().workspace(),
            task_id,
            execution_id,
        ))
        .await
        .expect("execution state");
        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &test_scope().principal(),
            &test_scope().workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert!(
            !execution_state.synthesis_pending,
            "synthesis_pending must clear after Step 2"
        );
        assert!(!task_state.synthesis_pending());
        assert!(
            task_state.synthesis_pending_executions.is_empty(),
            "in-flight registry must be empty after Step 2"
        );
        assert_eq!(task_state.synthesis_failed_execution_id, None);
    }

    /// A root remains the active owner through its synthesis phase. A second
    /// root cannot overlap it; once synthesis settles, the successor may start.
    #[tokio::test]
    async fn synthesis_pending_root_blocks_an_overlapping_run() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-multi";
        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        reducer.reduce_task_created(&task).await.expect("task");

        // Run A
        let mut exec_a = execution_record(task_id, "exec-a", "running", "2026-05-24T00:00:00Z");
        reducer
            .reduce_execution_initialized(&mut task, &exec_a)
            .await
            .expect("init A");
        exec_a.state.updated_at = "2026-05-24T00:01:00Z".to_string();
        exec_a.state.completed_at = Some("2026-05-24T00:01:00Z".to_string());
        let outcome_a = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "completed".to_string(),
            outcome_summary: "ok".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        reducer
            .reduce_execution_terminal_status_only(&mut task, &mut exec_a, &outcome_a)
            .await
            .expect("step 1 A");

        // Run B is refused while Run A's canonical synthesis is pending.
        let exec_b = execution_record(task_id, "exec-b", "running", "2026-05-24T00:02:00Z");
        let error = reducer
            .reduce_execution_initialized(&mut task, &exec_b)
            .await
            .expect_err("overlapping root must be refused");
        assert!(matches!(
            error,
            ArtifactV2Error::InvalidRequest(reason)
                if reason == "task_execution_in_progress:exec-a"
        ));

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert!(
            task_state.synthesis_pending(),
            "Run A synthesis must remain pending"
        );
        assert!(
            task_state
                .synthesis_pending_executions
                .contains(&"exec-a".to_string()),
            "exec-a must remain in the registry while its synthesis is in flight"
        );
        assert!(!task_state
            .synthesis_pending_executions
            .contains(&"exec-b".to_string()));

        // Simulate exec-a Step 2 (synthesis-failed path here for
        // simplicity — same removal semantic). Reload task to mirror
        // what the spawn does.
        let manifest = read_json::<crate::magician_v2::artifact_v2::models::TaskManifest>(
            &workspace.task_manifest_path(&scope.principal(), &scope.workspace(), task_id),
        )
        .await
        .expect("reload manifest");
        let mut fresh_task = TaskRecord {
            manifest,
            state: task_state.clone(),
            refs: TaskRefs::default(),
        };
        // Use the synthesis_failed reducer to remove exec-a only.
        let failure = crate::magician_v2::artifact_v2::models::SynthesisFailure {
            stage: crate::magician_v2::artifact_v2::models::SynthesisStage::ExecutionOutput,
            last_error: "test".to_string(),
            attempts: 3,
            failed_at: "2026-05-24T00:04:00Z".to_string(),
        };
        reducer
            .reduce_execution_synthesis_failed(&mut fresh_task, &mut exec_a, failure)
            .await
            .expect("exec-a synthesis failed");

        let task_state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state");
        assert!(
            !task_state
                .synthesis_pending_executions
                .contains(&"exec-a".to_string()),
            "exec-a must be removed from registry after its synthesis failed"
        );
        assert!(!task_state.synthesis_pending());

        // Synthesis failure settles the in-flight synthesis counter, but the
        // failed root deliberately remains the task owner so an operator can
        // retry that exact execution. A successor must not skip the unresolved
        // output and replace its authority merely because synthesis stopped.
        let manifest = read_json::<crate::magician_v2::artifact_v2::models::TaskManifest>(
            &workspace.task_manifest_path(&scope.principal(), &scope.workspace(), task_id),
        )
        .await
        .expect("reload manifest");
        let mut settled_task = TaskRecord {
            manifest,
            state: task_state,
            refs: TaskRefs::default(),
        };
        let error = reducer
            .reduce_execution_initialized(&mut settled_task, &exec_b)
            .await
            .expect_err("successor root stays blocked by the failed active root");
        assert!(matches!(
            error,
            ArtifactV2Error::InvalidRequest(reason)
                if reason == "task_execution_in_progress:exec-a"
        ));
    }

    /// Retry-synthesis path: an execution with `synthesis_failed`
    /// gets its failure cleared and rejoins the in-flight registry.
    /// Mirrors what `retry_synthesis_for_execution` does
    /// (pre-spawn) and what the spawn does on Step 2.
    #[tokio::test]
    async fn reduce_clear_synthesis_failure_restores_pending() {
        use super::super::models::{SynthesisFailure, SynthesisStage};

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-retry";
        let execution_id = "exec-retry";

        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        let mut execution =
            execution_record(task_id, execution_id, "completed", "2026-05-24T00:01:00Z");
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("init");

        // Land into the synthesis_failed state.
        let failure = SynthesisFailure {
            stage: SynthesisStage::TaskUserOutput,
            last_error: "openai 500".to_string(),
            attempts: 3,
            failed_at: "2026-05-24T00:02:00Z".to_string(),
        };
        reducer
            .reduce_execution_synthesis_failed(&mut task, &mut execution, failure)
            .await
            .expect("synthesis failed");

        // Sanity: failure marker present, registry empty.
        let task_state_pre = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state pre");
        assert_eq!(
            task_state_pre.synthesis_failed_execution_id.as_deref(),
            Some(execution_id)
        );
        assert!(task_state_pre.synthesis_pending_executions.is_empty());

        // Retry path: clear the failure marker, put execution back
        // into the registry. Reload fresh state to mirror what the
        // service method does.
        let mut fresh_task = task.clone();
        let mut fresh_execution = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        ))
        .await
        .map(|state| ExecutionRecord {
            state,
            refs: ExecutionRefs {
                execution_id: execution_id.to_string(),
                ..Default::default()
            },
        })
        .expect("reload execution");
        let won = reducer
            .reduce_clear_synthesis_failure(&mut fresh_task, &mut fresh_execution)
            .await
            .expect("clear synthesis failure");
        assert!(won, "first retry must win the race and return true");

        // Post-retry: failure cleared, registry contains this execution.
        let task_state_post = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state post");
        assert_eq!(task_state_post.synthesis_failed_execution_id, None);
        assert!(
            task_state_post
                .synthesis_pending_executions
                .contains(&execution_id.to_string()),
            "retry must re-register the execution"
        );
        assert!(task_state_post.synthesis_pending());

        let execution_state_post = read_json::<ExecutionState>(&workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        ))
        .await
        .expect("execution state post");
        assert!(execution_state_post.synthesis_pending);
        assert!(execution_state_post.synthesis_failed.is_none());
    }

    /// Bug 22 regression test: a second `reduce_clear_synthesis_failure`
    /// call on the same execution (double-click retry, browser-retry,
    /// multi-tab) MUST observe the cleared marker and return `false`
    /// so the caller skips the spawn. Without this guard, two
    /// synthesis pipelines would fire in parallel for the same
    /// execution.
    #[tokio::test]
    async fn reduce_clear_synthesis_failure_idempotent_on_concurrent_retry() {
        use super::super::models::{SynthesisFailure, SynthesisStage};

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-double-retry";
        let execution_id = "exec-double-retry";

        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        let mut execution =
            execution_record(task_id, execution_id, "completed", "2026-05-24T00:01:00Z");
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("init");

        // Land in synthesis_failed.
        let failure = SynthesisFailure {
            stage: SynthesisStage::ExecutionOutput,
            last_error: "test".to_string(),
            attempts: 3,
            failed_at: "2026-05-24T00:02:00Z".to_string(),
        };
        reducer
            .reduce_execution_synthesis_failed(&mut task, &mut execution, failure)
            .await
            .expect("synthesis failed");

        // First retry — wins the race.
        let mut task_a = task.clone();
        let mut execution_a = execution.clone();
        let won_a = reducer
            .reduce_clear_synthesis_failure(&mut task_a, &mut execution_a)
            .await
            .expect("first retry");
        assert!(won_a, "first retry must win");

        // Second retry on the SAME starting in-memory clones (which
        // are stale — synthesis_failed=Some, but on-disk is None).
        // This simulates two concurrent retry callers both passing
        // the precondition check before either acquired the lock.
        let mut task_b = task.clone();
        let mut execution_b = execution.clone();
        let won_b = reducer
            .reduce_clear_synthesis_failure(&mut task_b, &mut execution_b)
            .await
            .expect("second retry should not error");
        assert!(
            !won_b,
            "second concurrent retry must observe the cleared marker and return false"
        );
    }

    /// Bug 25 regression test: a manifest edit (rename / retag / etc.)
    /// landing between Step 1 and Step 2 (or any late reducer) must
    /// survive the reducer's writeback. Before the fix, the reducer
    /// wrote the FULL TaskRecord including the stale manifest from
    /// before the rename — silently clobbering the operator's edit.
    /// With `task_state_and_refs_writes`, the manifest file is never
    /// touched by the reducer, so concurrent edits land safely.
    #[tokio::test]
    async fn late_reducer_does_not_clobber_concurrent_manifest_edit() {
        use crate::magician_v2::artifact_v2::io::write_json_atomic;
        use crate::magician_v2::artifact_v2::models::{FinalizedOutputs, OutputRef, TaskManifest};

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-rename";
        let execution_id = "exec-rename";

        let mut task = task_record(task_id, "2026-05-24T00:00:00Z");
        let mut execution =
            execution_record(task_id, execution_id, "completed", "2026-05-24T00:01:00Z");
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("init");

        // Caller takes a clone of task at this point. Original
        // title is "Sample Task".
        let original_title = task.manifest.title.clone();

        // Step 1 — atomic terminal-status write. (Spawn would begin
        // here in production; we simulate by NOT running Step 2 yet.)
        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "completed".to_string(),
            outcome_summary: "ok".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        execution.state.updated_at = "2026-05-24T00:01:30Z".to_string();
        execution.state.completed_at = Some("2026-05-24T00:01:30Z".to_string());
        reducer
            .reduce_execution_terminal_status_only(&mut task, &mut execution, &outcome)
            .await
            .expect("step 1");

        // === Concurrent manifest edit lands here ===
        // Simulate by directly writing the new manifest to disk
        // (representing what an UpdateTaskInput call would do mid-
        // synthesis).
        let mut fresh_manifest = read_json::<TaskManifest>(&workspace.task_manifest_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("reload manifest");
        let new_title = "Renamed By Operator";
        fresh_manifest.title = new_title.to_string();
        fresh_manifest.updated_at = "2026-05-24T00:02:00Z".to_string();
        write_json_atomic(
            &workspace.task_manifest_path(&scope.principal(), &scope.workspace(), task_id),
            &fresh_manifest,
        )
        .await
        .expect("write fresh manifest");

        // Caller's in-memory `task.manifest.title` is still the
        // original — exactly the stale-clone shape Bug 25 captures.
        assert_eq!(task.manifest.title, original_title);

        // === Step 2 — reduce_execution_terminal fires from the spawn ===
        let make_ref = |id: &str, audience: &str| OutputRef {
            output_id: id.to_string(),
            scope: "execution".to_string(),
            audience: audience.to_string(),
            role: "primary".to_string(),
            relative_path: format!("outputs/{id}.txt"),
            media_type: "text/plain".to_string(),
            created_at: "2026-05-24T00:03:00Z".to_string(),
            source_execution_id: Some(execution_id.to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let outputs = FinalizedOutputs {
            execution_output: make_ref("out_exec", "agent"),
            task_agent_output: make_ref("out_task_agent", "agent"),
            task_user_output: make_ref("out_task_user", "user"),
            continuation_context_output: None,
            media_outputs: Vec::new(),
            task_output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
        };
        reducer
            .reduce_execution_terminal(&mut task, &mut execution, &outcome, &outputs)
            .await
            .expect("terminal reducer");

        // === Verification ===
        // Read the manifest from disk — it must still have the
        // operator's rename, NOT the stale title from the caller's
        // clone.
        let manifest_after = read_json::<TaskManifest>(&workspace.task_manifest_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("reload manifest after step 2");
        assert_eq!(
            manifest_after.title, new_title,
            "concurrent manifest rename must survive the late reducer's writeback"
        );

        // Sanity: state still reflects the terminal outcome.
        let state_after = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("reload state");
        assert_eq!(state_after.status, "completed");
        assert!(state_after.synthesis_pending_executions.is_empty());
    }

    fn minimal_finalized_outputs(task_id: &str, exec_id: &str, ts: &str) -> FinalizedOutputs {
        let out = |id: String, scope: &str, audience: &str, role: &str, path: &str| OutputRef {
            output_id: id,
            scope: scope.to_string(),
            audience: audience.to_string(),
            role: role.to_string(),
            relative_path: path.to_string(),
            media_type: "text/markdown".to_string(),
            created_at: ts.to_string(),
            source_execution_id: Some(exec_id.to_string()),
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        FinalizedOutputs {
            execution_output: out(
                format!("out_exec_{exec_id}"),
                "execution",
                "agent",
                "primary_execution",
                "outputs/execution.md",
            ),
            task_agent_output: out(
                format!("out_task_agent_{task_id}"),
                "task",
                "agent",
                "primary_task_agent",
                "outputs/task-agent.md",
            ),
            task_user_output: out(
                format!("out_task_user_{task_id}"),
                "task",
                "user",
                "primary_task_user",
                "outputs/task-user.md",
            ),
            continuation_context_output: None,
            media_outputs: Vec::new(),
            task_output_mode: TaskOutputMode::Accumulate,
        }
    }

    /// Hardening (the re-open race): a terminal ROOT execution finalizes the task
    /// even when a prior terminal pass already nulled `active_root_execution_id`
    /// (what the V3-resume double-run did). Pre-fix, the second pass saw
    /// `active_root != this` and skipped finalization, stranding the task at
    /// "running" — see docs/plans/2026-06-17-vibedev-v3-resume-double-run.md.
    #[tokio::test]
    async fn reduce_execution_terminal_finalizes_when_active_root_nulled_by_reopen() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-reopen";
        let exec_id = "exec-reopen";
        let ts = "2026-06-17T10:00:00Z";

        let mut task = task_record(task_id, ts);
        let mut execution = execution_record(task_id, exec_id, "running", ts);
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("init");

        // Re-open race: a prior terminal pass nulled active_root, but this IS still
        // the task's latest root and has now reached terminal.
        task.state.active_root_execution_id = None;
        task.state.latest_root_execution_id = Some(exec_id.to_string());
        task.state.status = "running".to_string();
        execution.state.status = "completed".to_string();
        execution.state.completed_at = Some(ts.to_string());

        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "completed".to_string(),
            outcome_summary: "done".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        reducer
            .reduce_execution_terminal(
                &mut task,
                &mut execution,
                &outcome,
                &minimal_finalized_outputs(task_id, exec_id, ts),
            )
            .await
            .expect("terminal");

        let state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("state");
        assert_eq!(
            state.status, "completed",
            "a terminal root must finalize the task even after a re-open nulled active_root"
        );
        assert!(state.active_root_execution_id.is_none());
        assert_eq!(
            state.last_completed_root_execution_id.as_deref(),
            Some(exec_id)
        );
    }

    /// Hardening must NOT clobber a NEWER run: if a DIFFERENT execution holds the
    /// active root, a late terminal pass for the OLD root leaves the task alone.
    #[tokio::test]
    async fn reduce_execution_terminal_does_not_clobber_when_newer_run_took_over() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let scope = test_scope();
        let task_id = "task-newer";
        let old_exec = "exec-old";
        let ts = "2026-06-17T10:00:00Z";

        let mut task = task_record(task_id, ts);
        let mut execution = execution_record(task_id, old_exec, "running", ts);
        reducer.reduce_task_created(&task).await.expect("task");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("init");

        // A NEWER execution has taken over the task's active root.
        task.state.active_root_execution_id = Some("exec-newer".to_string());
        task.state.latest_root_execution_id = Some("exec-newer".to_string());
        task.state.status = "running".to_string();
        execution.state.status = "completed".to_string();
        execution.state.completed_at = Some(ts.to_string());

        let outcome = ExecutionOutcomeSnapshot {
            execution_status: "completed".to_string(),
            task_status: "completed".to_string(),
            outcome_type: "completed".to_string(),
            outcome_summary: "done".to_string(),
            iterations_used: Some(1),
            is_terminal: true,
            completion_kind: None,
            open_items: Vec::new(),
        };
        reducer
            .reduce_execution_terminal(
                &mut task,
                &mut execution,
                &outcome,
                &minimal_finalized_outputs(task_id, old_exec, ts),
            )
            .await
            .expect("terminal");

        let state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("state");
        assert_eq!(
            state.status, "running",
            "a late terminal for the OLD root must not clobber the newer run's task status"
        );
        assert_eq!(
            state.active_root_execution_id.as_deref(),
            Some("exec-newer"),
            "the newer run's active_root must be preserved"
        );
    }

    /// The specification for `last_progress_at`, and the reason it exists as a
    /// separate field at all.
    ///
    /// `updated_at` moves on ANY write to the task record — and the realtime
    /// bridge calls `reduce_runtime_signal` for every mirrored runtime event, so
    /// a wedged run rewrites it continuously. Stall detection reading
    /// `updated_at` would therefore be refreshed by the wedge itself and could
    /// never fire: an indicator that reports writes while appearing to report
    /// life.
    ///
    /// Every instant below is distinct, and the heartbeat's effect on
    /// `updated_at` is asserted alongside its NON-effect on `last_progress_at`.
    /// Without that pairing the test could not tell "the guard held" from "the
    /// heartbeat never wrote anything", and a fixture where the two fields
    /// happened to share a timestamp would pass with the guard removed.
    #[tokio::test]
    async fn last_progress_at_moves_on_step_transitions_and_not_on_heartbeats() {
        const STARTED_AT: &str = "2026-07-29T10:00:00Z";
        const HEARTBEAT_AT: &str = "2026-07-29T10:04:00Z";
        const STEP_FINISHED_AT: &str = "2026-07-29T10:07:00Z";
        const LATE_HEARTBEAT_AT: &str = "2026-07-29T10:31:00Z";

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let ctx = ExecutionContext {
            principal: scope.principal().to_string(),
            workspace: scope.workspace().to_string(),
            task_id: task_id.to_string(),
            execution_id: "exec-1".to_string(),
        };
        let mut task = task_record(task_id, STARTED_AT);
        let execution = execution_record(task_id, "exec-1", "running", STARTED_AT);
        let state_path = workspace.task_state_path(&scope.principal(), &scope.workspace(), task_id);

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        assert!(
            read_json::<TaskState>(&state_path)
                .await
                .expect("task state should load")
                .last_progress_at
                .is_none(),
            "a task that has never run has no progress instant — absent, never a sentinel"
        );

        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution should initialize");
        assert_eq!(
            read_json::<TaskState>(&state_path)
                .await
                .expect("task state should load")
                .last_progress_at
                .as_deref(),
            Some(STARTED_AT),
            "starting the run starts its first step, which IS progress — without this \
             a run that wedges inside step one could never be reported as stalled"
        );

        // A heartbeat: the runtime re-asserting status with no step transition.
        // This is the write the whole field exists to be immune to.
        reducer
            .reduce_runtime_signal(
                &scope,
                &ctx,
                Some("running"),
                Some("running"),
                None,
                None,
                HEARTBEAT_AT,
            )
            .await
            .expect("heartbeat should persist");
        let state = read_json::<TaskState>(&state_path)
            .await
            .expect("task state should load");
        assert_eq!(
            state.updated_at, HEARTBEAT_AT,
            "the heartbeat must genuinely have written the record; if it did not, \
             the assertion below would prove nothing"
        );
        assert_eq!(
            state.last_progress_at.as_deref(),
            Some(STARTED_AT),
            "a status re-assert is not progress — pointing this field at updated_at \
             is exactly the defect this test exists to catch"
        );

        // A step finishing IS the run advancing.
        reducer
            .reduce_step_event(
                &scope,
                &ctx,
                crate::magician_v2::artifact_v2::events::ArtifactV2EventType::StepCompleted,
                "step-1",
                STEP_FINISHED_AT,
            )
            .await
            .expect("step completion should persist");
        let state = read_json::<TaskState>(&state_path)
            .await
            .expect("task state should load");
        assert_eq!(
            state.last_progress_at.as_deref(),
            Some(STEP_FINISHED_AT),
            "a completed step must move the progress instant"
        );
        assert_ne!(
            state.last_progress_at.as_deref(),
            Some(STARTED_AT),
            "the progress instant must have actually changed, not merely been rewritten"
        );

        // 24 minutes of heartbeats later, still no step transition. The silence
        // must remain visible: this is the stalled run the panel has to report.
        reducer
            .reduce_runtime_signal(
                &scope,
                &ctx,
                Some("running"),
                Some("running"),
                None,
                None,
                LATE_HEARTBEAT_AT,
            )
            .await
            .expect("late heartbeat should persist");
        let state = read_json::<TaskState>(&state_path)
            .await
            .expect("task state should load");
        assert_eq!(
            state.updated_at, LATE_HEARTBEAT_AT,
            "the wedged run keeps writing — that is the premise"
        );
        assert_eq!(
            state.last_progress_at.as_deref(),
            Some(STEP_FINISHED_AT),
            "and none of that writing counts as progress"
        );
    }

    /// A direct (flat-loop) run has no taskplan steps: its unit of
    /// advancement is an action that settled with a result. A WorkFlowy CUA
    /// run made 35 tool calls over 6.7 minutes and the panel called it
    /// "Stalled" at minute five, because nothing after the run's first step
    /// ever moved this instant. A settled action is evidence of work, not a
    /// heartbeat.
    #[tokio::test]
    async fn last_progress_at_moves_when_an_action_settles() {
        const STARTED_AT: &str = "2026-09-20T10:04:32Z";
        const ACTION_AT: &str = "2026-09-20T10:09:51Z";
        const FAILED_ACTION_AT: &str = "2026-09-20T10:10:02Z";

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let ctx = ExecutionContext {
            principal: scope.principal().to_string(),
            workspace: scope.workspace().to_string(),
            task_id: task_id.to_string(),
            execution_id: "exec-1".to_string(),
        };
        let mut task = task_record(task_id, STARTED_AT);
        let execution = execution_record(task_id, "exec-1", "running", STARTED_AT);
        let state_path = workspace.task_state_path(&scope.principal(), &scope.workspace(), task_id);
        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution should initialize");

        reducer
            .reduce_action_settled(&scope, &ctx, ACTION_AT)
            .await
            .expect("a settled action should persist");
        let state = read_json::<TaskState>(&state_path)
            .await
            .expect("task state should load");
        assert_eq!(
            state.last_progress_at.as_deref(),
            Some(ACTION_AT),
            "an action that produced a result is the run advancing"
        );

        // A failed action still settled: the run decided, acted, and observed.
        reducer
            .reduce_action_settled(&scope, &ctx, FAILED_ACTION_AT)
            .await
            .expect("a failed action should persist");
        let state = read_json::<TaskState>(&state_path)
            .await
            .expect("task state should load");
        assert_eq!(state.last_progress_at.as_deref(), Some(FAILED_ACTION_AT));
    }

    /// A step that FAILS still advanced the run — the plan moved on. Covered
    /// separately because `reduce_step_event` branches on the event type, and a
    /// guard written only for `StepCompleted` would leave a failing-and-retrying
    /// run looking permanently stalled.
    #[tokio::test]
    async fn last_progress_at_moves_on_a_failed_step() {
        const STARTED_AT: &str = "2026-07-29T09:00:00Z";
        const STEP_FAILED_AT: &str = "2026-07-29T09:02:00Z";

        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = test_reducer(workspace.clone());
        let task_id = "task-1";
        let scope = test_scope();
        let mut task = task_record(task_id, STARTED_AT);
        let execution = execution_record(task_id, "exec-1", "running", STARTED_AT);

        reducer
            .reduce_task_created(&task)
            .await
            .expect("task should persist");
        reducer
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("execution should initialize");
        reducer
            .reduce_step_event(
                &scope,
                &ExecutionContext {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    task_id: task_id.to_string(),
                    execution_id: "exec-1".to_string(),
                },
                crate::magician_v2::artifact_v2::events::ArtifactV2EventType::StepFailed,
                "step-1",
                STEP_FAILED_AT,
            )
            .await
            .expect("step failure should persist");

        let state = read_json::<TaskState>(&workspace.task_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        ))
        .await
        .expect("task state should load");
        assert_eq!(
            state.last_progress_at.as_deref(),
            Some(STEP_FAILED_AT),
            "a failed step is still the run advancing"
        );
    }

    #[tokio::test]
    async fn recipe_claim_renewal_waits_without_losing_a_timely_owner() {
        for scenario in ["timely", "late", "replaced", "cancelled", "budget_expired"] {
            let temp = tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
            let reducer = Arc::new(test_reducer(workspace.clone()));
            let scope = test_scope();
            let task_id = "task_app_recipe_atomic";
            let execution_id = "exec_recipe_atomic";
            let started = Utc::now();
            let started_at = started.to_rfc3339();
            let started_at_ms = started.timestamp_millis();
            let mut task = task_record(task_id, &started_at);
            reducer.reduce_task_created(&task).await.expect("task");
            let mut execution = execution_record(task_id, execution_id, "running", &started_at);
            execution.state.plan_id = Some("app_recipe_plan_exact".to_owned());
            execution.refs.plan_refs = vec![PlanRef {
                plan_id: "app_recipe_plan_exact".to_owned(),
                relative_path: "app_recipe_runs/exact/binding.json".to_owned(),
                created_at: started_at.clone(),
            }];
            let step_id = "app_recipe_step_exact";
            let schedule = ExecutionScheduleRecord {
                execution_id: execution_id.to_owned(),
                task_id: task_id.to_owned(),
                plan_id: execution.state.plan_id.clone(),
                source_plan_relative_path: Some("app_recipe_runs/exact/binding.json".to_owned()),
                source_kind: "app_recipe_v1".to_owned(),
                updated_at: started_at.clone(),
                steps: vec![ExecutionScheduleStep {
                    step_id: step_id.to_owned(),
                    title: "root".to_owned(),
                    order: 0,
                    depends_on_step_ids: Vec::new(),
                    capability: Some("app_entity_query".to_owned()),
                    delegate_agent_id: None,
                    taskplan_status: "pending".to_owned(),
                    taskplan_progress: "0/1".to_owned(),
                    sub_step_labels: Vec::new(),
                    recipe: Some(AppRecipeScheduleStepState {
                        node_execution_id: "app_recipe_node_exact".to_owned(),
                        parent_node_execution_id: execution_id.to_owned(),
                        binding_digest: "blake3:binding".to_owned(),
                        input_schema_ref: "schema:input".to_owned(),
                        output_schema_ref: "schema:output".to_owned(),
                        uncertainty: "impossible".to_owned(),
                        max_active_millis: 60_000,
                        cancellation_acknowledgement_timeout_millis: 1_000,
                        aggregate_deadline_at_ms: started_at_ms + 120_000,
                        phase: AppRecipeScheduleNodePhase::Pending,
                        attempt: 0,
                        claim_epoch: 0,
                        claim_owner_id: None,
                        claim_lease_expires_at_ms: None,
                        active_deadline_at_ms: None,
                        input_digest: None,
                        input_encoded_len: None,
                        retry_identity_digest: None,
                        output: None,
                        cancellation_requested_at: None,
                        cancellation_ack_deadline_at_ms: None,
                    }),
                }],
            };
            reducer
                .reduce_app_recipe_execution_initialized(&mut task, &execution, &schedule)
                .await
                .expect("initialize canonical root");
            let path = workspace.execution_schedule_path(
                scope.principal(),
                scope.workspace(),
                task_id,
                execution_id,
            );
            let guard = reducer
                .acquire_task_write_guard(&scope, task_id)
                .await
                .expect("effect lock");
            let mut schedule: ExecutionScheduleRecord =
                workspace.read_json_path(&path).await.expect("schedule");
            let before_wait = Utc::now().timestamp_millis();
            let lease_expiry = before_wait + 250;
            let recipe = schedule.steps[0].recipe.as_mut().expect("recipe");
            recipe.phase = AppRecipeScheduleNodePhase::Started;
            recipe.attempt = 1;
            recipe.claim_epoch = 1;
            recipe.claim_owner_id = Some("worker".to_owned());
            recipe.input_digest = Some("blake3:input".to_owned());
            recipe.input_encoded_len = Some(8);
            recipe.active_deadline_at_ms = Some(before_wait + 60_000);
            recipe.claim_lease_expires_at_ms = Some(if scenario == "late" {
                before_wait - 1
            } else {
                lease_expiry
            });
            workspace
                .write_json_value_atomic_stream_path(&path, schedule.clone(), 65536)
                .await
                .expect("seed short test lease");
            let renewal = reducer.reduce_app_recipe_step_claim_renewed(
                &scope,
                task_id,
                execution_id,
                step_id,
                "blake3:binding",
                "blake3:input",
                "worker",
                1,
                before_wait,
                &started_at,
            );
            tokio::pin!(renewal);
            assert!(
                futures_util::poll!(&mut renewal).is_pending(),
                "effect lock holds renewal"
            );
            assert!(
                Utc::now().timestamp_millis() < lease_expiry,
                "heartbeat requested before expiry"
            );
            tokio::time::sleep(std::time::Duration::from_millis(260)).await;
            let recipe = schedule.steps[0].recipe.as_mut().expect("recipe");
            match scenario {
                "replaced" => {
                    recipe.claim_owner_id = Some("successor".to_owned());
                    recipe.claim_epoch = 2;
                },
                "cancelled" => recipe.cancellation_requested_at = Some(Utc::now().to_rfc3339()),
                "budget_expired" => recipe.active_deadline_at_ms = Some(lease_expiry),
                _ => {},
            }
            workspace
                .write_json_value_atomic_stream_path(&path, schedule.clone(), 65536)
                .await
                .expect("concurrent fence winner");
            drop(guard);
            let decision = renewal.await.expect("renewal settles");
            match scenario {
                "timely" => assert!(matches!(
                    decision,
                    AppRecipeStepReducerAdmission::Reserved { claim_epoch: 1, .. }
                )),
                "replaced" => {
                    assert_eq!(decision, AppRecipeStepReducerAdmission::OwnedByLiveWorker)
                },
                "cancelled" => assert_eq!(decision, AppRecipeStepReducerAdmission::Cancelled),
                _ => assert_eq!(decision, AppRecipeStepReducerAdmission::OutcomeUncertain),
            }
            let saved: ExecutionScheduleRecord = workspace
                .read_json_path(&path)
                .await
                .expect("renewal state");
            let saved = saved.steps[0].recipe.as_ref().expect("recipe");
            if scenario == "timely" {
                assert!(
                    saved.claim_lease_expires_at_ms.expect("renewed lease")
                        > Utc::now().timestamp_millis()
                );
                assert_eq!(saved.active_deadline_at_ms, Some(before_wait + 60_000));
            }
            if scenario == "replaced" {
                assert_eq!(saved.claim_owner_id.as_deref(), Some("successor"));
                assert_eq!(saved.claim_epoch, 2);
            }
        }
    }

    #[tokio::test]
    async fn recipe_atomic_start_claim_deadline_and_schedule_cas_are_canonical() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = Arc::new(test_reducer(workspace.clone()));
        let scope = test_scope();
        let task_id = "task_app_recipe_atomic";
        let execution_id = "exec_recipe_atomic";
        let started = Utc::now();
        let started_at = started.to_rfc3339();
        let started_at_ms = started.timestamp_millis();
        let mut task = task_record(task_id, &started_at);
        reducer.reduce_task_created(&task).await.expect("task");
        let mut execution = execution_record(task_id, execution_id, "running", &started_at);
        execution.state.plan_id = Some("app_recipe_plan_exact".to_owned());
        execution.refs.plan_refs = vec![PlanRef {
            plan_id: "app_recipe_plan_exact".to_owned(),
            relative_path: "app_recipe_runs/exact/binding.json".to_owned(),
            created_at: started_at.clone(),
        }];
        let step_id = "app_recipe_step_exact";
        let mut schedule = ExecutionScheduleRecord {
            execution_id: execution_id.to_owned(),
            task_id: task_id.to_owned(),
            plan_id: execution.state.plan_id.clone(),
            source_plan_relative_path: Some("app_recipe_runs/exact/binding.json".to_owned()),
            source_kind: "app_recipe_v1".to_owned(),
            updated_at: started_at.clone(),
            steps: vec![ExecutionScheduleStep {
                step_id: step_id.to_owned(),
                title: "root".to_owned(),
                order: 0,
                depends_on_step_ids: Vec::new(),
                capability: Some("app_entity_query".to_owned()),
                delegate_agent_id: None,
                taskplan_status: "pending".to_owned(),
                taskplan_progress: "0/1".to_owned(),
                sub_step_labels: Vec::new(),
                recipe: Some(AppRecipeScheduleStepState {
                    node_execution_id: "app_recipe_node_exact".to_owned(),
                    parent_node_execution_id: execution_id.to_owned(),
                    binding_digest: "blake3:binding".to_owned(),
                    input_schema_ref: "schema:input".to_owned(),
                    output_schema_ref: "schema:output".to_owned(),
                    uncertainty: "impossible".to_owned(),
                    max_active_millis: 60_000,
                    cancellation_acknowledgement_timeout_millis: 1_000,
                    aggregate_deadline_at_ms: started_at_ms + 120_000,
                    phase: AppRecipeScheduleNodePhase::Pending,
                    attempt: 0,
                    claim_epoch: 0,
                    claim_owner_id: None,
                    claim_lease_expires_at_ms: None,
                    active_deadline_at_ms: None,
                    input_digest: None,
                    input_encoded_len: None,
                    retry_identity_digest: None,
                    output: None,
                    cancellation_requested_at: None,
                    cancellation_ack_deadline_at_ms: None,
                }),
            }],
        };
        let adoption_step_id = "app_recipe_step_adopt";
        let mut adoption_step = schedule.steps[0].clone();
        adoption_step.step_id = adoption_step_id.to_owned();
        adoption_step.title = "adopt crash output".to_owned();
        adoption_step.order = 1;
        let adoption_recipe = adoption_step.recipe.as_mut().expect("recipe state");
        adoption_recipe.node_execution_id = "app_recipe_node_adopt".to_owned();
        adoption_recipe.binding_digest = "blake3:binding-adopt".to_owned();
        schedule.steps.push(adoption_step);
        reducer
            .reduce_app_recipe_execution_initialized(&mut task, &execution, &schedule)
            .await
            .expect("root and recipe schedule commit atomically");

        let runtime_candidate = ExecutionScheduleRecord {
            source_kind: "runtime_context".to_owned(),
            steps: Vec::new(),
            ..schedule.clone()
        };
        let retained = reducer
            .reduce_execution_schedule_snapshot(&scope, task_id, &execution, &runtime_candidate)
            .await
            .expect("snapshot CAS");
        assert_eq!(retained.source_kind, "app_recipe_v1");

        let left = reducer.reduce_app_recipe_step_reserved(
            &scope,
            task_id,
            execution_id,
            step_id,
            "blake3:binding",
            "blake3:input",
            8,
            "blake3:retry",
            "worker-left",
            started_at_ms,
            &started_at,
        );
        let right = reducer.reduce_app_recipe_step_reserved(
            &scope,
            task_id,
            execution_id,
            step_id,
            "blake3:binding",
            "blake3:input",
            8,
            "blake3:retry",
            "worker-right",
            started_at_ms,
            &started_at,
        );
        let (left, right) = tokio::join!(left, right);
        let left = left.expect("left claim");
        let right = right.expect("right claim");
        let (owner, epoch, deadline) = match (left, right) {
            (
                AppRecipeStepReducerAdmission::Reserved {
                    claim_epoch,
                    deadline_at_ms,
                },
                AppRecipeStepReducerAdmission::OwnedByLiveWorker,
            ) => ("worker-left", claim_epoch, deadline_at_ms),
            (
                AppRecipeStepReducerAdmission::OwnedByLiveWorker,
                AppRecipeStepReducerAdmission::Reserved {
                    claim_epoch,
                    deadline_at_ms,
                },
            ) => ("worker-right", claim_epoch, deadline_at_ms),
            other => panic!("exactly one claim must win: {other:?}"),
        };
        reducer
            .reduce_app_recipe_step_started(
                &scope,
                task_id,
                execution_id,
                step_id,
                owner,
                epoch,
                started_at_ms + 1,
                &started_at,
            )
            .await
            .expect("winner starts");
        assert!(matches!(
            reducer
                .reduce_app_recipe_step_claim_renewed(
                    &scope,
                    task_id,
                    execution_id,
                    step_id,
                    "blake3:binding",
                    "blake3:input",
                    owner,
                    epoch,
                    started_at_ms + 2,
                    &started_at,
                )
                .await
                .expect("same epoch heartbeat renews"),
            AppRecipeStepReducerAdmission::Reserved {
                claim_epoch,
                deadline_at_ms,
            } if claim_epoch == epoch && deadline_at_ms == deadline
        ));
        let heartbeat_schedule: ExecutionScheduleRecord = workspace
            .read_json_path(workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            ))
            .await
            .expect("renewed schedule");
        let heartbeat_state = heartbeat_schedule.steps[0]
            .recipe
            .as_ref()
            .expect("heartbeat state");
        let heartbeat_lease_expires_at_ms =
            heartbeat_state.claim_lease_expires_at_ms.expect("lease");
        assert!(
            heartbeat_lease_expires_at_ms
                < heartbeat_state
                    .active_deadline_at_ms
                    .expect("active deadline"),
            "claim lease must remain distinct from the immutable active deadline"
        );
        let output = OutputRef {
            output_id: "app_recipe_value_exact".to_owned(),
            scope: "execution".to_owned(),
            audience: "internal".to_owned(),
            role: "app_recipe_node_value".to_owned(),
            relative_path: "app_recipe_runs/exact/nodes/root/output.json".to_owned(),
            media_type: "application/json".to_owned(),
            created_at: started_at.clone(),
            source_execution_id: Some(execution_id.to_owned()),
            source_plan_id: Some("app_recipe_binding_exact".to_owned()),
            source_output_ids: Vec::new(),
        };
        assert!(matches!(
            reducer
                .reduce_app_recipe_step_completed(
                    &scope,
                    task_id,
                    execution_id,
                    step_id,
                    "blake3:binding",
                    "blake3:input",
                    1,
                    owner,
                    epoch,
                    heartbeat_lease_expires_at_ms,
                    heartbeat_lease_expires_at_ms,
                    &output,
                    &started_at,
                )
                .await
                .expect("expired-lease completion settles"),
            AppRecipeStepReducerAdmission::OutcomeUncertain
        ));

        let adopted_claim = reducer
            .reduce_app_recipe_step_reserved(
                &scope,
                task_id,
                execution_id,
                adoption_step_id,
                "blake3:binding-adopt",
                "blake3:input-adopt",
                8,
                "blake3:retry-adopt",
                "worker-crashed",
                Utc::now().timestamp_millis(),
                &started_at,
            )
            .await
            .expect("adoption claim");
        let (adoption_epoch, adoption_deadline) = match adopted_claim {
            AppRecipeStepReducerAdmission::Reserved {
                claim_epoch,
                deadline_at_ms,
            } => (claim_epoch, deadline_at_ms),
            other => panic!("adoption claim must reserve: {other:?}"),
        };
        reducer
            .reduce_app_recipe_step_started(
                &scope,
                task_id,
                execution_id,
                adoption_step_id,
                "worker-crashed",
                adoption_epoch,
                Utc::now().timestamp_millis(),
                &started_at,
            )
            .await
            .expect("crashed owner starts");
        let claimed_schedule: ExecutionScheduleRecord = workspace
            .read_json_path(workspace.execution_schedule_path(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            ))
            .await
            .expect("claimed schedule");
        let crashed = claimed_schedule
            .steps
            .iter()
            .find(|step| step.step_id == adoption_step_id)
            .and_then(|step| step.recipe.as_ref())
            .expect("crashed claim state");
        let lease_expiry = crashed.claim_lease_expires_at_ms.expect("claim lease");
        assert!(lease_expiry < adoption_deadline);
        let adopted_output = OutputRef {
            output_id: "app_recipe_value_adopted".to_owned(),
            scope: "execution".to_owned(),
            audience: "internal".to_owned(),
            role: "app_recipe_node_value".to_owned(),
            relative_path: "app_recipe_runs/exact/nodes/adopt/output.json".to_owned(),
            media_type: "application/json".to_owned(),
            created_at: started_at.clone(),
            source_execution_id: Some(execution_id.to_owned()),
            source_plan_id: Some("app_recipe_binding_adopt".to_owned()),
            source_output_ids: Vec::new(),
        };
        assert!(matches!(
            reducer
                .reduce_app_recipe_step_output_adopted(
                    &scope,
                    task_id,
                    execution_id,
                    adoption_step_id,
                    "blake3:binding-adopt",
                    "blake3:input-adopt",
                    "worker-stale",
                    adoption_epoch,
                    lease_expiry - 1,
                    lease_expiry + 1,
                    &adopted_output,
                    &started_at,
                )
                .await,
            Err(ArtifactV2Error::InvalidRequest(reason))
                if reason == "app_recipe_output_adoption_substitution"
        ));
        assert!(matches!(
            reducer
                .reduce_app_recipe_step_output_adopted(
                    &scope,
                    task_id,
                    execution_id,
                    adoption_step_id,
                    "blake3:binding-adopt",
                    "blake3:input-adopt",
                    "worker-crashed",
                    adoption_epoch,
                    lease_expiry - 1,
                    lease_expiry + 1,
                    &adopted_output,
                    &started_at,
                )
                .await
                .expect("persisted pre-deadline output adopts after lease expiry"),
            AppRecipeStepReducerAdmission::Reserved {
                claim_epoch,
                deadline_at_ms,
            } if claim_epoch == adoption_epoch && deadline_at_ms == adoption_deadline
        ));
    }

    #[tokio::test]
    async fn recipe_cancellation_due_cannot_fire_early_or_repeat_terminal_transition() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let reducer = Arc::new(test_reducer(workspace));
        let scope = test_scope();
        let task_id = "task_app_recipe_cancel_due";
        let execution_id = "exec_recipe_cancel_due";
        let step_id = "app_recipe_step_cancel_due";
        let started = Utc::now();
        let started_at = started.to_rfc3339();
        let started_at_ms = started.timestamp_millis();
        let mut task = task_record(task_id, &started_at);
        reducer.reduce_task_created(&task).await.expect("task");
        let mut execution = execution_record(task_id, execution_id, "running", &started_at);
        execution.state.plan_id = Some("app_recipe_plan_cancel_due".to_owned());
        execution.refs.plan_refs = vec![PlanRef {
            plan_id: "app_recipe_plan_cancel_due".to_owned(),
            relative_path: "app_recipe_runs/cancel-due/binding.json".to_owned(),
            created_at: started_at.clone(),
        }];
        let schedule = ExecutionScheduleRecord {
            execution_id: execution_id.to_owned(),
            task_id: task_id.to_owned(),
            plan_id: execution.state.plan_id.clone(),
            source_plan_relative_path: Some("app_recipe_runs/cancel-due/binding.json".to_owned()),
            source_kind: "app_recipe_v1".to_owned(),
            updated_at: started_at.clone(),
            steps: vec![ExecutionScheduleStep {
                step_id: step_id.to_owned(),
                title: "root".to_owned(),
                order: 0,
                depends_on_step_ids: Vec::new(),
                capability: Some("app_entity_query".to_owned()),
                delegate_agent_id: None,
                taskplan_status: "pending".to_owned(),
                taskplan_progress: "0/1".to_owned(),
                sub_step_labels: Vec::new(),
                recipe: Some(AppRecipeScheduleStepState {
                    node_execution_id: "app_recipe_node_cancel_due".to_owned(),
                    parent_node_execution_id: execution_id.to_owned(),
                    binding_digest: "blake3:binding-cancel-due".to_owned(),
                    input_schema_ref: "schema:input".to_owned(),
                    output_schema_ref: "schema:output".to_owned(),
                    uncertainty: "impossible".to_owned(),
                    max_active_millis: 60_000,
                    cancellation_acknowledgement_timeout_millis: 5_000,
                    aggregate_deadline_at_ms: started_at_ms + 120_000,
                    phase: AppRecipeScheduleNodePhase::Pending,
                    attempt: 0,
                    claim_epoch: 0,
                    claim_owner_id: None,
                    claim_lease_expires_at_ms: None,
                    active_deadline_at_ms: None,
                    input_digest: None,
                    input_encoded_len: None,
                    retry_identity_digest: None,
                    output: None,
                    cancellation_requested_at: None,
                    cancellation_ack_deadline_at_ms: None,
                }),
            }],
        };
        reducer
            .reduce_app_recipe_execution_initialized(&mut task, &execution, &schedule)
            .await
            .expect("atomic recipe root");
        let claim = reducer
            .reduce_app_recipe_step_reserved(
                &scope,
                task_id,
                execution_id,
                step_id,
                "blake3:binding-cancel-due",
                "blake3:input-cancel-due",
                8,
                "blake3:retry-cancel-due",
                "worker-cancel-due",
                started_at_ms,
                &started_at,
            )
            .await
            .expect("claim");
        let (claim_epoch, active_deadline) = match claim {
            AppRecipeStepReducerAdmission::Reserved {
                claim_epoch,
                deadline_at_ms,
            } => (claim_epoch, deadline_at_ms),
            other => panic!("claim must reserve: {other:?}"),
        };
        assert!(active_deadline > started_at_ms);
        reducer
            .reduce_app_recipe_step_started(
                &scope,
                task_id,
                execution_id,
                step_id,
                "worker-cancel-due",
                claim_epoch,
                started_at_ms,
                &started_at,
            )
            .await
            .expect("start");
        let requested_at = Utc::now().to_rfc3339();
        let due_at_ms = match reducer
            .reduce_app_recipe_cancellation_requested(&scope, task_id, execution_id, &requested_at)
            .await
            .expect("cancel intent")
        {
            AppRecipeCancellationReducerAdmission::AwaitingStartedSettlement { deadline_at_ms } => {
                deadline_at_ms
            },
            other => panic!("started cancellation must retain a due item: {other:?}"),
        };
        assert!(matches!(
            reducer
                .reduce_app_recipe_cancellation_due(
                    &scope,
                    task_id,
                    execution_id,
                    due_at_ms,
                    &requested_at,
                )
                .await
                .expect("early due delivery"),
            AppRecipeCancellationDueAdmission::NotDue { deadline_at_ms }
                if deadline_at_ms == due_at_ms
        ));
        reducer
            .reduce_app_recipe_cancellation_settled(
                &scope,
                task_id,
                execution_id,
                &Utc::now().to_rfc3339(),
            )
            .await
            .expect("worker acknowledges before due");
        assert_eq!(
            reducer
                .reduce_app_recipe_cancellation_due(
                    &scope,
                    task_id,
                    execution_id,
                    due_at_ms,
                    &Utc::now().to_rfc3339(),
                )
                .await
                .expect("duplicate due delivery"),
            AppRecipeCancellationDueAdmission::ReconcileTerminal {
                outcome_uncertain: false,
            },
            "a duplicate timer can only reconcile the retained terminal fact",
        );
    }
}
