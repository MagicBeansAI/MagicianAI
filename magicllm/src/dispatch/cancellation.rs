//! Cancellation traits + helpers. Magicllm-side trait so this crate does not
//! depend on `magician_v2::artifact_v2`. Impls live in `magician`.

use async_trait::async_trait;

pub use tokio_util::sync::CancellationToken;

use super::types::TaskRef;

/// Read-only snapshot of a task's lifecycle state used by the pre-dispatch
/// cancellation gate.
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    /// Whether the task has reached a terminal state that warrants
    /// tombstoning queued LLM calls (Cancelled / Failed).
    pub is_cancelled_or_terminal_failed: bool,
    /// Optional reason string, propagated into the tombstone payload.
    pub cancel_reason: Option<String>,
}

/// Resolves task state on demand. Implementors typically wrap the host's
/// task store.
#[async_trait]
pub trait TaskStateView: Send + Sync {
    /// Fetch a snapshot for `task_id`. `Ok(None)` indicates the task no
    /// longer exists (caller will tombstone the pending job with
    /// `TombstoneReason::TaskMissing`). `Err(_)` is treated conservatively
    /// — the worker proceeds with the call.
    async fn snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, String>;

    /// Subscribe to a cancellation token that fires when the supplied task
    /// reference transitions to a terminal/cancelled state. Returning a
    /// fresh, never-fired token is acceptable when the host can't provide
    /// real cancel signals — the pre-dispatch gate is the safety net.
    fn subscribe_cancel(&self, task_ref: Option<&TaskRef>) -> CancellationToken;
}

/// Default-no-op implementation, for tests and the magicllm `submit_and_wait`
/// helper when callers don't have a task store.
pub struct NoopTaskStateView;

#[async_trait]
impl TaskStateView for NoopTaskStateView {
    async fn snapshot(&self, _task_id: &str) -> Result<Option<TaskSnapshot>, String> {
        Ok(Some(TaskSnapshot {
            is_cancelled_or_terminal_failed: false,
            cancel_reason: None,
        }))
    }

    fn subscribe_cancel(&self, _task_ref: Option<&TaskRef>) -> CancellationToken {
        CancellationToken::new() // never fires
    }
}
