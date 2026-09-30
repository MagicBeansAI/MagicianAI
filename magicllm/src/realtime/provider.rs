//! Realtime provider trait — provider-neutral session bootstrap.

use async_trait::async_trait;

use crate::realtime::types::{
    AudioStreamChannel, RealtimeAudioTopology, RealtimeProviderError, RealtimeProviderKind,
    RealtimeSessionDescriptor,
};

/// Bidirectional realtime voice/audio provider.
///
/// Implementations live in `magicllm/src/realtime/<provider>.rs` and
/// are registered via `magician-config.yaml` profiles. The trait is
/// intentionally small — it only covers the upstream-session lifecycle
/// pieces that vary per provider. Session orchestration (rotation,
/// compaction, replay) lives one level up in the magician
/// `VoiceOrchestrator` and is uniform across providers.
#[async_trait]
pub trait RealtimeProvider: Send + Sync {
    /// Stable identifier for this provider (e.g. `"openai-realtime"`,
    /// `"gemini-live"`). Used in logs + telemetry only — never as a
    /// branching key inside magician (use [`kind`] for that).
    fn id(&self) -> &str;

    /// Discriminator the orchestrator can match on when it needs
    /// provider-specific behaviour (rare — most paths stay uniform).
    fn kind(&self) -> RealtimeProviderKind;

    /// Default model identifier this provider uses when the YAML
    /// profile doesn't pin one. Surfaced for diagnostics; the
    /// orchestrator passes the configured model to `create_session`
    /// when it has one in scope.
    fn default_model(&self) -> &str;

    /// Topology the orchestrator + frontend should set up around
    /// this provider. Stable across rotations — the orchestrator
    /// reads this once at session start.
    fn audio_topology(&self) -> RealtimeAudioTopology;

    /// Mint a fresh upstream session. Called on initial start and
    /// on every rotation (proactive, watermark, recovery).
    /// `voice_session_id` is magician's stable per-call id —
    /// providers don't need to interpret it but may stash it as a
    /// debug breadcrumb on upstream telemetry.
    /// `preferred_voice` is a Settings overlay; empty/unknown falls
    /// back to the YAML / provider default.
    async fn create_session(
        &self,
        principal: &str,
        workspace: &str,
        voice_session_id: &str,
        thread_id: Option<&str>,
        preferred_voice: Option<&str>,
    ) -> Result<RealtimeSessionDescriptor, RealtimeProviderError>;

    /// True when the provider supports a native resume protocol
    /// (Gemini's `sessionResumption.handle` etc.). On reconnect the
    /// orchestrator can pass the prior handle back and skip manual
    /// context replay. Defaults to `false` — providers like today's
    /// OpenAI Realtime require the client/orchestrator to re-send
    /// context.
    fn supports_native_resume(&self) -> bool {
        false
    }

    /// Upper bound on how long a single upstream session can live,
    /// in seconds. Used by the orchestrator to schedule proactive
    /// rotation well before the provider's hard cutoff. `None` =
    /// no known cap. Implementations should be conservative —
    /// rotating slightly early beats losing the call to an abrupt
    /// upstream close.
    fn max_session_duration_secs(&self) -> Option<u64> {
        None
    }

    /// Fraction of the provider's context window that triggers a
    /// compaction-then-rotate when crossed. `0.7` means: when 70 %
    /// of the window is consumed, rotate. Tuned per provider so
    /// the rotation lands before quality starts to degrade.
    fn compaction_token_watermark(&self) -> f32 {
        0.7
    }

    /// Open the bidirectional audio stream for a provider whose
    /// topology is [`RealtimeAudioTopology::BackendProxied`]
    /// (Gemini Live etc.). The orchestrator calls this AFTER
    /// `create_session` and hands the channel halves to the
    /// control-WS actor, which pipes browser ↔ provider PCM frames
    /// in both directions.
    ///
    /// Default impl errors with `UnsupportedTopology` — appropriate
    /// for `DirectPeerToPeer` providers (OpenAI Realtime) where
    /// audio rides browser ↔ provider WebRTC and never crosses
    /// magician. Override in any provider that declares
    /// `audio_topology() == BackendProxied`.
    async fn open_proxied_audio(
        &self,
        _descriptor: &RealtimeSessionDescriptor,
    ) -> Result<AudioStreamChannel, RealtimeProviderError> {
        Err(RealtimeProviderError::UnsupportedTopology(format!(
            "provider `{}` declared `{:?}` topology — open_proxied_audio is only valid for BackendProxied",
            self.id(),
            self.audio_topology()
        )))
    }

    /// Tear down an upstream session the provider previously
    /// minted via [`Self::create_session`]. The orchestrator calls
    /// this on:
    ///
    ///   * call end (the most recently minted descriptor)
    ///   * successful rotation (the prior descriptor — the new
    ///     one is now live)
    ///
    /// For `DirectPeerToPeer` providers the default no-op is
    /// correct: the browser closes its WebRTC peer and OpenAI's
    /// ephemeral `ek_…` token expires on its own ~60 s window —
    /// there's no server-side connection to release.
    ///
    /// For `BackendProxied` providers (Gemini Live etc.) the impl
    /// MUST close the upstream WebSocket and release any per-call
    /// vendor resources. Failing to do so leaks one upstream
    /// connection per rotation across a long call.
    ///
    /// Implementations must be tolerant of being called multiple
    /// times with the same descriptor (e.g. retry after partial
    /// failure) — idempotent close, no error on already-closed.
    async fn close_session(
        &self,
        _descriptor: &RealtimeSessionDescriptor,
    ) -> Result<(), RealtimeProviderError> {
        Ok(())
    }
}
