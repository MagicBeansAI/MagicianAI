//! Long-lived voice session lifecycle — tracks the *ephemeral* upstream
//! provider session(s) behind a *stable* magician-side voice session.
//!
//! Why this is a separate side-map from `RealtimeSession`:
//!
//!   - `RealtimeSession` represents a connected media *surface* (mic,
//!     camera, voice — anything that registered via
//!     `/media/sessions`). It outlives upstream provider sessions and
//!     also describes non-voice surfaces, so we don't bloat it with
//!     voice-specific lifecycle fields.
//!   - The voice session id (which equals the media session id) stays
//!     stable across upstream rotations. The provider's session id
//!     (`sess_…` for OpenAI, native resume handle for Gemini) rotates
//!     whenever we:
//!       * proactively refresh before the provider's hard cutoff, or
//!       * reconnect after a transport drop / context-window watermark.
//!   - Each rotation needs to replay context. We cache a compacted
//!     summary here so consecutive rotations don't re-summarise the
//!     entire history from scratch — we just extend the prior summary
//!     with whatever turns were added since `last_compacted_turn_id`.
//!
//! All times are millis since epoch so they round-trip through JSON
//! cleanly and match the chat ledger timestamps.

use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// One voice session's lifecycle state. Cheap to clone — small,
/// inline String/Option fields. Stored by-value in a DashMap entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSessionLifecycle {
    /// Stable voice session id (also the media session id). Never
    /// rotates — survives every upstream session swap.
    pub voice_session_id: String,
    /// Which realtime voice provider minted this session (e.g.
    /// `"openai"`, `"gemini"`). Surfaces in `/resume-context` so the
    /// transport can pick the right replay strategy.
    pub provider: String,
    /// Current upstream provider session id. `None` until the first
    /// successful bootstrap completes. For OpenAI this is the
    /// `session.id` from the first `session.created` event; for
    /// Gemini it'll be the native resume handle.
    pub upstream_provider_session_id: Option<String>,
    /// When the *current* upstream session was minted (ms epoch).
    /// Used by the frontend to schedule proactive rotation.
    pub upstream_minted_at_ms: i64,
    /// Best-effort upper bound on when the current upstream session
    /// will be force-closed by the provider. Computed as
    /// `upstream_minted_at_ms + max_session_duration_secs * 1000`.
    /// `None` if the provider doesn't expose a duration cap.
    pub upstream_expires_at_hint_ms: Option<i64>,
    /// Total number of upstream rotations on this voice session.
    /// Zero on first mint, incremented on every refresh.
    pub rotation_count: u32,
    /// Compacted "summary so far" cached for cheap reuse on the next
    /// rotation. `None` until the compactor runs at least once.
    pub compacted_summary: Option<String>,
    /// Chat-ledger message id of the most recent turn included in
    /// `compacted_summary`. The compactor only re-summarises turns
    /// strictly newer than this. `None` until first compaction.
    pub last_compacted_turn_id: Option<String>,
    /// Last time anything in this lifecycle was touched (ms epoch).
    /// Used by `prune_idle` to evict records for calls that ended
    /// without a clean teardown.
    pub last_activity_at_ms: i64,
}

impl VoiceSessionLifecycle {
    pub fn new(voice_session_id: String, provider: String, now_ms: i64) -> Self {
        Self {
            voice_session_id,
            provider,
            upstream_provider_session_id: None,
            upstream_minted_at_ms: now_ms,
            upstream_expires_at_hint_ms: None,
            rotation_count: 0,
            compacted_summary: None,
            last_compacted_turn_id: None,
            last_activity_at_ms: now_ms,
        }
    }
}

/// Process-local registry of voice session lifecycles. Cloning is
/// cheap (Arc inside). Mutations are serialised per-entry via DashMap.
#[derive(Clone, Default)]
pub struct VoiceSessionLifecycleStore {
    entries: Arc<DashMap<String, VoiceSessionLifecycle>>,
}

impl VoiceSessionLifecycleStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record the *initial* bootstrap of a voice session. Replaces
    /// any prior entry under the same id (e.g. a stale lifecycle
    /// from an earlier call that ended uncleanly).
    pub fn record_minted(
        &self,
        voice_session_id: &str,
        provider: &str,
        upstream_provider_session_id: Option<String>,
        max_session_duration_secs: Option<u64>,
        now_ms: i64,
    ) {
        let mut record =
            VoiceSessionLifecycle::new(voice_session_id.to_string(), provider.to_string(), now_ms);
        record.upstream_provider_session_id = upstream_provider_session_id;
        record.upstream_expires_at_hint_ms = max_session_duration_secs.map(|secs| {
            now_ms.saturating_add(i64::try_from(secs).unwrap_or(0).saturating_mul(1000))
        });
        self.entries.insert(voice_session_id.to_string(), record);
    }

    /// Record a successful rotation onto a new upstream session.
    /// Bumps `rotation_count` and updates the mint/expiry timestamps.
    /// No-ops (with a `false` return) when the voice session id is
    /// unknown — callers should only ever rotate sessions they
    /// previously minted.
    pub fn record_rotated(
        &self,
        voice_session_id: &str,
        upstream_provider_session_id: Option<String>,
        max_session_duration_secs: Option<u64>,
        now_ms: i64,
    ) -> bool {
        let Some(mut entry) = self.entries.get_mut(voice_session_id) else {
            return false;
        };
        entry.upstream_provider_session_id = upstream_provider_session_id;
        entry.upstream_minted_at_ms = now_ms;
        entry.upstream_expires_at_hint_ms = max_session_duration_secs.map(|secs| {
            now_ms.saturating_add(i64::try_from(secs).unwrap_or(0).saturating_mul(1000))
        });
        entry.rotation_count = entry.rotation_count.saturating_add(1);
        entry.last_activity_at_ms = now_ms;
        true
    }

    /// Update the cached compaction state after a successful summary
    /// run. Safe to call repeatedly — each call clobbers the prior
    /// summary with whatever the compactor produced.
    pub fn update_summary(
        &self,
        voice_session_id: &str,
        summary: String,
        last_compacted_turn_id: Option<String>,
        now_ms: i64,
    ) -> bool {
        let Some(mut entry) = self.entries.get_mut(voice_session_id) else {
            return false;
        };
        entry.compacted_summary = Some(summary);
        entry.last_compacted_turn_id = last_compacted_turn_id;
        entry.last_activity_at_ms = now_ms;
        true
    }

    /// Snapshot a lifecycle record. Returns a clone — modifications
    /// to the returned value won't write back to the store.
    pub fn get(&self, voice_session_id: &str) -> Option<VoiceSessionLifecycle> {
        self.entries.get(voice_session_id).map(|e| e.clone())
    }

    /// Remove the lifecycle entry for a voice session. Called on
    /// voice-call teardown so abandoned entries don't accumulate
    /// across long-running processes.
    pub fn remove(&self, voice_session_id: &str) {
        self.entries.remove(voice_session_id);
    }

    /// Drop entries older than `max_idle_ms`. Best-effort cleanup
    /// for cases where the frontend disconnected without calling
    /// teardown (network failure, browser crash). Returns the
    /// number of entries evicted.
    pub fn prune_idle(&self, now_ms: i64, max_idle_ms: i64) -> usize {
        let mut victims = Vec::new();
        for entry in self.entries.iter() {
            if now_ms.saturating_sub(entry.last_activity_at_ms) > max_idle_ms {
                victims.push(entry.key().clone());
            }
        }
        for key in &victims {
            self.entries.remove(key);
        }
        victims.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    #[test]
    fn record_minted_creates_entry_with_expiry_hint() {
        let store = VoiceSessionLifecycleStore::new();
        let t0 = now();
        store.record_minted("vs-1", "openai", Some("sess_a".into()), Some(1800), t0);
        let entry = store.get("vs-1").expect("entry should exist");
        assert_eq!(entry.provider, "openai");
        assert_eq!(entry.rotation_count, 0);
        assert_eq!(
            entry.upstream_provider_session_id.as_deref(),
            Some("sess_a")
        );
        assert_eq!(entry.upstream_expires_at_hint_ms, Some(t0 + 1_800_000));
    }

    #[test]
    fn record_rotated_bumps_counter_and_refreshes_expiry() {
        let store = VoiceSessionLifecycleStore::new();
        let t0 = now();
        store.record_minted("vs-1", "openai", Some("sess_a".into()), Some(1800), t0);
        let t1 = t0 + 1_700_000;
        let rotated = store.record_rotated("vs-1", Some("sess_b".into()), Some(1800), t1);
        assert!(rotated);
        let entry = store.get("vs-1").unwrap();
        assert_eq!(entry.rotation_count, 1);
        assert_eq!(
            entry.upstream_provider_session_id.as_deref(),
            Some("sess_b")
        );
        assert_eq!(entry.upstream_expires_at_hint_ms, Some(t1 + 1_800_000));
    }

    #[test]
    fn record_rotated_unknown_session_returns_false() {
        let store = VoiceSessionLifecycleStore::new();
        let rotated = store.record_rotated("vs-missing", None, None, now());
        assert!(!rotated);
    }

    #[test]
    fn update_summary_caches_compaction_state() {
        let store = VoiceSessionLifecycleStore::new();
        let t0 = now();
        store.record_minted("vs-1", "openai", None, None, t0);
        let updated = store.update_summary(
            "vs-1",
            "Summary so far".to_string(),
            Some("msg-7".to_string()),
            t0 + 1000,
        );
        assert!(updated);
        let entry = store.get("vs-1").unwrap();
        assert_eq!(entry.compacted_summary.as_deref(), Some("Summary so far"));
        assert_eq!(entry.last_compacted_turn_id.as_deref(), Some("msg-7"));
    }

    #[test]
    fn prune_idle_evicts_stale_entries() {
        let store = VoiceSessionLifecycleStore::new();
        let t0 = now();
        store.record_minted("vs-old", "openai", None, None, t0 - 3_600_000);
        store.record_minted("vs-fresh", "openai", None, None, t0);
        let evicted = store.prune_idle(t0, 60_000);
        assert_eq!(evicted, 1);
        assert!(store.get("vs-old").is_none());
        assert!(store.get("vs-fresh").is_some());
    }
}
