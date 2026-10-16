//! Shared model admission and recovery state, retained across compatible reloads.
use crate::{runtime::UNHEALTHY_COOLDOWN, DecisionError};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

#[derive(Default)]
struct Health {
    epoch: u64,
    observed: bool,
    until: Option<Instant>,
    probing: bool,
    issue: Option<String>,
}
pub struct ModelAdmission {
    slots: Arc<crate::dispatch::ModelDispatch>,
    health: Mutex<Health>,
}
impl Default for ModelAdmission {
    fn default() -> Self {
        Self::new(6)
    }
}
impl ModelAdmission {
    pub fn new(limit: usize) -> Self {
        Self {
            slots: Arc::new(crate::dispatch::ModelDispatch::new(limit)),
            health: Mutex::default(),
        }
    }
    pub fn set_limit(&self, limit: usize) {
        self.configure(
            limit,
            crate::dispatch::DEFAULT_CAPACITY,
            crate::dispatch::DEFAULT_BYTES,
        );
    }
    pub fn configure(&self, limit: usize, capacity: usize, bytes: usize) {
        self.slots.configure(limit, capacity, bytes);
    }
    pub async fn enter(self: &Arc<Self>) -> Result<ModelPermit, DecisionError> {
        self.enter_request("default", 0).await
    }
    pub async fn enter_request(
        self: &Arc<Self>,
        owner: &str,
        bytes: usize,
    ) -> Result<ModelPermit, DecisionError> {
        // An unhealthy primary must fall through before queueing behind work
        // still occupying its physical slots. Recheck after admission as well:
        // provider health may change while a healthy request is queued.
        {
            let health = self.health.lock().unwrap_or_else(|p| p.into_inner());
            if health.probing || health.until.is_some_and(|until| Instant::now() < until) {
                return Err(DecisionError::Transport(
                    "decision model cooling down".into(),
                ));
            }
        }
        let background = SHADOW.try_with(|s| *s).unwrap_or(false);
        let slot = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            self.slots.acquire(owner, bytes, background),
        )
        .await
        .map_err(|_| DecisionError::DispatchTimeout)??;
        let mut health = self.health.lock().unwrap_or_else(|p| p.into_inner());
        let probe = health.until.is_some();
        if health.probing || health.until.is_some_and(|until| Instant::now() < until) {
            return Err(DecisionError::Transport(
                "decision model cooling down".into(),
            ));
        }
        if probe {
            health.probing = true;
        }
        Ok(ModelPermit {
            admission: self.clone(),
            _slot: slot,
            probe,
            epoch: health.epoch,
        })
    }
    pub fn health(&self) -> Option<Option<String>> {
        let health = self.health.lock().unwrap_or_else(|p| p.into_inner());
        health.observed.then(|| health.issue.clone())
    }
}
pub struct ModelPermit {
    admission: Arc<ModelAdmission>,
    _slot: crate::dispatch::DispatchPermit,
    probe: bool,
    epoch: u64,
}
impl ModelPermit {
    pub(crate) fn physical_lease(&self) -> Arc<crate::dispatch::Reservation> {
        self._slot.lease()
    }
    pub fn queue_wait_ms(&self) -> u64 {
        self._slot.queue_wait_ms
    }
    pub fn finish(&self, error: Option<&DecisionError>) {
        let mut health = self
            .admission
            .health
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match error {
            None if self.epoch == health.epoch => {
                health.observed = true;
                health.until = None;
                health.issue = None;
            },
            Some(error)
                if crate::runtime::is_availability_error(error)
                    && !matches!(
                        error,
                        DecisionError::DispatchFull | DecisionError::DispatchTimeout
                    ) =>
            {
                health.epoch = health.epoch.wrapping_add(1);
                health.observed = true;
                health.until = Some(Instant::now() + UNHEALTHY_COOLDOWN);
                health.issue = Some(error.health_reason().unwrap_or("invalid_response").into());
            },
            _ => {},
        }
    }
}
impl Drop for ModelPermit {
    fn drop(&mut self) {
        if self.probe {
            self.admission
                .health
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .probing = false;
        }
    }
}

tokio::task_local! { static SHADOW: bool; }
pub async fn with_priority<T>(shadow: bool, future: impl std::future::Future<Output = T>) -> T {
    SHADOW.scope(shadow, future).await
}

#[derive(Default)]
struct Counts {
    limit: usize,
    active: usize,
    shadow: usize,
}
/// Resize without losing in-flight work on a policy reload. Shadow work leaves
/// one model slot available for foreground decisions when capacity permits.
pub struct Limiter {
    counts: Mutex<Counts>,
    changed: Notify,
}
impl Limiter {
    pub fn new(limit: usize) -> Self {
        Self {
            counts: Mutex::new(Counts {
                limit,
                ..Default::default()
            }),
            changed: Notify::new(),
        }
    }
    pub fn set_limit(&self, limit: usize) {
        self.counts.lock().unwrap_or_else(|p| p.into_inner()).limit = limit;
        self.changed.notify_waiters();
    }
    pub async fn acquire(self: &Arc<Self>) -> Permit {
        let shadow = SHADOW.try_with(|s| *s).unwrap_or(false);
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
                if counts.active < counts.limit
                    && (!shadow || counts.shadow < counts.limit.saturating_sub(1).max(1))
                {
                    counts.active += 1;
                    counts.shadow += usize::from(shadow);
                    return Permit {
                        limiter: self.clone(),
                        shadow,
                    };
                }
            }
            notified.await;
        }
    }
}
pub struct Permit {
    limiter: Arc<Limiter>,
    shadow: bool,
}
impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = self
            .limiter
            .counts
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        counts.active -= 1;
        counts.shadow -= usize::from(self.shadow);
        drop(counts);
        self.limiter.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn unhealthy_model_does_not_wait_for_busy_physical_capacity() {
        let admission = Arc::new(ModelAdmission::new(1));
        let held = admission.enter().await.unwrap();
        held.finish(Some(&DecisionError::Timeout));
        // Keep its physical slot occupied, as with a cancelled GPU job.
        let result = tokio::time::timeout(Duration::from_millis(100), admission.enter())
            .await
            .expect("cooldown should bypass the occupied dispatch queue");
        assert!(matches!(result, Err(DecisionError::Transport(_))));
        drop(held);
    }

    #[tokio::test]
    async fn late_success_cannot_clear_a_newer_outage() {
        let admission = Arc::new(ModelAdmission::new(2));
        let slow_success = admission.enter().await.unwrap();
        let failed = admission.enter().await.unwrap();
        failed.finish(Some(&DecisionError::Timeout));
        drop(failed);
        slow_success.finish(None);
        drop(slow_success);
        assert!(admission.health().unwrap().is_some());
        assert!(admission.enter().await.is_err());
    }
    #[tokio::test]
    async fn successful_probe_cannot_clear_a_concurrent_newer_failure() {
        let admission = Arc::new(ModelAdmission::new(3));
        let older = admission.enter().await.unwrap();
        let failed = admission.enter().await.unwrap();
        failed.finish(Some(&DecisionError::Timeout));
        drop(failed);
        admission.health.lock().unwrap().until = Some(Instant::now() - Duration::from_secs(1));
        let probe = admission.enter().await.unwrap();
        older.finish(Some(&DecisionError::Timeout));
        drop(older);
        probe.finish(None);
        drop(probe);
        assert!(admission.health().unwrap().is_some());
        assert!(admission.enter().await.is_err());
    }
    #[tokio::test]
    async fn recovery_allows_only_one_probe_and_cancel_releases_it() {
        let admission = Arc::new(ModelAdmission::new(3));
        let permit = admission.enter().await.unwrap();
        permit.finish(Some(&DecisionError::Timeout));
        drop(permit);
        assert!(admission.enter().await.is_err());
        admission.health.lock().unwrap().until = Some(Instant::now() - Duration::from_secs(1));
        let probe = admission.enter().await.unwrap();
        assert!(admission.enter().await.is_err());
        drop(probe);
        let recovery = admission.enter().await.unwrap();
        recovery.finish(None);
        drop(recovery);
        assert_eq!(admission.health(), Some(None));
        assert!(admission.enter().await.is_ok());
    }
    #[tokio::test]
    async fn shadow_reserves_foreground_capacity_and_resizing_keeps_counts() {
        let limiter = Arc::new(Limiter::new(2));
        let shadow = with_priority(true, limiter.acquire()).await;
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            with_priority(true, limiter.acquire())
        )
        .await
        .is_err());
        let foreground = limiter.acquire().await;
        limiter.set_limit(1);
        drop(shadow);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), limiter.acquire())
                .await
                .is_err()
        );
        drop(foreground);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), limiter.acquire())
                .await
                .is_ok()
        );
    }
}

// Async cancellation cannot preempt blocking inference. Carry admission into
// the worker job so a timeout cannot advertise a still-busy model as idle.
tokio::task_local! { static PHYSICAL_LEASE: Arc<crate::dispatch::Reservation>; }
pub(crate) async fn with_physical_lease<T>(
    lease: Arc<crate::dispatch::Reservation>,
    future: impl std::future::Future<Output = T>,
) -> T {
    PHYSICAL_LEASE.scope(lease, future).await
}
#[cfg(any(feature = "mlx", feature = "onnx"))]
pub(crate) fn physical_lease() -> Option<Arc<crate::dispatch::Reservation>> {
    PHYSICAL_LEASE.try_with(Arc::clone).ok()
}
