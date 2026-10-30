//! Runtime/Artifact divergence repair.
//!
//! One execution is tracked by two stores: the runtime conversation store's
//! [`WaitingState`] and the Artifact V2 execution record (status + outcome).
//! Owners that record an outcome in Artifact first and settle the runtime
//! second (the orphaned-pause finalizer, terminal reducers) can lose the
//! second write. What is left is an execution whose record is terminal while
//! the runtime still says `WaitingUser`/`Paused`/`Sleeping`/`Runnable` — and,
//! for a delegated child, a parent parked in `WaitingChildren` that nothing
//! wakes, because only the runtime's terminal transition runs the parent wake.
//!
//! This sweep finds those executions and settles the runtime from the
//! recorded outcome through
//! `MagicianV2Orchestrator::settle_runtime_after_recorded_outcome`, so HITL
//! and pause cleanup and the parent's `ChildCompleted` wake all run. It also
//! re-drives a `WaitingChildren` parent whose runtime child group has already
//! ended (a wake that was lost).
//!
//! It is conservative: never a run this process owns, never one with a live
//! or durable pause, never a record another owner wrote moments ago, never
//! a governed app workflow, and at most a bounded number of repairs a pass.
//! Each repair is logged once with its ids. It runs once at boot and then on
//! a fixed cadence (`spawn_runtime_divergence_reconciler`).

use super::*;

/// How long a terminal record written by *this* process must stand before the
/// sweep settles the runtime itself. Inside the window the owner that wrote
/// it is still expected to settle; a record from before this process started
/// has no owner left and is eligible at once.
pub(crate) const RECORDED_OUTCOME_SETTLE_GRACE_MS: i64 = 120_000;
/// Repairs (runtime settlements plus parent wakes) one pass may make.
pub(crate) const RUNTIME_DIVERGENCE_MAX_REPAIRS_PER_PASS: usize = 32;
/// Cadence of the periodic pass after the boot pass.
pub const RUNTIME_DIVERGENCE_SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Boot pass delay after HTTP is ready, so the boot recovery owners claim
/// their rows first and the two do not race on the same status CAS.
const RUNTIME_DIVERGENCE_BOOT_DELAY: Duration = Duration::from_secs(30);
/// Per-task budget; a task that exceeds it is retried on the next pass.
const RUNTIME_DIVERGENCE_TASK_BUDGET: Duration = Duration::from_secs(20);
/// Bound on the process-local "already reported" ledger.
const RUNTIME_DIVERGENCE_REPORTED_CAP: usize = 4_096;

fn reported_runtime_divergence() -> &'static Mutex<HashSet<String>> {
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REPORTED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn runtime_divergence_already_reported(key: &str) -> bool {
    reported_runtime_divergence()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(key)
}

/// Record `key`; `true` the first time this process sees it.
fn mark_runtime_divergence_reported(key: String) -> bool {
    let mut reported = reported_runtime_divergence()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reported.len() >= RUNTIME_DIVERGENCE_REPORTED_CAP && !reported.contains(&key) {
        reported.clear();
    }
    reported.insert(key)
}

/// The runtime status a terminal Artifact status settles to; `None` for a
/// record that has not ended.
pub(crate) fn runtime_status_for_recorded_outcome(artifact_status: &str) -> Option<WaitingState> {
    match artifact_status {
        "completed" => Some(WaitingState::Completed),
        "failed" => Some(WaitingState::Failed),
        "cancelled" | "canceled" => Some(WaitingState::Cancelled),
        _ => None,
    }
}

/// Runtime states the sweep may settle from a recorded outcome. `Executing`
/// is a live run, `WaitingChildren` belongs to its children's settlement, and
/// planning states belong to accepted-launch recovery — none is touched.
pub(crate) fn runtime_state_settles_from_record(state: &WaitingState) -> bool {
    matches!(
        state,
        WaitingState::WaitingUser
            | WaitingState::Paused
            | WaitingState::Sleeping
            | WaitingState::Runnable
    )
}

/// Whether a terminal record has stood long enough to be acted on: written
/// before this process started (its owner is gone), or older than the grace.
/// An unreadable timestamp is never acted on.
pub(crate) fn recorded_outcome_settled_long_enough(
    updated_at: &str,
    process_started_at_ms: i64,
    now_ms: i64,
) -> bool {
    let Ok(updated) = chrono::DateTime::parse_from_rfc3339(updated_at) else {
        return false;
    };
    let updated_ms = updated.timestamp_millis();
    updated_ms < process_started_at_ms
        || now_ms.saturating_sub(updated_ms) >= RECORDED_OUTCOME_SETTLE_GRACE_MS
}

/// Everything the settle decision reads about one execution.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RecordedOutcomeObservation<'a> {
    pub artifact_status: &'a str,
    pub runtime_state: &'a WaitingState,
    pub owned_in_this_process: bool,
    pub has_live_pause: bool,
    pub has_durable_pause: bool,
    pub record_settled_long_enough: bool,
}

/// The divergence detector: the runtime status to settle to, or `None` when
/// the execution is not a divergence the sweep may repair.
pub(crate) fn recorded_outcome_runtime_settlement(
    observation: RecordedOutcomeObservation<'_>,
) -> Option<WaitingState> {
    if observation.owned_in_this_process
        || observation.has_live_pause
        || observation.has_durable_pause
        || !observation.record_settled_long_enough
        || !runtime_state_settles_from_record(observation.runtime_state)
    {
        return None;
    }
    runtime_status_for_recorded_outcome(observation.artifact_status)
}

/// A parent parked on children that have all ended at the runtime, which no
/// wake reached. An empty group is never judged: a cleared group belongs to
/// the stateless handoff owner, not to this sweep.
pub(crate) fn parked_parent_needs_wake(
    parent_runtime_state: &WaitingState,
    parent_artifact_status: &str,
    parent_owned_in_this_process: bool,
    child_runtime_states: &[WaitingState],
) -> bool {
    *parent_runtime_state == WaitingState::WaitingChildren
        && !is_terminal_execution_status(parent_artifact_status)
        && !parent_owned_in_this_process
        && !child_runtime_states.is_empty()
        && child_runtime_states.iter().all(WaitingState::is_terminal)
}

/// What one divergence pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RuntimeDivergenceReport {
    pub tasks_examined: usize,
    pub runtimes_settled: usize,
    pub parents_woken: usize,
    pub deferred: usize,
    /// Where the next pass resumes when this one ran out of repair budget.
    pub resume_after: Option<String>,
}

impl RuntimeDivergenceReport {
    pub fn repairs(&self) -> usize {
        self.runtimes_settled.saturating_add(self.parents_woken)
    }

    fn absorb(&mut self, task: RuntimeDivergenceReport) {
        self.runtimes_settled = self.runtimes_settled.saturating_add(task.runtimes_settled);
        self.parents_woken = self.parents_woken.saturating_add(task.parents_woken);
        self.deferred = self.deferred.saturating_add(task.deferred);
    }
}

impl ArtifactV2Service {
    /// Run the divergence sweep once at boot (after HTTP is ready and a short
    /// delay) and then every [`RUNTIME_DIVERGENCE_SWEEP_INTERVAL`]. The handle
    /// may be dropped; the sweep lives as long as the process.
    pub fn spawn_runtime_divergence_reconciler(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        // Pin the process start now, during boot: the ownership and record
        // age gates measure "written by this process" against it, and the
        // value is fixed on first read.
        let _ = MagicianV2Orchestrator::process_started_at_ms();
        let service = Arc::clone(self);
        tokio::spawn(async move {
            if !crate::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            tokio::time::sleep(RUNTIME_DIVERGENCE_BOOT_DELAY).await;
            let mut ticker = tokio::time::interval(RUNTIME_DIVERGENCE_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut resume_after = None;
            loop {
                // The first tick completes at once: that is the boot pass.
                ticker.tick().await;
                let report = service
                    .reconcile_runtime_divergence_pass(
                        resume_after.take(),
                        RUNTIME_DIVERGENCE_MAX_REPAIRS_PER_PASS,
                    )
                    .await;
                if report.repairs() > 0 || report.deferred > 0 {
                    info!(
                        tasks_examined = report.tasks_examined,
                        runtimes_settled = report.runtimes_settled,
                        parents_woken = report.parents_woken,
                        deferred = report.deferred,
                        budget_exhausted = report.resume_after.is_some(),
                        "[RUNTIME-DIVERGENCE] reconciled runtime state against recorded Artifact outcomes"
                    );
                }
                resume_after = report.resume_after;
            }
        })
    }

    /// One bounded pass over every non-terminal task in every scope.
    pub async fn reconcile_runtime_divergence_pass(
        self: &Arc<Self>,
        resume_after: Option<String>,
        max_repairs: usize,
    ) -> RuntimeDivergenceReport {
        let mut report = RuntimeDivergenceReport::default();
        let candidates = match self.runtime_divergence_task_keys().await {
            Ok(candidates) => candidates,
            Err(error) => {
                warn!(%error, "[RUNTIME-DIVERGENCE] task listing failed; the next pass retries");
                report.deferred = 1;
                report.resume_after = resume_after;
                return report;
            },
        };
        let mut last_done: Option<String> = None;
        for (key, principal, workspace, task_id) in candidates {
            if resume_after
                .as_deref()
                .is_some_and(|after| key.as_str() <= after)
            {
                continue;
            }
            if report.repairs() >= max_repairs {
                report.resume_after = last_done.or(resume_after);
                return report;
            }
            report.tasks_examined = report.tasks_examined.saturating_add(1);
            let remaining = max_repairs.saturating_sub(report.repairs());
            let service = Arc::clone(self);
            let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
            let job_task_id = task_id.clone();
            let outcome = tokio::time::timeout(
                RUNTIME_DIVERGENCE_TASK_BUDGET,
                crate::magician_v2::execution::runtime_boundary::run_execution_job(
                    move || async move {
                        service
                            .repair_runtime_divergence_for_task(&scope, &job_task_id, remaining)
                            .await
                    },
                ),
            )
            .await;
            match outcome {
                Ok(Ok(Ok(task_report))) => report.absorb(task_report),
                Ok(Ok(Err(error))) => {
                    report.deferred = report.deferred.saturating_add(1);
                    debug!(task_id = %task_id, %error, "[RUNTIME-DIVERGENCE] task skipped this pass");
                },
                Ok(Err(error)) => {
                    report.deferred = report.deferred.saturating_add(1);
                    warn!(task_id = %task_id, %error, "[RUNTIME-DIVERGENCE] task repair job failed");
                },
                Err(_) => {
                    report.deferred = report.deferred.saturating_add(1);
                    warn!(
                        task_id = %task_id,
                        budget_ms = RUNTIME_DIVERGENCE_TASK_BUDGET.as_millis(),
                        "[RUNTIME-DIVERGENCE] task repair ran out of budget; the next pass retries"
                    );
                },
            }
            last_done = Some(key);
        }
        report
    }

    /// Every task directory in every scope, as `(key, principal, workspace,
    /// task_id)` ordered by key, so a budget-limited pass can resume.
    async fn runtime_divergence_task_keys(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, ArtifactV2Error> {
        let mut keys = Vec::new();
        for (principal, workspace) in self.workspace.list_scope_segments().await? {
            let roots = [
                self.workspace.tasks_root(&principal, &workspace),
                self.workspace.internal_tasks_root(&principal, &workspace),
            ];
            for root in roots {
                for entry in self.workspace.read_dir_path_or_empty(&root).await? {
                    if !entry.is_dir {
                        continue;
                    }
                    let key = format!("{principal}\u{1f}{workspace}\u{1f}{}", entry.file_name);
                    keys.push((key, principal.clone(), workspace.clone(), entry.file_name));
                }
            }
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// Repair one task's divergences, at most `max_repairs` of them: first
    /// settle every runtime whose Artifact record already ended (which wakes
    /// its parent), then wake any parent still parked on children that have
    /// all ended at the runtime.
    pub(crate) async fn repair_runtime_divergence_for_task(
        self: &Arc<Self>,
        scope: &ScopeRef,
        task_id: &str,
        max_repairs: usize,
    ) -> Result<RuntimeDivergenceReport, ArtifactV2Error> {
        let mut report = RuntimeDivergenceReport::default();
        // App workflows have their own lifecycle owner and refuse generic
        // status changes; leave them to it.
        if max_repairs == 0 || is_canonical_app_workflow_task_id(task_id) {
            return Ok(report);
        }
        if !self.task_record_exists(scope, task_id).await? {
            return Ok(report);
        }
        let task = self.get_task(scope, task_id).await?;
        if is_terminal_execution_status(&task.state.status) {
            return Ok(report);
        }
        let mut records = Vec::new();
        for entry in self.list_executions(scope, task_id).await? {
            match self
                .get_execution(scope, task_id, &entry.execution_id)
                .await
            {
                Ok(record) => records.push(record),
                Err(ArtifactV2Error::ExecutionNotFound(_)) => {},
                Err(error) => {
                    report.deferred = report.deferred.saturating_add(1);
                    debug!(
                        task_id,
                        execution_id = %entry.execution_id,
                        %error,
                        "[RUNTIME-DIVERGENCE] execution record unreadable; skipped this pass"
                    );
                },
            }
        }

        // 1. A terminal record over a runtime that never settled.
        let mut woken_parents = HashSet::new();
        for record in &records {
            if report.repairs() >= max_repairs {
                return Ok(report);
            }
            if runtime_status_for_recorded_outcome(&record.state.status).is_none() {
                continue;
            }
            let execution_id = record.state.execution_id.as_str();
            let Some(runtime) = self.read_runtime_for_divergence(execution_id).await? else {
                continue;
            };
            if !runtime_state_settles_from_record(&runtime.waiting_state) {
                continue;
            }
            if let Err(error) =
                Self::validate_runtime_artifact_binding(scope, task_id, record, &runtime)
            {
                if mark_runtime_divergence_reported(format!("binding:{execution_id}")) {
                    warn!(
                        task_id,
                        execution_id,
                        %error,
                        "[RUNTIME-DIVERGENCE] runtime and Artifact disagree on this execution's identity; not settling it"
                    );
                }
                report.deferred = report.deferred.saturating_add(1);
                continue;
            }
            match self
                .settle_runtime_from_recorded_outcome(scope, task_id, record, &runtime)
                .await
            {
                Ok(true) => {
                    report.runtimes_settled = report.runtimes_settled.saturating_add(1);
                    if let Some(parent) = runtime.parent_execution_id.clone() {
                        woken_parents.insert(parent);
                    }
                },
                Ok(false) => {},
                Err(error) => {
                    report.deferred = report.deferred.saturating_add(1);
                    if mark_runtime_divergence_reported(format!("settle-failed:{execution_id}")) {
                        warn!(
                            task_id,
                            execution_id,
                            %error,
                            "[RUNTIME-DIVERGENCE] runtime settlement from the recorded outcome failed; the next pass retries"
                        );
                    }
                },
            }
        }

        // 2. A parked parent whose runtime child group has already ended.
        for record in &records {
            if report.repairs() >= max_repairs {
                return Ok(report);
            }
            if is_terminal_execution_status(&record.state.status) {
                continue;
            }
            let parent_execution_id = record.state.execution_id.as_str();
            if woken_parents.contains(parent_execution_id) {
                continue;
            }
            let Some(parent) = self
                .read_runtime_for_divergence(parent_execution_id)
                .await?
            else {
                continue;
            };
            if parent.waiting_state != WaitingState::WaitingChildren
                || parent.principal != scope.principal()
                || parent.workspace != scope.workspace()
                || parent.task_id.as_deref() != Some(task_id)
            {
                continue;
            }
            let mut child_states = Vec::with_capacity(parent.active_delegation_group.len());
            let mut group_readable = true;
            for child_execution_id in &parent.active_delegation_group {
                match self.read_runtime_for_divergence(child_execution_id).await? {
                    Some(child) => child_states.push(child.waiting_state),
                    None => {
                        group_readable = false;
                        break;
                    },
                }
            }
            if !group_readable {
                continue;
            }
            let owned = self
                .orchestrator
                .execution_is_owned_in_this_process(parent_execution_id, parent.updated_at);
            if !parked_parent_needs_wake(
                &parent.waiting_state,
                &record.state.status,
                owned,
                &child_states,
            ) {
                continue;
            }
            // Once per process per child group: a wake that did not move the
            // parent will not move it the second time either.
            let wake_key = format!(
                "wake:{parent_execution_id}:{}",
                parent.active_delegation_group.join(",")
            );
            if runtime_divergence_already_reported(&wake_key) {
                continue;
            }
            match self.wake_parked_parent(parent_execution_id).await {
                Ok(()) => {
                    mark_runtime_divergence_reported(wake_key);
                    report.parents_woken = report.parents_woken.saturating_add(1);
                    warn!(
                        principal = %scope.principal(),
                        workspace = %scope.workspace(),
                        task_id,
                        parent_execution_id,
                        child_execution_ids = ?parent.active_delegation_group,
                        "[RUNTIME-DIVERGENCE] woke a parent parked on children that had all ended"
                    );
                },
                Err(error) => {
                    report.deferred = report.deferred.saturating_add(1);
                    warn!(
                        task_id,
                        parent_execution_id,
                        %error,
                        "[RUNTIME-DIVERGENCE] parked-parent wake failed; the next pass retries"
                    );
                },
            }
        }
        Ok(report)
    }

    /// The runtime row, or `None` when the runtime has no such execution.
    async fn read_runtime_for_divergence(
        &self,
        execution_id: &str,
    ) -> Result<Option<ExecutionRun>, ArtifactV2Error> {
        match self
            .orchestrator
            .conversation_store()
            .get_execution(execution_id)
            .await
        {
            Ok(run) => Ok(Some(run)),
            Err(crate::magician_v2::storage::V2StorageError::ExecutionNotFound(_)) => Ok(None),
            Err(error) => Err(ArtifactV2Error::Runtime(format!(
                "runtime_divergence_read_failed:{execution_id}:{error}"
            ))),
        }
    }

    /// Settle one runtime from its terminal record, under the execution's
    /// pause-lifecycle admission so a pause cannot land between the check and
    /// the write. `Ok(false)` when the execution is not a repairable
    /// divergence (owned here, paused, too fresh, or no longer diverged).
    async fn settle_runtime_from_recorded_outcome(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        record: &ExecutionRecord,
        runtime: &ExecutionRun,
    ) -> Result<bool, ArtifactV2Error> {
        let execution_id = runtime.id.as_str();
        let record_settled_long_enough = recorded_outcome_settled_long_enough(
            &record.state.updated_at,
            MagicianV2Orchestrator::process_started_at_ms(),
            Utc::now().timestamp_millis(),
        );
        // Cheap gates first; the pause admission takes the lifecycle fence.
        let unfenced = RecordedOutcomeObservation {
            artifact_status: &record.state.status,
            runtime_state: &runtime.waiting_state,
            owned_in_this_process: self
                .orchestrator
                .execution_is_owned_in_this_process(execution_id, runtime.updated_at),
            has_live_pause: false,
            has_durable_pause: runtime.paused_from_state.is_some(),
            record_settled_long_enough,
        };
        if recorded_outcome_runtime_settlement(unfenced).is_none() {
            return Ok(false);
        }
        let pause_lifecycle_admission = self
            .orchestrator
            .acquire_resumable_pause_lifecycle_admission_scoped(
                execution_id,
                scope.principal(),
                scope.workspace(),
            )
            .await
            .map_err(ArtifactV2Error::Runtime)?;
        let Some(current) = self.read_runtime_for_divergence(execution_id).await? else {
            return Ok(false);
        };
        let fenced = RecordedOutcomeObservation {
            artifact_status: &record.state.status,
            runtime_state: &current.waiting_state,
            owned_in_this_process: self
                .orchestrator
                .execution_is_owned_in_this_process(execution_id, current.updated_at),
            has_live_pause: pause_lifecycle_admission.resumable()
                || pause_lifecycle_admission.staged(),
            has_durable_pause: current.paused_from_state.is_some(),
            record_settled_long_enough,
        };
        let Some(target) = recorded_outcome_runtime_settlement(fenced) else {
            return Ok(false);
        };
        warn!(
            principal = %scope.principal(),
            workspace = %scope.workspace(),
            task_id,
            execution_id,
            parent_execution_id = current.parent_execution_id.as_deref().unwrap_or(""),
            recorded_status = %record.state.status,
            runtime_state = ?current.waiting_state,
            settled_to = ?target,
            "[RUNTIME-DIVERGENCE] settling a runtime execution whose Artifact record already ended; the settlement wakes its parent"
        );
        // Through the side-effecting transition, holding the admission like
        // the orphaned-pause finalizer does: HITL and pause cleanup and the
        // parent's ChildCompleted wake all run, and only while the runtime is
        // still in the state judged above.
        Box::pin(self.orchestrator.settle_runtime_after_recorded_outcome(
            execution_id,
            &current.waiting_state,
            target,
            "runtime settled from its recorded Artifact outcome (divergence repair)",
        ))
        .await
        .map_err(ArtifactV2Error::Runtime)?;
        drop(pause_lifecycle_admission);
        Ok(true)
    }

    /// Re-drive a parked parent's reconciliation through the durable wake
    /// queue, or the orchestrator's retry owner when there is no queue.
    async fn wake_parked_parent(&self, parent_execution_id: &str) -> Result<(), ArtifactV2Error> {
        if let Some(queue) = self.orchestrator.wake_up_queue() {
            return queue
                .schedule_child_completed_wake(parent_execution_id, Utc::now())
                .await;
        }
        self.orchestrator
            .retain_parent_reconciliation_retry(parent_execution_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation<'a>(
        artifact_status: &'a str,
        runtime_state: &'a WaitingState,
    ) -> RecordedOutcomeObservation<'a> {
        RecordedOutcomeObservation {
            artifact_status,
            runtime_state,
            owned_in_this_process: false,
            has_live_pause: false,
            has_durable_pause: false,
            record_settled_long_enough: true,
        }
    }

    #[test]
    fn terminal_record_over_a_waiting_runtime_settles_to_the_recorded_outcome() {
        assert_eq!(
            recorded_outcome_runtime_settlement(observation("failed", &WaitingState::WaitingUser)),
            Some(WaitingState::Failed)
        );
        assert_eq!(
            recorded_outcome_runtime_settlement(observation("completed", &WaitingState::Paused)),
            Some(WaitingState::Completed)
        );
        assert_eq!(
            recorded_outcome_runtime_settlement(observation("cancelled", &WaitingState::Sleeping)),
            Some(WaitingState::Cancelled)
        );
        assert_eq!(
            recorded_outcome_runtime_settlement(observation("canceled", &WaitingState::Runnable)),
            Some(WaitingState::Cancelled)
        );
    }

    #[test]
    fn a_record_that_has_not_ended_is_not_a_divergence() {
        for status in [
            "running",
            "waiting_for_user",
            "waiting_for_children",
            "paused",
            "",
        ] {
            assert_eq!(
                recorded_outcome_runtime_settlement(observation(
                    status,
                    &WaitingState::WaitingUser
                )),
                None,
                "{status}"
            );
        }
    }

    #[test]
    fn live_or_owned_runtime_states_are_never_settled() {
        for state in [
            WaitingState::Executing,
            WaitingState::WaitingChildren,
            WaitingState::Planning,
            WaitingState::PlanningComplete,
            WaitingState::Completed,
            WaitingState::Failed,
            WaitingState::Cancelled,
        ] {
            assert_eq!(
                recorded_outcome_runtime_settlement(observation("failed", &state)),
                None,
                "{state:?}"
            );
        }
    }

    #[test]
    fn a_live_pause_or_in_process_owner_or_fresh_record_blocks_settlement() {
        let waiting = WaitingState::WaitingUser;
        let mut live_pause = observation("failed", &waiting);
        live_pause.has_live_pause = true;
        assert_eq!(recorded_outcome_runtime_settlement(live_pause), None);

        let mut durable_pause = observation("failed", &WaitingState::Paused);
        durable_pause.has_durable_pause = true;
        assert_eq!(recorded_outcome_runtime_settlement(durable_pause), None);

        let mut owned = observation("failed", &waiting);
        owned.owned_in_this_process = true;
        assert_eq!(recorded_outcome_runtime_settlement(owned), None);

        let mut fresh = observation("failed", &waiting);
        fresh.record_settled_long_enough = false;
        assert_eq!(recorded_outcome_runtime_settlement(fresh), None);
    }

    #[test]
    fn record_age_gate_spares_only_fresh_records_written_by_this_process() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-10-19T18:33:00Z")
            .unwrap()
            .timestamp_millis();
        let now = started + 10 * 60_000;
        // Written by the previous process: its owner is gone.
        assert!(recorded_outcome_settled_long_enough(
            "2026-10-19T17:42:25Z",
            started,
            now
        ));
        // Written by this process long enough ago.
        assert!(recorded_outcome_settled_long_enough(
            "2026-10-19T18:34:00Z",
            started,
            now
        ));
        // Written by this process moments ago: its owner is still settling.
        assert!(!recorded_outcome_settled_long_enough(
            "2026-10-19T18:42:30Z",
            started,
            now
        ));
        assert!(!recorded_outcome_settled_long_enough(
            "not-a-time",
            started,
            now
        ));
    }

    #[test]
    fn parked_parent_is_woken_only_when_every_child_has_ended() {
        let parked = WaitingState::WaitingChildren;
        assert!(parked_parent_needs_wake(
            &parked,
            "running",
            false,
            &[WaitingState::Failed, WaitingState::Completed]
        ));
        assert!(!parked_parent_needs_wake(
            &parked,
            "running",
            false,
            &[WaitingState::Failed, WaitingState::WaitingUser]
        ));
        assert!(!parked_parent_needs_wake(&parked, "running", false, &[]));
        assert!(!parked_parent_needs_wake(
            &parked,
            "running",
            true,
            &[WaitingState::Failed]
        ));
        assert!(!parked_parent_needs_wake(
            &parked,
            "cancelled",
            false,
            &[WaitingState::Failed]
        ));
        assert!(!parked_parent_needs_wake(
            &WaitingState::Executing,
            "running",
            false,
            &[WaitingState::Failed]
        ));
    }

    #[test]
    fn reported_ledger_reports_each_key_once() {
        let key = format!("test:{}", uuid::Uuid::new_v4());
        assert!(!runtime_divergence_already_reported(&key));
        assert!(mark_runtime_divergence_reported(key.clone()));
        assert!(runtime_divergence_already_reported(&key));
        assert!(!mark_runtime_divergence_reported(key));
    }
}
