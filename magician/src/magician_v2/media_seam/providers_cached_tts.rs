//! Decorator that puts a process-local LRU cache in front of any
//! `TtsProvider`. Identical requests (same text + delivery hints +
//! voice/model/format) return the cached `TtsResponse` without
//! hitting the upstream.
//!
//! Why a decorator and not a registry-level cache:
//!
//! * Per-provider caching is symmetric with how the chain works —
//!   each adapter has its own cache, so swapping the primary doesn't
//!   poison the fallback's cache and vice versa.
//! * The wrapped provider's `id()` / `default_voice()` /
//!   `default_model()` pass through unchanged, so the registry,
//!   snapshot endpoint, and chain rotation logic are oblivious to
//!   caching.
//! * Composes with future cache strategies — disk cache, distributed
//!   cache, TTL — by stacking decorators.
//!
//! Cache discipline:
//!
//! * Only successful responses are cached. Errors never poison.
//! * Cache key includes every field the provider actually uses, so
//!   `<speech emotion="happy">hi</speech>` and
//!   `<speech emotion="sad">hi</speech>` produce distinct entries.
//! * Bounded by entry count, not bytes. Audio for a short utterance
//!   is ~10–30 KB; 200 entries ≈ 4 MB worst-case — fine.
//! * FIFO eviction. Strict LRU would need access-time bookkeeping
//!   that adds locking complexity; FIFO is sufficient because hot
//!   replies (`Done — moved to inbox.`, `On it.`, etc.) cycle in
//!   quickly enough that even FIFO retains them.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::debug;

use crate::magician_v2::media_seam::tts::{
    TtsCacheStats, TtsEmotion, TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse, TtsStyle,
    TtsVoiceMode,
};

/// Decorator that caches successful `TtsProvider::synthesize` results.
#[derive(Clone)]
pub struct CachedTtsProvider {
    inner: Arc<dyn TtsProvider>,
    state: Arc<Mutex<CacheState>>,
    capacity: usize,
    enabled_gate: Option<Arc<AtomicBool>>,
    generation_gate: Option<Arc<AtomicU64>>,
    /// Lock-free counters surfaced via `cache_stats()`. Bumped on
    /// every synth call so ops can read hit rate without locking the
    /// HashMap. Process-local — resets on restart.
    counters: Arc<CacheCounters>,
}

#[derive(Default)]
struct CacheCounters {
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

struct CacheState {
    entries: HashMap<CacheKey, TtsResponse>,
    order: VecDeque<CacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    text: String,
    voice: Option<String>,
    rate_bits: Option<u32>,
    model: Option<String>,
    format: Option<String>,
    emotion: Option<TtsEmotion>,
    style: Option<TtsStyle>,
    pace: Option<TtsPace>,
    voice_mode: Option<TtsVoiceMode>,
    emphasis: Option<String>,
}

impl CacheKey {
    fn from_request(request: &TtsRequest) -> Self {
        Self {
            text: request.text.clone(),
            voice: request.voice.clone(),
            // `f32` isn't `Hash`. Map through its bit representation so
            // 1.0 and 1.0 always hash the same. NaN is intentionally
            // not handled — a NaN rate is a caller bug.
            rate_bits: request.rate.map(|r| r.to_bits()),
            model: request.model.clone(),
            format: request.format.clone(),
            emotion: request.emotion,
            style: request.style,
            pace: request.pace,
            voice_mode: request.voice_mode,
            emphasis: request.emphasis.clone(),
        }
    }
}

impl CachedTtsProvider {
    pub fn new(inner: Arc<dyn TtsProvider>, capacity: usize) -> Self {
        Self::new_with_gate(inner, capacity, None, None)
    }

    pub fn new_gated(
        inner: Arc<dyn TtsProvider>,
        capacity: usize,
        enabled_gate: Arc<AtomicBool>,
        generation_gate: Arc<AtomicU64>,
    ) -> Self {
        Self::new_with_gate(inner, capacity, Some(enabled_gate), Some(generation_gate))
    }

    fn new_with_gate(
        inner: Arc<dyn TtsProvider>,
        capacity: usize,
        enabled_gate: Option<Arc<AtomicBool>>,
        generation_gate: Option<Arc<AtomicU64>>,
    ) -> Self {
        Self {
            inner,
            state: Arc::new(Mutex::new(CacheState {
                entries: HashMap::with_capacity(capacity.min(256)),
                order: VecDeque::with_capacity(capacity.min(256)),
            })),
            capacity,
            enabled_gate,
            generation_gate,
            counters: Arc::new(CacheCounters::default()),
        }
    }

    /// Wrap `inner` with a cache of up to `capacity` entries. A
    /// `capacity` of 0 disables caching (the decorator becomes a
    /// pass-through).
    pub fn wrap(inner: Arc<dyn TtsProvider>, capacity: usize) -> Arc<dyn TtsProvider> {
        Arc::new(Self::new(inner, capacity))
    }

    pub async fn clear_entries(&self) {
        let mut state = self.state.lock().await;
        state.entries.clear();
        state.order.clear();
    }

    fn begin_gate(&self) -> Result<Option<u64>, TtsError> {
        let Some(enabled) = self.enabled_gate.as_ref() else {
            return Ok(None);
        };
        let generation = self
            .generation_gate
            .as_ref()
            .map(|generation| generation.load(Ordering::Acquire));
        if !enabled.load(Ordering::Acquire) {
            return Err(self.disabled_error());
        }
        self.ensure_gate_current(generation)?;
        Ok(generation)
    }

    fn ensure_gate_current(&self, expected_generation: Option<u64>) -> Result<(), TtsError> {
        if self
            .enabled_gate
            .as_ref()
            .is_some_and(|enabled| !enabled.load(Ordering::Acquire))
            || expected_generation.is_some_and(|expected| {
                self.generation_gate
                    .as_ref()
                    .is_some_and(|generation| generation.load(Ordering::Acquire) != expected)
            })
        {
            Err(self.disabled_error())
        } else {
            Ok(())
        }
    }

    fn disabled_error(&self) -> TtsError {
        TtsError::NotConfigured(format!("{} is disabled", self.inner.id()))
    }
}

#[async_trait]
impl TtsProvider for CachedTtsProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn label(&self) -> Option<&str> {
        self.inner.label()
    }

    fn default_voice(&self) -> Option<&str> {
        self.inner.default_voice()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn default_format(&self) -> Option<&str> {
        self.inner.default_format()
    }

    fn supported_voices(&self) -> Vec<String> {
        self.inner.supported_voices()
    }

    fn supported_formats(&self) -> Vec<String> {
        self.inner.supported_formats()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn clear_cache(&self) {
        self.clear_entries().await;
        // Also clear the wrapped provider in case someone stacks
        // decorators. Default impl is a no-op so this is cheap.
        // Drop the lock before recursing so a stacked CachedTtsProvider
        // can take its own lock without deadlocking.
        self.inner.clear_cache().await;
    }

    async fn cache_len(&self) -> usize {
        self.state.lock().await.entries.len()
    }

    fn cache_stats(&self) -> TtsCacheStats {
        TtsCacheStats {
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            evictions: self.counters.evictions.load(Ordering::Relaxed),
        }
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        let gate_generation = self.begin_gate()?;
        if self.capacity == 0 {
            // Bypass-mode bookkeeping: count as a miss so ops can see
            // the cache was hit-rate=0% (vs. "no traffic at all").
            self.counters.misses.fetch_add(1, Ordering::Relaxed);
            let response = self.inner.synthesize(request).await?;
            self.ensure_gate_current(gate_generation)?;
            return Ok(response);
        }
        let key = CacheKey::from_request(&request);
        let cached = {
            let state = self.state.lock().await;
            state.entries.get(&key).cloned()
        };
        if let Some(mut response) = cached {
            self.ensure_gate_current(gate_generation)?;
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            debug!(
                provider = self.inner.id(),
                text_len = request.text.len(),
                "[TTS-CACHE] hit"
            );
            // We MUST return a response stamped with the requester's
            // message_id so callers can correlate, even if a prior
            // request with a different message_id populated the entry.
            response.message_id = request.message_id.clone();
            return Ok(response);
        }
        self.counters.misses.fetch_add(1, Ordering::Relaxed);

        let response = self.inner.synthesize(request.clone()).await?;
        self.ensure_gate_current(gate_generation)?;
        // Cache the value *with* the originating message_id stripped —
        // we always overwrite it on read above, so storing one tags
        // every cached entry with a stale id.
        let mut to_cache = response.clone();
        to_cache.message_id = None;

        let mut state = self.state.lock().await;
        self.ensure_gate_current(gate_generation)?;
        if state.entries.len() >= self.capacity {
            if let Some(evicted) = state.order.pop_front() {
                state.entries.remove(&evicted);
                self.counters.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Could race: another concurrent `synthesize` might have
        // populated the same key between the early-out check and here.
        // That's fine — last writer wins and we don't push a dup into
        // the FIFO order (would double-evict the entry later).
        if !state.entries.contains_key(&key) {
            state.order.push_back(key.clone());
        }
        state.entries.insert(key, to_cache);
        Ok(response)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use bytes::Bytes;

    use crate::magician_v2::media_seam::*;

    struct CountingProvider {
        id: &'static str,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl TtsProvider for CountingProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn default_voice(&self) -> Option<&str> {
            Some("test-voice")
        }
        fn default_model(&self) -> &str {
            "test-model"
        }
        async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TtsResponse {
                audio: Bytes::from(request.text.into_bytes()),
                content_type: "audio/mpeg".into(),
                model: "test-model".into(),
                voice: Some("test-voice".into()),
                message_id: request.message_id,
            })
        }
    }

    fn fake_request(text: &str) -> TtsRequest {
        TtsRequest {
            text: text.into(),
            voice: None,
            rate: None,
            model: None,
            format: None,
            message_id: None,
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }
    }

    #[tokio::test]
    async fn second_identical_request_is_a_cache_hit() {
        use std::sync::Arc;
        let counting = Arc::new(CountingProvider {
            id: "fake",
            calls: AtomicUsize::new(0),
        });
        let cached = CachedTtsProvider::wrap(counting.clone(), 10);
        cached.synthesize(fake_request("hi")).await.unwrap();
        cached.synthesize(fake_request("hi")).await.unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn distinct_emotion_keys_separately() {
        use std::sync::Arc;
        let counting = Arc::new(CountingProvider {
            id: "fake",
            calls: AtomicUsize::new(0),
        });
        let cached = CachedTtsProvider::wrap(counting.clone(), 10);
        let mut a = fake_request("hi");
        a.emotion = Some(TtsEmotion::Happy);
        let mut b = fake_request("hi");
        b.emotion = Some(TtsEmotion::Sad);
        cached.synthesize(a.clone()).await.unwrap();
        cached.synthesize(b.clone()).await.unwrap();
        cached.synthesize(a).await.unwrap(); // cache hit
        cached.synthesize(b).await.unwrap(); // cache hit
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn message_id_does_not_split_cache() {
        use std::sync::Arc;
        let counting = Arc::new(CountingProvider {
            id: "fake",
            calls: AtomicUsize::new(0),
        });
        let cached = CachedTtsProvider::wrap(counting.clone(), 10);
        let mut a = fake_request("hi");
        a.message_id = Some("msg-1".into());
        let mut b = fake_request("hi");
        b.message_id = Some("msg-2".into());
        let resp_a = cached.synthesize(a).await.unwrap();
        let resp_b = cached.synthesize(b).await.unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
        // The cached response is restamped with the requester's msg id.
        assert_eq!(resp_a.message_id.as_deref(), Some("msg-1"));
        assert_eq!(resp_b.message_id.as_deref(), Some("msg-2"));
    }

    #[tokio::test]
    async fn capacity_zero_is_passthrough() {
        use std::sync::Arc;
        let counting = Arc::new(CountingProvider {
            id: "fake",
            calls: AtomicUsize::new(0),
        });
        let cached = CachedTtsProvider::wrap(counting.clone(), 0);
        cached.synthesize(fake_request("hi")).await.unwrap();
        cached.synthesize(fake_request("hi")).await.unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn fifo_evicts_oldest_when_full() {
        use std::sync::Arc;
        let counting = Arc::new(CountingProvider {
            id: "fake",
            calls: AtomicUsize::new(0),
        });
        let cached = CachedTtsProvider::wrap(counting.clone(), 2);
        cached.synthesize(fake_request("one")).await.unwrap();
        cached.synthesize(fake_request("two")).await.unwrap();
        cached.synthesize(fake_request("three")).await.unwrap();
        // "one" should have been evicted; calling it again is a miss.
        cached.synthesize(fake_request("one")).await.unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        struct FlakyProvider {
            calls: AtomicUsize,
        }
        #[async_trait]
        impl TtsProvider for FlakyProvider {
            fn id(&self) -> &str {
                "flaky"
            }
            fn default_voice(&self) -> Option<&str> {
                None
            }
            fn default_model(&self) -> &str {
                "flaky-model"
            }
            async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    return Err(TtsError::Upstream {
                        status: 502,
                        body: "boom".into(),
                    });
                }
                Ok(TtsResponse {
                    audio: Bytes::from(request.text.into_bytes()),
                    content_type: "audio/mpeg".into(),
                    model: "flaky-model".into(),
                    voice: None,
                    message_id: request.message_id,
                })
            }
        }
        use std::sync::Arc;
        let flaky = Arc::new(FlakyProvider {
            calls: AtomicUsize::new(0),
        });
        let cached = CachedTtsProvider::wrap(flaky.clone(), 10);
        assert!(cached.synthesize(fake_request("hi")).await.is_err());
        // Second call should hit upstream again (the error wasn't cached).
        assert!(cached.synthesize(fake_request("hi")).await.is_ok());
        assert_eq!(flaky.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn disabled_gate_rejects_cached_audio_without_calling_inner_provider() {
        use std::sync::Arc;
        let counting = Arc::new(CountingProvider {
            id: "fluid-test",
            calls: AtomicUsize::new(0),
        });
        let enabled = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(AtomicU64::new(0));
        let cached = CachedTtsProvider::new_gated(
            counting.clone(),
            10,
            Arc::clone(&enabled),
            Arc::clone(&generation),
        );
        cached.synthesize(fake_request("hi")).await.unwrap();
        assert_eq!(cached.cache_len().await, 1);

        enabled.store(false, Ordering::Release);
        let error = cached
            .synthesize(fake_request("hi"))
            .await
            .expect_err("disabled cache must not serve a hit");
        assert!(matches!(error, TtsError::NotConfigured(_)));
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
    }
}
