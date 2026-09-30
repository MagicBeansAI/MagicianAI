//! Process-wide cap on simultaneous non-Ollama provider HTTP.
//!
//! Independent of per-provider semaphores and the scheduler worker count.
//! `DispatchConfig.global_cloud_concurrency == 0` disables the extra cap.
//! Ollama does not take a permit: the local generate path is already cap-1.
//! Sync workers and streaming jobs share the same cap: acquire after the
//! provider permit and RPM/TPM, hold through the physical HTTP attempt or
//! stream, and `observe` the terminal provider on 429 / success.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::capability::LLMProviderKind;

use super::job::ErrorClass;

/// Consecutive successes before recovering one cloud slot after a 429 shrink.
const RECOVER_AFTER_SUCCESSES: usize = 4;

/// Global cloud HTTP admission. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct CloudAdmission {
    inner: Option<Arc<CloudAdmissionInner>>,
}

struct CloudAdmissionInner {
    semaphore: Arc<Semaphore>,
    policy: AdaptiveCloudPolicy,
    pending_shrink: AtomicUsize,
}

/// Numeric 429 shrink / slow-recover policy. Floor is `max(1, configured / 2)`.
pub struct AdaptiveCloudPolicy {
    configured: usize,
    effective: AtomicUsize,
    successes: AtomicUsize,
}

impl AdaptiveCloudPolicy {
    pub fn new(configured: usize) -> Self {
        let configured = configured.max(1);
        Self {
            configured,
            effective: AtomicUsize::new(configured),
            successes: AtomicUsize::new(0),
        }
    }

    pub fn configured(&self) -> usize {
        self.configured
    }

    pub fn effective(&self) -> usize {
        self.effective.load(Ordering::SeqCst)
    }

    pub fn floor(&self) -> usize {
        (self.configured / 2).max(1)
    }

    /// Shrink by 1 on 429. Returns the new effective cap when it changed.
    pub fn on_rate_limited(&self) -> Option<usize> {
        let floor = self.floor();
        loop {
            let current = self.effective.load(Ordering::SeqCst);
            if current <= floor {
                return None;
            }
            match self.effective.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.successes.store(0, Ordering::SeqCst);
                    return Some(current - 1);
                },
                Err(_) => continue,
            }
        }
    }

    /// Recover +1 after a streak of successes. Returns the new effective cap
    /// when it grew. Never exceeds `configured`.
    pub fn on_success(&self) -> Option<usize> {
        let n = self.successes.fetch_add(1, Ordering::SeqCst) + 1;
        if n < RECOVER_AFTER_SUCCESSES {
            return None;
        }
        loop {
            let current = self.effective.load(Ordering::SeqCst);
            if current >= self.configured {
                self.successes.store(0, Ordering::SeqCst);
                return None;
            }
            match self.effective.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.successes.store(0, Ordering::SeqCst);
                    return Some(current + 1);
                },
                Err(_) => continue,
            }
        }
    }
}

/// Held for the lifetime of one physical non-Ollama HTTP attempt.
pub struct CloudPermit {
    inner: Arc<CloudAdmissionInner>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for CloudPermit {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        loop {
            let pending = self.inner.pending_shrink.load(Ordering::SeqCst);
            if pending == 0 {
                drop(permit);
                return;
            }
            if self
                .inner
                .pending_shrink
                .compare_exchange(pending, pending - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                permit.forget();
                return;
            }
        }
    }
}

/// The cloud semaphore was closed (queue shutdown tearing down admission).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudAdmissionClosed;

impl CloudAdmission {
    /// `0` means no extra cap: acquire is a no-op for every provider.
    pub fn new(global_cloud_concurrency: usize) -> Arc<Self> {
        if global_cloud_concurrency == 0 {
            return Arc::new(Self { inner: None });
        }
        Arc::new(Self {
            inner: Some(Arc::new(CloudAdmissionInner {
                semaphore: Arc::new(Semaphore::new(global_cloud_concurrency)),
                policy: AdaptiveCloudPolicy::new(global_cloud_concurrency),
                pending_shrink: AtomicUsize::new(0),
            })),
        })
    }

    pub fn applies_to(kind: &LLMProviderKind) -> bool {
        !matches!(kind, LLMProviderKind::Ollama)
    }

    /// Wait for a cloud slot when this provider is non-Ollama and the cap is
    /// enabled. Returns `Ok(None)` for Ollama or a disabled cap.
    pub async fn acquire(
        &self,
        kind: &LLMProviderKind,
    ) -> Result<Option<CloudPermit>, CloudAdmissionClosed> {
        if !Self::applies_to(kind) {
            return Ok(None);
        }
        let Some(inner) = self.inner.as_ref() else {
            return Ok(None);
        };
        let permit = inner
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CloudAdmissionClosed)?;
        Ok(Some(CloudPermit {
            inner: Arc::clone(inner),
            permit: Some(permit),
        }))
    }

    /// 429 shrinks the effective cap by 1 (floor `max(1, configured/2)`);
    /// successes recover +1 slowly. No-op when the cap is disabled or the
    /// provider is Ollama.
    pub fn observe(&self, kind: &LLMProviderKind, class: Option<ErrorClass>) {
        if !Self::applies_to(kind) {
            return;
        }
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        match class {
            Some(ErrorClass::RateLimit) => {
                if inner.policy.on_rate_limited().is_some() {
                    match inner.semaphore.clone().try_acquire_owned() {
                        Ok(permit) => permit.forget(),
                        Err(_) => {
                            inner.pending_shrink.fetch_add(1, Ordering::SeqCst);
                        },
                    }
                }
            },
            None => {
                if inner.policy.on_success().is_some() {
                    loop {
                        let pending = inner.pending_shrink.load(Ordering::SeqCst);
                        if pending == 0 {
                            inner.semaphore.add_permits(1);
                            break;
                        }
                        if inner
                            .pending_shrink
                            .compare_exchange(
                                pending,
                                pending - 1,
                                Ordering::SeqCst,
                                Ordering::SeqCst,
                            )
                            .is_ok()
                        {
                            break;
                        }
                    }
                }
            },
            _ => {},
        }
    }

    pub fn effective(&self) -> Option<usize> {
        self.inner.as_ref().map(|inner| inner.policy.effective())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_does_not_apply() {
        assert!(!CloudAdmission::applies_to(&LLMProviderKind::Ollama));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::OpenAI));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::Anthropic));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::OpenRouter));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::Gemini));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::DeepSeek));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::Minimax));
        assert!(CloudAdmission::applies_to(&LLMProviderKind::Custom(
            "test".into()
        )));
    }

    #[test]
    fn zero_cap_is_disabled() {
        let admission = CloudAdmission::new(0);
        assert!(admission.inner.is_none());
        assert!(admission.effective().is_none());
    }

    #[test]
    fn floor_is_half_configured_at_least_one() {
        assert_eq!(AdaptiveCloudPolicy::new(16).floor(), 8);
        assert_eq!(AdaptiveCloudPolicy::new(2).floor(), 1);
        assert_eq!(AdaptiveCloudPolicy::new(1).floor(), 1);
        assert_eq!(AdaptiveCloudPolicy::new(3).floor(), 1);
    }

    #[test]
    fn rate_limit_shrinks_by_one_and_stops_at_floor() {
        let policy = AdaptiveCloudPolicy::new(4);
        assert_eq!(policy.floor(), 2);
        assert_eq!(policy.on_rate_limited(), Some(3));
        assert_eq!(policy.on_rate_limited(), Some(2));
        assert_eq!(policy.on_rate_limited(), None);
        assert_eq!(policy.effective(), 2);
    }

    #[test]
    fn successes_recover_one_slot_slowly() {
        let policy = AdaptiveCloudPolicy::new(4);
        assert_eq!(policy.on_rate_limited(), Some(3));
        for _ in 0..(RECOVER_AFTER_SUCCESSES - 1) {
            assert_eq!(policy.on_success(), None);
        }
        assert_eq!(policy.on_success(), Some(4));
        assert_eq!(policy.effective(), 4);
        for _ in 0..RECOVER_AFTER_SUCCESSES {
            assert_eq!(policy.on_success(), None);
        }
        assert_eq!(policy.effective(), 4);
    }

    #[test]
    fn rate_limit_resets_success_streak() {
        let policy = AdaptiveCloudPolicy::new(4);
        assert_eq!(policy.on_rate_limited(), Some(3));
        for _ in 0..(RECOVER_AFTER_SUCCESSES - 1) {
            assert_eq!(policy.on_success(), None);
        }
        assert_eq!(policy.on_rate_limited(), Some(2));
        for _ in 0..(RECOVER_AFTER_SUCCESSES - 1) {
            assert_eq!(policy.on_success(), None);
        }
        assert_eq!(policy.effective(), 2);
        assert_eq!(policy.on_success(), Some(3));
    }

    #[tokio::test]
    async fn acquire_is_noop_for_ollama_and_disabled_cap() {
        let disabled = CloudAdmission::new(0);
        assert!(disabled
            .acquire(&LLMProviderKind::OpenAI)
            .await
            .unwrap()
            .is_none());
        let capped = CloudAdmission::new(2);
        assert!(capped
            .acquire(&LLMProviderKind::Ollama)
            .await
            .unwrap()
            .is_none());
        let held = capped
            .acquire(&LLMProviderKind::OpenAI)
            .await
            .unwrap()
            .expect("openai takes a cloud permit");
        drop(held);
    }

    #[tokio::test]
    async fn observe_rate_limit_reduces_available_permits() {
        let admission = CloudAdmission::new(2);
        admission.observe(&LLMProviderKind::OpenAI, Some(ErrorClass::RateLimit));
        assert_eq!(admission.effective(), Some(1));
        let first = admission
            .acquire(&LLMProviderKind::Anthropic)
            .await
            .unwrap()
            .expect("one slot remains");
        let try_second = admission
            .inner
            .as_ref()
            .unwrap()
            .semaphore
            .clone()
            .try_acquire_owned();
        assert!(try_second.is_err(), "second cloud slot must be forgotten");
        drop(first);
    }
}
