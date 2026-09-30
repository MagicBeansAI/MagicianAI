//! In-process LLM dispatch queue + worker pool.
//!
//! Provides a single chokepoint for all LLM calls in the process. Replaces
//! ad-hoc per-component semaphores with a global priority-queue + worker
//! pool that adds: cancellation gates, idempotency, retry with backoff,
//! provider-level resilience (cool-down, circuit breaker, per-provider
//! concurrency), watchdog, graceful shutdown, ledger emission, and a
//! realtime event stream for observability.
//!
//! See `docs/plans/2026-05-26-llm-dispatch-queue-and-local-prep.md` for the
//! full design. See `docs/plans/2026-05-26-llm-dispatch-implementation-checklist.md`
//! for the implementation checklist.

pub mod cancellation;
pub mod capacity;
pub mod classifier;
pub mod cloud_admission;
pub mod config;
pub mod events;
mod fair_lane;
pub mod inflight_index;
pub mod job;
pub mod ledger;
pub mod local_prep;
pub mod local_prep_coordinator;
pub mod metrics;
pub mod provider_state;
pub mod queue;
pub mod quota;
pub mod registry;
pub mod retry;
pub mod router_handle;
pub mod streaming;
pub mod test_support;
pub mod types;
pub mod worker;

pub use cancellation::{CancellationToken, NoopTaskStateView, TaskSnapshot, TaskStateView};
pub use capacity::{
    default_dispatch_engine, DispatchCapacityError, DispatchCapacityPlan, DispatchEngine,
};
pub use config::{
    BreakerConfig, DispatchConfig, LocalPrepConfig, ProviderConcurrencyConfig, RetryConfig,
};
pub use events::{EventBus, LlmQueueEvent};
pub use job::{
    AttemptError, AttemptHistory, DispatchedResponse, ErrorClass, JobMeta, LlmJob, LlmStreamJob,
};
pub use ledger::{LlmCallLedgerEvent, NoopTaskLedgerSink, TaskLedgerSink};
pub use queue::{LlmDispatchQueue, QueueSnapshot, ShutdownStats};
pub use quota::{ProviderQuota, ProviderQuotaMap};
pub use registry::JobRegistry;
pub use router_handle::DispatchRouter;
pub use types::{
    JobId, JobOrigin, JobState, LocalPrepCallStat, LocalPrepStat, Priority, TaskRef, TokenSummary,
    TombstoneReason,
};

#[cfg(test)]
mod tests;
