//! Magician-side adapters that bridge `magicllm::dispatch` to the host's
//! task store + runtime ledger.
//!
//! - `task_state_view`: implements `TaskStateView` over `ArtifactV2Service`.
//! - `task_ledger_sink`: writes `LlmCallLedgerEvent`s into the task's
//!   runtime ledger.
//! - `orphan_sweep`: on boot, emits synthetic `Tombstoned` events for any
//!   ledger entries left unresolved by the previous process.
//! - `realtime_fanout`: bridges `LlmQueueEvent` broadcast into the existing
//!   runtime realtime channel.
//! - `boot`: constructs `LlmDispatchQueue` from `magician-config.yaml` and
//!   wires shutdown.

pub mod boot;
pub mod budget_aware_await;
pub mod cancel_bridge;
pub mod orphan_sweep;
pub mod prewarm;
pub mod realtime_fanout;
pub mod task_ledger_sink;
pub mod task_state_view;

pub use boot::start_dispatch_queue;
pub use budget_aware_await::{await_llm_with_budget, QueueWaitBudget};
pub use cancel_bridge::CancelBridge;
pub use orphan_sweep::{run_orphan_sweep, run_startup_orphan_sweep};
pub use prewarm::spawn_local_prep_prewarm;
pub use realtime_fanout::spawn_realtime_fanout;
pub use task_ledger_sink::ArtifactV2TaskLedgerSink;
pub use task_state_view::ArtifactV2TaskStateView;
