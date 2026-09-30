//! Synthesis-pending read gate for terminal-execution artifacts.
//!
//! When an execution reaches a terminal state, magician's write path
//! (`service::persist_execution_outcome`) atomically flips the execution
//! to terminal + marks `synthesis_pending = true`, then spawns the
//! output-synthesis pipeline (1.1 `v3_execution_output_synthesis`, 1.2
//! `v3_task_agent_output_synthesis`, 1.3 `v3_task_user_output_synthesis`)
//! in a `tokio::spawn`. Until that synthesis lands, the synthesized
//! artifacts (`execution_output`, `task_agent_output`, `task_user_output`)
//! either don't exist on disk yet or are partial.
//!
//! Any consumer of those artifacts — downstream tasks whose `depends_on`
//! resolves to this execution, chat-pack continuations that read the
//! prior task's output, UI download buttons, "create dashboard from
//! task X" agentic actions — MUST route reads through this gate. The
//! gate inspects `ExecutionState.synthesis_pending` /
//! `synthesis_failed`, returns a tri-state outcome
//! ([`OutputReadOutcome`]), and lets the caller decide whether to wait,
//! surface a "synthesizing…" affordance, or escalate to HITL.
//!
//! Silently reading past a `Pending` gate would either produce a
//! NotFound error (synthesis hasn't written the file) or — worse — read
//! a stale artifact from a prior execution under the same task. Both
//! paths corrupt downstream behaviour. The gate makes the in-flight
//! state explicit at the type level.
//!
//! The gate is intentionally *passive* — it only inspects on-disk
//! state. Waiting / polling is the caller's policy decision (different
//! consumers have different tolerance for blocking).

use crate::magician_v2::artifact_v2::models::{ExecutionState, SynthesisFailure, TaskState};

/// Three-state result of attempting to read an output artifact under the
/// `synthesis_pending` contract.
#[derive(Debug)]
pub enum OutputReadOutcome<T> {
    /// Synthesis completed successfully. The loaded artifact is in `T`.
    Ready(T),
    /// Execution / task is terminal but the output-synthesis pipeline
    /// hasn't landed yet. Caller should either wait + retry (chat-pack
    /// continuations, dependent-task schedulers) or render a
    /// "synthesizing…" affordance (UI list rows, task detail panel).
    Pending,
    /// Synthesis exhausted retries and was marked permanently failed.
    /// Caller MUST NOT silently consume a degraded artifact — surface
    /// the failure up to the user / HITL. `execution_id` always points
    /// at the failed execution; `detail` carries the full
    /// `SynthesisFailure` when the gate was built from an
    /// `ExecutionState` (which has the failure recorded directly),
    /// `None` when the gate was built from the task-level projection
    /// (which only carries the execution_id pointer and needs an
    /// extra disk read to retrieve the detail).
    Failed {
        execution_id: String,
        detail: Option<SynthesisFailure>,
    },
}

impl<T> OutputReadOutcome<T> {
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> OutputReadOutcome<U> {
        match self {
            OutputReadOutcome::Ready(value) => OutputReadOutcome::Ready(f(value)),
            OutputReadOutcome::Pending => OutputReadOutcome::Pending,
            OutputReadOutcome::Failed {
                execution_id,
                detail,
            } => OutputReadOutcome::Failed {
                execution_id,
                detail,
            },
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, OutputReadOutcome::Ready(_))
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, OutputReadOutcome::Pending)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, OutputReadOutcome::Failed { .. })
    }
}

/// Inspection summary derived from an `ExecutionState` or `TaskState`.
/// Lets gate inspectors stay decoupled from the full record.
///
/// The `Failed` variant carries the failed `execution_id` always (so
/// callers know which execution to retry) but the `SynthesisFailure`
/// `detail` is `None` when the gate was built from the task-level
/// projection — only the execution itself stores the rich detail
/// (`stage`, `last_error`, `attempts`, `failed_at`). Callers needing
/// the detail call `from_execution_state` directly OR look up the
/// execution by id and call it themselves.
#[derive(Debug, Clone)]
pub enum SynthesisReadiness {
    Ready,
    Pending,
    Failed {
        execution_id: String,
        detail: Option<SynthesisFailure>,
    },
}

impl SynthesisReadiness {
    /// Read the gate state from an `ExecutionState` snapshot. Always
    /// produces full `SynthesisFailure` detail when failed.
    pub fn from_execution_state(state: &ExecutionState) -> Self {
        if let Some(failure) = state.synthesis_failed.as_ref() {
            return SynthesisReadiness::Failed {
                execution_id: state.execution_id.clone(),
                detail: Some(failure.clone()),
            };
        }
        if state.synthesis_pending {
            return SynthesisReadiness::Pending;
        }
        SynthesisReadiness::Ready
    }

    /// Read the gate state from a `TaskState` snapshot. Useful for the
    /// per-task downstream dependency check (`task_dependencies_blocked`)
    /// where we don't need to load any specific execution.
    ///
    /// When `Failed`, `detail` is `None` — the task projection only
    /// carries the failed execution's id, not the full
    /// `SynthesisFailure`. Callers that need stage / error / attempts
    /// must load the execution by `execution_id` and call
    /// [`Self::from_execution_state`] on its `ExecutionState`.
    pub fn from_task_state(state: &TaskState) -> Self {
        if let Some(execution_id) = state.synthesis_failed_execution_id.as_ref() {
            return SynthesisReadiness::Failed {
                execution_id: execution_id.clone(),
                detail: None,
            };
        }
        if !state.synthesis_pending_executions.is_empty() {
            return SynthesisReadiness::Pending;
        }
        SynthesisReadiness::Ready
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, SynthesisReadiness::Ready)
    }
}

/// Apply a gate to a loader closure. The loader only runs if the
/// `ExecutionState` indicates synthesis has landed; otherwise the
/// closure is skipped and the corresponding tri-state outcome is
/// returned.
///
/// This is the canonical helper for "read an artifact tied to a
/// specific execution" — task-level reads should compose this against
/// each candidate execution, or use the lighter
/// [`SynthesisReadiness::from_task_state`] for the task-only projection.
pub async fn gated_execution_read<T, F, Fut, E>(
    state: &ExecutionState,
    loader: F,
) -> Result<OutputReadOutcome<T>, E>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    match SynthesisReadiness::from_execution_state(state) {
        SynthesisReadiness::Ready => Ok(OutputReadOutcome::Ready(loader().await?)),
        SynthesisReadiness::Pending => Ok(OutputReadOutcome::Pending),
        SynthesisReadiness::Failed {
            execution_id,
            detail,
        } => Ok(OutputReadOutcome::Failed {
            execution_id,
            detail,
        }),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::{SynthesisFailure, SynthesisStage};

    fn execution_state(
        synthesis_pending: bool,
        failed: Option<SynthesisFailure>,
    ) -> ExecutionState {
        ExecutionState {
            execution_id: "exec-1".into(),
            task_id: "task-1".into(),
            root_execution_id: None,
            parent_execution_id: None,
            agent_id: "agent".into(),
            relationship_type: "root".into(),
            status: "completed".into(),
            plan_id: None,
            primary_execution_output_id: None,
            active_child_execution_ids: Vec::new(),
            started_at: "2026-05-24T00:00:00Z".into(),
            completed_at: Some("2026-05-24T00:01:00Z".into()),
            updated_at: "2026-05-24T00:01:00Z".into(),
            completed_step_ids: Vec::new(),
            failed_step_ids: Vec::new(),
            current_step_id: None,
            task_output_mode: Default::default(),
            refinement: None,
            synthesis_pending,
            synthesis_failed: failed,
            completion_kind: None,
            open_items: Vec::new(),
        }
    }

    #[tokio::test]
    async fn ready_runs_loader() {
        let state = execution_state(false, None);
        let outcome = gated_execution_read::<_, _, _, ()>(&state, || async { Ok("loaded") })
            .await
            .unwrap();
        assert!(matches!(outcome, OutputReadOutcome::Ready("loaded")));
    }

    #[tokio::test]
    async fn pending_skips_loader() {
        let state = execution_state(true, None);
        let mut called = false;
        let outcome = gated_execution_read::<_, _, _, ()>(&state, || async {
            called = true;
            Ok("loaded")
        })
        .await
        .unwrap();
        assert!(matches!(outcome, OutputReadOutcome::Pending));
        assert!(!called, "loader must not run when synthesis_pending");
    }

    #[tokio::test]
    async fn failed_short_circuits() {
        let failure = SynthesisFailure {
            stage: SynthesisStage::TaskUserOutput,
            last_error: "boom".into(),
            attempts: 3,
            failed_at: "2026-05-24T00:02:00Z".into(),
        };
        let state = execution_state(false, Some(failure.clone()));
        let outcome = gated_execution_read::<_, _, _, ()>(&state, || async { Ok("loaded") })
            .await
            .unwrap();
        match outcome {
            OutputReadOutcome::Failed {
                execution_id,
                detail,
            } => {
                assert_eq!(execution_id, "exec-1");
                assert_eq!(detail.as_ref(), Some(&failure));
            },
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn from_task_state_failed_has_no_detail() {
        use crate::magician_v2::artifact_v2::models::TaskState;
        let state = TaskState {
            task_id: "t".into(),
            status: "completed".into(),
            completion_kind: None,
            open_items: Vec::new(),
            active_root_execution_id: None,
            latest_root_execution_id: Some("exec-1".into()),
            last_completed_root_execution_id: None,
            default_task_agent_output_id: None,
            primary_user_output_id: None,
            schedule_fire_count: 0,
            synthesis_pending_executions: Vec::new(),
            synthesis_failed_execution_id: Some("exec-1".into()),
            monitor_cursor: None,
            last_progress_at: None,
            updated_at: "2026-05-24T00:00:00Z".into(),
        };
        match SynthesisReadiness::from_task_state(&state) {
            SynthesisReadiness::Failed {
                execution_id,
                detail,
            } => {
                assert_eq!(execution_id, "exec-1");
                assert!(detail.is_none(), "task-state projection has no detail");
            },
            other => panic!("expected Failed; got {other:?}"),
        }
    }
}
