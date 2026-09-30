//! Bridge orchestrator task-cancellation transitions into the dispatch
//! queue. Without this, in-flight LLM calls only learn that their owning
//! task was cancelled on their NEXT call's pre-dispatch gate — meaning a
//! 10-minute in-flight provider call ignores cancellation for up to 10
//! minutes.
//!
//! Call `on_task_cancelled(task_id, reason)` from wherever the
//! orchestrator transitions a task to `cancelled` / `failed`. The bridge
//! does both halves:
//!
//! 1. `queue.cancel_task(task_id, reason)` — tombstones any pending jobs
//!    for the task before a worker picks them up.
//! 2. `task_state_view.fire_cancel(task_id)` — fires the per-task cancel
//!    token; the worker's `tokio::select!` race aborts the in-flight
//!    provider future and tombstones with `TaskCancelledInFlight`.
//!
//! Wire once at boot:
//!
//! ```ignore
//! let bridge = Arc::new(CancelBridge::new(queue.clone(), task_state_view.clone()));
//! // In whatever orchestrator path transitions task → cancelled:
//! bridge.on_task_cancelled(&task_id, "user_pressed_stop");
//! ```

use std::sync::Arc;

use magicllm::LlmDispatchQueue;

use super::task_state_view::ArtifactV2TaskStateView;

/// Owns clones of the queue + task-state-view so the orchestrator can fire
/// both halves of task cancellation through one call.
pub struct CancelBridge {
    queue: Arc<LlmDispatchQueue>,
    task_state_view: Arc<ArtifactV2TaskStateView>,
}

impl CancelBridge {
    pub fn new(
        queue: Arc<LlmDispatchQueue>,
        task_state_view: Arc<ArtifactV2TaskStateView>,
    ) -> Self {
        Self {
            queue,
            task_state_view,
        }
    }

    /// Cancel everything attached to a task: queued jobs + in-flight provider
    /// call. Returns the number of queued jobs that were tombstoned.
    pub fn on_task_cancelled(&self, task_id: &str, reason: impl Into<String>) -> usize {
        let reason = reason.into();
        // TEMP TRACE (delegated-child synthesis cancellation hunt): this fires
        // BOTH the queued-job tombstone and the in-flight abort, so it is the
        // single point that attributes a cancellation to its caller.
        tracing::info!(
            task_id = %task_id,
            reason = %reason,
            "[CANCEL-TRACE] on_task_cancelled (queued tombstone + in-flight abort)"
        );
        let n = self.queue.cancel_task(task_id, reason);
        self.task_state_view.fire_cancel(task_id);
        n
    }

    /// Cancel everything attached to a chat session.
    pub fn on_chat_session_ended(&self, chat_session_id: &str, reason: impl Into<String>) -> usize {
        self.queue.cancel_chat_session(chat_session_id, reason)
    }
}
