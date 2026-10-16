//! Process startup barriers. Embedded/CLI runtimes that do not install a
//! barrier retain their existing immediate-start behavior.

use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

static STARTUP: OnceLock<Arc<StartupBarrier>> = OnceLock::new();

pub struct StartupBarrier {
    pub ready: CancellationToken,
    pub shutdown: CancellationToken,
    heavy: Arc<Semaphore>,
}

impl StartupBarrier {
    pub fn new() -> Self {
        Self {
            ready: CancellationToken::new(),
            shutdown: CancellationToken::new(),
            // These jobs can each materialize a whole store or index. Serialize
            // their first passes so readiness does not release an unbounded
            // burst of database and document allocations.
            heavy: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn install() -> Arc<Self> {
        Arc::clone(STARTUP.get_or_init(|| Arc::new(Self::new())))
    }

    async fn wait(&self) -> bool {
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => false,
            _ = self.ready.cancelled() => true,
        }
    }

    async fn admit(&self) -> Option<OwnedSemaphorePermit> {
        if !self.wait().await {
            return None;
        }
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => None,
            permit = Arc::clone(&self.heavy).acquire_owned() => permit.ok(),
        }
    }
}

impl Default for StartupBarrier {
    fn default() -> Self {
        Self::new()
    }
}

/// Wait for the full HTTP application to be usable, or abandon work when
/// startup/shutdown cancels it. This is a readiness signal, not a guessed sleep.
pub async fn wait_for_http() -> bool {
    let Some(state) = STARTUP.get() else {
        return true;
    };
    state.wait().await
}

pub async fn wait_for_http_or_cancel(cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        ready = wait_for_http() => ready,
    }
}

/// Admission for finite, heavy startup backfills. Never hold this permit for
/// the lifetime of a periodic worker; only its individual initial pass.
pub async fn admit_backfill() -> Option<Option<OwnedSemaphorePermit>> {
    let Some(state) = STARTUP.get() else {
        return Some(None);
    };
    state.admit().await.map(Some)
}

pub async fn admit_backfill_or_cancel(
    cancel: &CancellationToken,
) -> Option<Option<OwnedSemaphorePermit>> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        permit = admit_backfill() => permit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn readiness_is_required_and_shutdown_wins() {
        let state = StartupBarrier::new();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), state.wait())
                .await
                .is_err()
        );
        state.ready.cancel();
        assert!(state.wait().await);
        state.shutdown.cancel();
        assert!(!state.wait().await);
        assert!(state.admit().await.is_none());
    }

    #[tokio::test]
    async fn heavy_jobs_are_bounded_and_cancel_while_waiting() {
        let state = StartupBarrier::new();
        state.ready.cancel();
        let first = state.admit().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), state.admit())
                .await
                .is_err()
        );
        drop(first);
        let second = state.admit().await.unwrap();
        state.shutdown.cancel();
        assert!(state.admit().await.is_none());
        drop(second);
    }
}
