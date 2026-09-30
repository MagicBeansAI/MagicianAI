//! Realtime broadcast events emitted by the dispatch queue. Powers the
//! viewer; can be fanned out into any host-level realtime channel.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::capability::LLMProviderKind;

use super::job::{ErrorClass, JobMeta};
use super::provider_state::BreakerState;
use super::types::{JobId, TombstoneReason};

/// Discriminated event published whenever a job changes state, plus
/// per-provider transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LlmQueueEvent {
    Submitted {
        meta: JobMeta,
    },
    WaitingForProvider {
        meta: JobMeta,
        provider: LLMProviderKind,
    },
    WaitingForLocalPrep {
        meta: JobMeta,
    },
    Dispatched {
        meta: JobMeta,
    },
    AttemptDone {
        meta: JobMeta,
        success: bool,
    },
    Requeued {
        meta: JobMeta,
        cycle: u32,
    },
    Completed {
        meta: JobMeta,
    },
    Failed {
        meta: JobMeta,
        error_class: ErrorClass,
    },
    Tombstoned {
        meta: JobMeta,
        reason: TombstoneReason,
    },
    ProviderStateChanged {
        provider: LLMProviderKind,
        from: BreakerState,
        to: BreakerState,
        reason: String,
    },
    LocalPrepSkipped {
        job_id: JobId,
        reason: String,
    },
}

/// Broadcast bus. The queue owns the sender; subscribers hold receivers.
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<LlmQueueEvent>,
}

impl EventBus {
    /// Construct with the supplied broadcast capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _rx) = broadcast::channel(capacity.max(16));
        Self { sender }
    }

    /// Subscribe to event stream. Lagging consumers receive `Lagged` errors
    /// from `broadcast::Receiver::recv`; that's their problem to handle.
    pub fn subscribe(&self) -> broadcast::Receiver<LlmQueueEvent> {
        self.sender.subscribe()
    }

    /// Emit one event; send failures (no subscribers) are ignored silently.
    pub fn emit(&self, event: LlmQueueEvent) {
        let _ = self.sender.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}
