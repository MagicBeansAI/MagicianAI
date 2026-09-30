//! Budget-aware LLM call helper.
//!
//! Callers that participate in agent goal-pipeline budgets should use this
//! helper instead of awaiting `queue.submit_and_wait` directly. Wait-time
//! (queue dwell before worker pickup) is added to the supplied counter so
//! the goal-pipeline deadline check excludes time spent queued.
//!
//! ```ignore
//! // One budget per goal pipeline; clone it to whoever awaits LLM calls.
//! let budget = QueueWaitBudget::new();
//! let resp = await_llm_with_budget(&queue, job, &budget).await?;
//! // Queue dwell only — excludes provider execution time.
//! let queued_ms = budget.total_ms();
//! ```
//!
//! **Currently unused.** `await_llm_with_budget` and [`QueueWaitBudget`] are
//! exported from `dispatch_glue` but have no callers: nothing yet subtracts
//! queue dwell from a goal-pipeline deadline. The previous example here called
//! a `goal_pipeline_context()` that does not exist in the tree, so the
//! integration this module was written for either never landed or was renamed.
//! Verify against a real call site before trusting the shape above.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use magicllm::dispatch::{DispatchedResponse, LlmJob};
use magicllm::{LLMError, LLMResult, LlmDispatchQueue};

/// Counter shared with the goal-pipeline context.
#[derive(Clone, Default)]
pub struct QueueWaitBudget {
    inner: Arc<AtomicU64>,
}

impl QueueWaitBudget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total milliseconds spent waiting in the queue across all calls
    /// submitted under this budget. Subtract from the wall-clock deadline.
    pub fn total_ms(&self) -> u64 {
        self.inner.load(Ordering::Relaxed)
    }

    /// Reset the counter (e.g., when starting a new pipeline run).
    pub fn reset(&self) {
        self.inner.store(0, Ordering::Relaxed);
    }

    fn add(&self, ms: u64) {
        self.inner.fetch_add(ms, Ordering::Relaxed);
    }
}

/// Submit an `LlmJob` and await its result, adding the queue-wait portion to
/// the supplied budget counter.
pub async fn await_llm_with_budget(
    queue: &LlmDispatchQueue,
    job: LlmJob,
    budget: &QueueWaitBudget,
) -> LLMResult<DispatchedResponse> {
    // The job carries its own oneshot::Sender. We need the receiver to await
    // it. Re-create the channel here so we own the rx side. (Callers that
    // already hold the receiver from `LlmJob::new` should use `submit` +
    // their own rx + manually account for `wait`.)
    let (job, rx) = rebuild_job_with_fresh_response_channel(job);
    queue.submit(job).await?;
    let result = rx.await.map_err(|_| LLMError::Cancelled {
        reason: "receiver_dropped".to_string(),
    })??;
    budget.add(result.wait.as_millis() as u64);
    Ok(result)
}

fn rebuild_job_with_fresh_response_channel(
    mut job: LlmJob,
) -> (
    LlmJob,
    tokio::sync::oneshot::Receiver<LLMResult<DispatchedResponse>>,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    job.response_tx = tx;
    (job, rx)
}
