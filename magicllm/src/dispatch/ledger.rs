//! Task ledger sink trait + the LlmCallLedgerEvent enum.
//!
//! Every job's lifecycle is mirrored as discrete events into the owning task's
//! runtime ledger so executors can resume / audit after a process restart.
//! Magicllm defines the trait; magician provides the impl that writes to
//! `artifact_v2` runtime ledgers.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capability::LLMProviderKind;

use super::job::ErrorClass;
use super::types::{JobId, JobOrigin, Priority, TaskRef, TokenSummary, TombstoneReason};

/// Lifecycle events for a single LLM call. Appended in order to the owning
/// task's ledger; consumers project the stream to reconstruct state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmCallLedgerEvent {
    /// Submission intent was durably recorded before queue admission. A
    /// following tombstone means shutdown, deadline, or capacity rejection
    /// won before the job became worker-visible.
    Submitted {
        job_id: JobId,
        origin: JobOrigin,
        priority: Priority,
        provider: Option<LLMProviderKind>,
        model: Option<String>,
        attempts_so_far: u32,
    },
    /// Worker picked up the job and is about to call the provider.
    AttemptStart {
        job_id: JobId,
        attempt: u32,
        dispatched_at_unix_ms: i64,
    },
    /// Attempt finished — success or single-attempt failure.
    AttemptDone {
        job_id: JobId,
        attempt: u32,
        success: bool,
        tokens: Option<TokenSummary>,
        duration_ms: u64,
        error_class: Option<ErrorClass>,
        error_msg: Option<String>,
    },
    /// Job exhausted in-place retries and was re-queued for another cycle.
    Requeued {
        job_id: JobId,
        cycle: u32,
        reason: String,
        error_class: ErrorClass,
    },
    /// Terminal success.
    Completed {
        job_id: JobId,
        total_attempts: u32,
        wait_ms: u64,
        execution_ms: u64,
        tokens: Option<TokenSummary>,
    },
    /// Terminal failure (all retries exhausted or non-retriable).
    Failed {
        job_id: JobId,
        total_attempts: u32,
        last_error: String,
        error_class: ErrorClass,
    },
    /// Terminal tombstone (cancellation, deadline, shutdown).
    Tombstoned {
        job_id: JobId,
        reason: TombstoneReason,
        attempts_so_far: u32,
    },
}

/// Sink that records ledger events. Implementors append to the task's
/// runtime ledger.
#[async_trait]
pub trait TaskLedgerSink: Send + Sync {
    /// Append one event for the supplied task. Failures are logged by the
    /// caller; the dispatch loop should never block on this.
    async fn append(&self, task_ref: &TaskRef, event: LlmCallLedgerEvent);
}

/// No-op sink for tests and contexts without a task store.
pub struct NoopTaskLedgerSink;

#[async_trait]
impl TaskLedgerSink for NoopTaskLedgerSink {
    async fn append(&self, _task_ref: &TaskRef, _event: LlmCallLedgerEvent) {}
}
