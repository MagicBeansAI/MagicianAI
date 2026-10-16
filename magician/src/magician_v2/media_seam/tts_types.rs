//! Provider-agnostic text-to-speech contract.
//!
//! Adapters return raw audio bytes plus a MIME type so the HTTP layer
//! can stream them straight back to the requesting browser or persist
//! them to the artifact ledger when an explicit replay/debug feature
//! flag is enabled. The contract is intentionally non-streaming for
//! Phase 3 — interactive streaming TTS is Phase 4 territory and a
//! different (realtime voice) provider trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Provider-agnostic emotional color for a TTS utterance. Each
/// adapter maps these to its provider-native representation:
///   * OpenAI's `gpt-4o-mini-tts` — composes a natural-language
///     `instructions` string.
///   * MiniMax's `speech-02-*` family — maps to the native typed
///     `voice_setting.emotion` enum (with a small lossy table for
///     values MiniMax doesn't carry verbatim).
///   * Future providers (ElevenLabs, Azure, etc.) — same pattern,
///     each adapter owns its translation.
///
/// Keep the vocabulary small. The LLM is the author of these
/// attributes (via `<speech emotion="..."` tags on voice replies),
/// so the set should cover the moods a chat reply actually needs
/// without tempting the model to over-act every turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TtsEmotion {
    #[default]
    Neutral,
    Happy,
    Excited,
    Concerned,
    Apologetic,
    Confident,
    Playful,
    Urgent,
    Sad,
    Confused,
}

/// Provider-agnostic delivery style. Layered on top of `TtsEmotion`
/// — emotion = what the speaker feels, style = how they deliver it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsStyle {
    Casual,
    Formal,
    Dramatic,
    Deadpan,
    Warm,
    Clinical,
}

/// Provider-agnostic pacing. Adapters that take a typed `speed`
/// parameter (MiniMax, Azure) map this to a numeric multiplier.
/// Adapters that take free-form instructions (OpenAI) phrase it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TtsPace {
    Slow,
    #[default]
    Normal,
    Fast,
}

/// Provider-agnostic voice-mode override. Distinct from `style`
/// because some providers represent these as separate voice ids
/// (e.g. MiniMax's whisper voice) rather than a delivery hint on
/// the base voice. Adapters that don't support a mode fall back
/// to the default voice + a style hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TtsVoiceMode {
    #[default]
    Default,
    Whisper,
    Announcement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub text: String,
    /// Vendor-specific voice id (`"alloy"`, `"male-1"`, etc.). When
    /// `None` the adapter picks its configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Speech rate (1.0 = normal). Adapters clamp to their supported
    /// range; some ignore this entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
    /// Vendor-specific model id (`"tts-1"`, `"tts-1-hd"`). When `None`
    /// the adapter falls back to its default model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Requested output format. Adapters that don't support the
    /// requested format SHOULD return their default + the real
    /// `content_type` in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Originating message id — propagated into the response so
    /// callers can correlate the synthesized blob with the chat
    /// message that produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Optional emotional color. `None` means "use neutral / let
    /// provider pick". Adapters translate to their native shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion: Option<TtsEmotion>,
    /// Optional delivery style. Stacks with `emotion`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<TtsStyle>,
    /// Optional pacing. `None` is treated as `Normal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace: Option<TtsPace>,
    /// Optional voice-mode override (whisper, announcement).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_mode: Option<TtsVoiceMode>,
    /// Optional short phrase the model wants emphasised. Adapters
    /// surface this either via prosody emphasis (SSML providers) or
    /// by injecting it into the natural-language instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TtsResponse {
    pub audio: bytes::Bytes,
    pub content_type: String,
    pub model: String,
    pub voice: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("tts provider not configured: {0}")]
    NotConfigured(String),
    #[error("tts request rejected: {0}")]
    BadRequest(String),
    #[error("tts upstream returned {status}: {body}")]
    Upstream { status: u16, body: String },
    #[error("tts transport: {0}")]
    Transport(String),
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Stable identifier for this adapter, e.g. `"openai"`. Used by
    /// the registry and the UI's session capability advertisement.
    fn id(&self) -> &str;

    /// Optional human-readable label. Config-defined providers use this so UI
    /// selectors can show operator-owned names instead of deriving labels from
    /// ids and model strings.
    fn label(&self) -> Option<&str> {
        None
    }

    /// Default voice id this provider would use when none is supplied.
    /// Surfaced to the UI so it can render a meaningful default in
    /// the voice picker.
    fn default_voice(&self) -> Option<&str>;

    /// Default model id this provider would use when none is supplied.
    fn default_model(&self) -> &str;

    /// Default response format, when the provider advertises one.
    fn default_format(&self) -> Option<&str> {
        None
    }

    /// Configured voices safe to present to clients.
    fn supported_voices(&self) -> Vec<String> {
        self.default_voice()
            .map(str::to_string)
            .into_iter()
            .collect()
    }

    /// Configured output formats safe to present to clients.
    fn supported_formats(&self) -> Vec<String> {
        self.default_format()
            .map(str::to_string)
            .into_iter()
            .collect()
    }

    /// Whether this provider can yield audio before the utterance completes.
    fn supports_streaming(&self) -> bool {
        false
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError>;

    /// Drop every cached response this adapter holds. Default no-op
    /// for adapters that don't cache; `CachedTtsProvider` overrides
    /// to drain its LRU. Used by the admin
    /// `POST /media/tts/cache/clear` endpoint when an operator
    /// redeploys a model or rotates voice ids and wants the next
    /// synth to hit the upstream fresh.
    ///
    /// Async because the lock-protected state lives behind an
    /// `async`-capable mutex; concrete impls that hold no state can
    /// just leave this as the default.
    async fn clear_cache(&self) {}

    /// Best-effort count of entries currently in this adapter's
    /// cache. Returns 0 for adapters that don't cache. Surfaced by
    /// the cache-clear endpoint so the operator sees how much was
    /// dropped.
    async fn cache_len(&self) -> usize {
        0
    }

    /// Hit / miss / eviction counters for this adapter's cache.
    /// Returns the zero snapshot for adapters that don't cache.
    /// Surfaced by `GET /media/tts/cache/stats` for ops debugging
    /// without dragging in a full metrics framework.
    fn cache_stats(&self) -> TtsCacheStats {
        TtsCacheStats::default()
    }
}

/// Lock-free snapshot of a `CachedTtsProvider`'s counters. Process-
/// local; resets on restart. Returned by `TtsProvider::cache_stats`
/// and exposed through `/media/tts/cache/stats`.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TtsCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}
