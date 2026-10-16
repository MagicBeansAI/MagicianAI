//! Per-provider RPM/TPM token buckets for dispatch admission.
//!
//! Missing map entries and `0` rates are unlimited. Burst equals the per-minute
//! rate. Token estimates fall back to `max(1, message text chars / 4)`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::capability::LLMProviderKind;
use crate::types::{ContentBlock, LLMRequest};

/// Configured RPM/TPM for one provider key (`openai`, `anthropic`, ...).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderQuota {
    #[serde(default)]
    pub rpm: Option<u32>,
    #[serde(default)]
    pub tpm: Option<u32>,
}

/// Token bucket with `burst = rate` tokens per minute.
pub struct TokenBucket {
    rate_per_minute: u32,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// `rate_per_minute == 0` is rejected by [`ProviderQuotaMap`]; callers pass
    /// a positive rate. Burst equals the rate.
    pub fn new(rate_per_minute: u32) -> Self {
        let rate_per_minute = rate_per_minute.max(1);
        Self {
            rate_per_minute,
            tokens: rate_per_minute as f64,
            last_refill: Instant::now(),
        }
    }

    fn burst(&self) -> f64 {
        f64::from(self.rate_per_minute)
    }

    fn need(&self, n: u32) -> f64 {
        f64::from(n.max(1)).min(self.burst())
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_refill);
        let add = self.burst() * elapsed.as_secs_f64() / 60.0;
        self.tokens = (self.tokens + add).min(self.burst());
        self.last_refill = now;
    }

    /// Duration until `n` tokens (capped at burst) are available.
    fn wait_for(&mut self, n: u32) -> Duration {
        self.refill();
        let need = self.need(n);
        if self.tokens >= need {
            Duration::ZERO
        } else {
            let missing = need - self.tokens;
            Duration::from_secs_f64(missing * 60.0 / self.burst())
        }
    }

    fn consume(&mut self, n: u32) {
        let need = self.need(n);
        self.tokens = (self.tokens - need).max(0.0);
    }

    /// Consume `n` tokens or return how long to wait. Burst equals rate.
    pub fn try_acquire(&mut self, n: u32) -> Result<(), Duration> {
        let wait = self.wait_for(n);
        if wait.is_zero() {
            self.consume(n);
            Ok(())
        } else {
            Err(wait)
        }
    }
}

struct ProviderBuckets {
    rpm: Option<Mutex<TokenBucket>>,
    tpm: Option<Mutex<TokenBucket>>,
}

/// Process-wide RPM/TPM map keyed by [`LLMProviderKind::as_str`].
pub struct ProviderQuotaMap {
    providers: HashMap<String, ProviderBuckets>,
}

fn limited_rate(rate: Option<u32>) -> Option<u32> {
    rate.filter(|n| *n > 0)
}

/// `max(1, total message text chars / 4)` when the request has no better count.
pub fn estimate_request_tokens(request: &LLMRequest) -> u32 {
    let chars: usize = request
        .messages
        .iter()
        .map(|message| {
            message
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => text.len(),
                    _ => 0,
                })
                .sum::<usize>()
        })
        .sum();
    u32::try_from((chars / 4).max(1)).unwrap_or(u32::MAX)
}

impl ProviderQuotaMap {
    /// Build from `DispatchConfig.provider_quota`. Empty / 0 = unlimited.
    pub fn from_config(config: &HashMap<String, ProviderQuota>) -> Arc<Self> {
        let mut providers = HashMap::new();
        for (key, quota) in config {
            let rpm = limited_rate(quota.rpm).map(|rate| Mutex::new(TokenBucket::new(rate)));
            let tpm = limited_rate(quota.tpm).map(|rate| Mutex::new(TokenBucket::new(rate)));
            if rpm.is_some() || tpm.is_some() {
                providers.insert(key.clone(), ProviderBuckets { rpm, tpm });
            }
        }
        Arc::new(Self { providers })
    }

    /// Consume one RPM token and `estimated_tokens` TPM tokens, or the wait.
    pub fn try_acquire(
        &self,
        kind: &LLMProviderKind,
        estimated_tokens: u32,
    ) -> Result<(), Duration> {
        let Some(buckets) = self.providers.get(kind.as_str()) else {
            return Ok(());
        };
        let mut rpm = buckets.rpm.as_ref().map(|mutex| mutex.lock());
        let mut tpm = buckets.tpm.as_ref().map(|mutex| mutex.lock());
        let rpm_wait = rpm
            .as_mut()
            .map(|bucket| bucket.wait_for(1))
            .unwrap_or(Duration::ZERO);
        let tpm_wait = tpm
            .as_mut()
            .map(|bucket| bucket.wait_for(estimated_tokens.max(1)))
            .unwrap_or(Duration::ZERO);
        let wait = rpm_wait.max(tpm_wait);
        if wait.is_zero() {
            if let Some(bucket) = rpm.as_mut() {
                bucket.consume(1);
            }
            if let Some(bucket) = tpm.as_mut() {
                bucket.consume(estimated_tokens.max(1));
            }
            Ok(())
        } else {
            Err(wait)
        }
    }

    /// Wait until RPM and TPM both allow this attempt. No-op when unlimited.
    pub async fn acquire(&self, kind: &LLMProviderKind, estimated_tokens: u32) {
        loop {
            match self.try_acquire(kind, estimated_tokens) {
                Ok(()) => return,
                Err(wait) => {
                    tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpm_allows_burst_then_waits() {
        let mut bucket = TokenBucket::new(2);
        assert!(bucket.try_acquire(1).is_ok());
        assert!(bucket.try_acquire(1).is_ok());
        let wait = bucket
            .try_acquire(1)
            .expect_err("burst equals rate; third request waits");
        assert!(wait > Duration::ZERO);

        let mut config = HashMap::new();
        config.insert(
            "anthropic".to_string(),
            ProviderQuota {
                rpm: Some(2),
                tpm: None,
            },
        );
        let map = ProviderQuotaMap::from_config(&config);
        assert!(map.try_acquire(&LLMProviderKind::Anthropic, 1).is_ok());
        assert!(map.try_acquire(&LLMProviderKind::Anthropic, 1).is_ok());
        let wait = map
            .try_acquire(&LLMProviderKind::Anthropic, 1)
            .expect_err("rpm burst then wait");
        assert!(wait > Duration::ZERO);
    }

    #[test]
    fn unlimited_when_unset() {
        let empty = ProviderQuotaMap::from_config(&HashMap::new());
        assert!(empty.try_acquire(&LLMProviderKind::OpenAI, 10_000).is_ok());

        let mut zero = HashMap::new();
        zero.insert(
            "openai".to_string(),
            ProviderQuota {
                rpm: Some(0),
                tpm: Some(0),
            },
        );
        let unlimited = ProviderQuotaMap::from_config(&zero);
        assert!(unlimited
            .try_acquire(&LLMProviderKind::OpenAI, 10_000)
            .is_ok());
    }

    #[test]
    fn tpm_deducts_estimated_tokens() {
        let mut config = HashMap::new();
        config.insert(
            "openai".to_string(),
            ProviderQuota {
                rpm: None,
                tpm: Some(10),
            },
        );
        let map = ProviderQuotaMap::from_config(&config);
        assert!(map.try_acquire(&LLMProviderKind::OpenAI, 7).is_ok());
        let wait = map
            .try_acquire(&LLMProviderKind::OpenAI, 4)
            .expect_err("only 3 TPM tokens remain");
        assert!(wait > Duration::ZERO);
        assert!(map.try_acquire(&LLMProviderKind::OpenAI, 3).is_ok());
    }

    #[test]
    fn estimate_is_at_least_one_and_uses_chars_over_four() {
        let empty = LLMRequest::default();
        assert_eq!(estimate_request_tokens(&empty), 1);

        let request = LLMRequest {
            messages: Arc::new(vec![crate::types::LLMMessage::user("abcdabcd")]),
            ..LLMRequest::default()
        };
        assert_eq!(estimate_request_tokens(&request), 2);
    }

    #[test]
    fn tpm_request_larger_than_burst_consumes_burst_not_forever() {
        let mut config = HashMap::new();
        config.insert(
            "openai".to_string(),
            ProviderQuota {
                rpm: None,
                tpm: Some(10),
            },
        );
        let map = ProviderQuotaMap::from_config(&config);
        assert!(
            map.try_acquire(&LLMProviderKind::OpenAI, 10_000).is_ok(),
            "a request bigger than burst must consume the burst, not block forever"
        );
        let wait = map
            .try_acquire(&LLMProviderKind::OpenAI, 1)
            .expect_err("burst already spent");
        assert!(wait > Duration::ZERO);
    }

    #[test]
    fn rpm_and_tpm_are_all_or_nothing() {
        let mut config = HashMap::new();
        config.insert(
            "anthropic".to_string(),
            ProviderQuota {
                rpm: Some(10),
                tpm: Some(10),
            },
        );
        let map = ProviderQuotaMap::from_config(&config);
        assert!(map.try_acquire(&LLMProviderKind::Anthropic, 6).is_ok());
        assert!(
            map.try_acquire(&LLMProviderKind::Anthropic, 6).is_err(),
            "TPM remainder is 4"
        );
        assert!(
            map.try_acquire(&LLMProviderKind::Anthropic, 4).is_ok(),
            "failed acquire must not consume TPM"
        );
    }

    #[test]
    fn unknown_provider_is_unlimited() {
        let mut config = HashMap::new();
        config.insert(
            "openai".to_string(),
            ProviderQuota {
                rpm: Some(1),
                tpm: Some(1),
            },
        );
        let map = ProviderQuotaMap::from_config(&config);
        assert!(map.try_acquire(&LLMProviderKind::Anthropic, 10_000).is_ok());
    }

    #[tokio::test]
    async fn acquire_is_immediate_when_unlimited() {
        let map = ProviderQuotaMap::from_config(&HashMap::new());
        tokio::time::timeout(
            Duration::from_millis(50),
            map.acquire(&LLMProviderKind::OpenAI, 1),
        )
        .await
        .expect("unlimited acquire must not wait");
    }
}
