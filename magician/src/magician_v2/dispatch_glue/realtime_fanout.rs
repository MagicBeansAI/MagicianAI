//! Bridge `LlmQueueEvent` broadcast → realtime channel.
//!
//! Spawns a long-running task that subscribes to the queue's event bus and
//! forwards each event into the realtime transport. The actual transport
//! wiring is host-specific; this module exposes the spawn helper and lets
//! the caller plug in any forwarder closure.

use std::sync::Arc;

use magicllm::dispatch::LlmQueueEvent;
use magicllm::LlmDispatchQueue;
use tokio::task::JoinHandle;
use tracing::warn;

/// Subscribe to the queue's event bus and call `forward` for every event.
/// Returns the spawned `JoinHandle`; the caller may abort it on shutdown.
pub fn spawn_realtime_fanout<F>(queue: Arc<LlmDispatchQueue>, forward: F) -> JoinHandle<()>
where
    F: Fn(LlmQueueEvent) + Send + Sync + 'static,
{
    let mut rx = queue.subscribe_events();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => forward(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "realtime fanout lagged");
                },
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
