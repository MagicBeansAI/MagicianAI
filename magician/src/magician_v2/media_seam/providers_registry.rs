//! Process-local registry of TTS / STT providers.
//!
//! Construction happens at boot in `bin/magician.rs`. Handlers reach
//! for whatever is currently registered via `Arc<MediaProviderRegistry>`
//! and gracefully fall back when nothing is configured (browser-native
//! TTS for the synth path).
//!
//! Provider chain semantics: TTS and STT each carry an ordered chain
//! (`Vec<Arc<...>>`). The first entry is the primary; subsequent
//! entries are fallbacks the synthesize/transcribe handlers rotate
//! through when the primary returns an `Upstream` or `Transport`
//! error. Boot order picks the policy — e.g. OpenAI primary +
//! MiniMax fallback is `with_tts(openai).with_tts_fallback(minimax)`.
//!
//! Realtime voice providers are resolved per-call through
//! `OperationLlmRouter::resolve_realtime_provider` against the
//! `magician-config.yaml > realtime_voice` profile section, so config
//! + provider selection live in one declarative place. We ALSO cache
//! the resolved provider here so the `/media/providers` introspection
//! endpoint can advertise its existence without re-resolving on every
//! request — the frontend's `VoiceCallButton` gate
//! (`providers.realtime_voice !== null`) reads this snapshot.
//! See `magicllm::realtime` for the trait.

use std::sync::Arc;

use magicllm::config::RealtimeVoiceMode;
use magicllm::realtime::{RealtimeAudioTopology, RealtimeProvider};

use crate::magician_v2::media_seam::stt::SttProvider;
use crate::magician_v2::media_seam::tts::TtsProvider;
use crate::magician_v2::media_seam::{DiarizationProvider, StreamingSttProvider, VadProvider};

// Provider id/model defaults shared with the magician-media crate engines.
pub const MACOS_SPEECH_PROVIDER_ID: &str = "macos_speech";
pub const MACOS_SPEECH_DEFAULT_MODEL: &str = "macos_speech";
pub const MACOS_TTS_PROVIDER_ID: &str = "macos_tts";
pub const MACOS_TTS_DEFAULT_MODEL: &str = "av_speech_synthesizer";

#[derive(Clone, Default)]

pub struct MediaProviderRegistry {
    /// Ordered TTS chain. `[0]` is the primary; later entries are
    /// fallbacks the synth handler rotates through on
    /// `Upstream`/`Transport` errors. Empty = no provider configured.
    tts_chain: Vec<Arc<dyn TtsProvider>>,
    /// Ordered STT chain. Same fallback semantics. Streaming and
    /// non-streaming handlers both walk this chain.
    stt_chain: Vec<Arc<dyn SttProvider>>,
    /// Streaming STT adapters available to resolved Meeting and Listening
    /// profiles. Phase 1 advertises these without changing legacy execution.
    streaming_stt_chain: Vec<Arc<dyn StreamingSttProvider>>,
    /// Reusable VAD stage adapters. Empty until a platform engine is wired.
    vad_chain: Vec<Arc<dyn VadProvider>>,
    /// Independent diarization stage adapters. Embedded STT diarization does
    /// not appear here because it cannot be selected independently.
    diarization_chain: Vec<Arc<dyn DiarizationProvider>>,
    /// Realtime voice provider resolved from the `realtime_voice`
    /// operation mapping in `magician-config.yaml`. Surfaced in the
    /// `/media/providers` snapshot so the frontend can flip the
    /// voice-call affordance from "unavailable" to "ready to call".
    /// `None` when no profile is mapped OR the provider failed to
    /// instantiate (missing API key, unknown provider kind, …).
    realtime_voice: Option<Arc<dyn RealtimeProvider>>,
    realtime_voice_profiles: Vec<RealtimeVoiceProfileInfo>,
    realtime_voice_default_profile: Option<String>,
}

impl std::fmt::Debug for MediaProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaProviderRegistry")
            .field(
                "tts_chain",
                &self.tts_chain.iter().map(|p| p.id()).collect::<Vec<_>>(),
            )
            .field(
                "stt_chain",
                &self.stt_chain.iter().map(|p| p.id()).collect::<Vec<_>>(),
            )
            .field(
                "streaming_stt_chain",
                &self
                    .streaming_stt_chain
                    .iter()
                    .map(|p| p.id())
                    .collect::<Vec<_>>(),
            )
            .field(
                "vad_chain",
                &self.vad_chain.iter().map(|p| p.id()).collect::<Vec<_>>(),
            )
            .field(
                "diarization_chain",
                &self
                    .diarization_chain
                    .iter()
                    .map(|p| p.id())
                    .collect::<Vec<_>>(),
            )
            .field(
                "realtime_voice",
                &self.realtime_voice.as_ref().map(|p| p.id()),
            )
            .field("realtime_voice_profiles", &self.realtime_voice_profiles)
            .field(
                "realtime_voice_default_profile",
                &self.realtime_voice_default_profile,
            )
            .finish()
    }
}

impl MediaProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the primary TTS provider. Replaces any existing primary.
    /// Existing fallbacks are preserved — call this when you want to
    /// swap the head of the chain without touching the tail.
    pub fn with_tts(mut self, provider: Arc<dyn TtsProvider>) -> Self {
        if self.tts_chain.is_empty() {
            self.tts_chain.push(provider);
        } else {
            self.tts_chain[0] = provider;
        }
        self
    }

    /// Append a fallback TTS provider to the chain. Order of calls
    /// determines fallback order — first appended is tried first.
    pub fn with_tts_fallback(mut self, provider: Arc<dyn TtsProvider>) -> Self {
        self.tts_chain.push(provider);
        self
    }

    pub fn with_stt(mut self, provider: Arc<dyn SttProvider>) -> Self {
        if self.stt_chain.is_empty() {
            self.stt_chain.push(provider);
        } else {
            self.stt_chain[0] = provider;
        }
        self
    }

    pub fn with_stt_fallback(mut self, provider: Arc<dyn SttProvider>) -> Self {
        self.stt_chain.push(provider);
        self
    }

    pub fn with_streaming_stt(mut self, provider: Arc<dyn StreamingSttProvider>) -> Self {
        if self.streaming_stt_chain.is_empty() {
            self.streaming_stt_chain.push(provider);
        } else {
            self.streaming_stt_chain[0] = provider;
        }
        self
    }

    pub fn with_streaming_stt_fallback(mut self, provider: Arc<dyn StreamingSttProvider>) -> Self {
        self.streaming_stt_chain.push(provider);
        self
    }

    pub fn with_vad(mut self, provider: Arc<dyn VadProvider>) -> Self {
        if self.vad_chain.is_empty() {
            self.vad_chain.push(provider);
        } else {
            self.vad_chain[0] = provider;
        }
        self
    }

    pub fn with_vad_fallback(mut self, provider: Arc<dyn VadProvider>) -> Self {
        self.vad_chain.push(provider);
        self
    }

    pub fn with_diarization(mut self, provider: Arc<dyn DiarizationProvider>) -> Self {
        if self.diarization_chain.is_empty() {
            self.diarization_chain.push(provider);
        } else {
            self.diarization_chain[0] = provider;
        }
        self
    }

    pub fn with_diarization_fallback(mut self, provider: Arc<dyn DiarizationProvider>) -> Self {
        self.diarization_chain.push(provider);
        self
    }

    /// Register the resolved realtime voice provider so the
    /// `/media/providers` snapshot can advertise its existence. Called
    /// once at boot from the same place that wires the orchestrator's
    /// `OperationLlmRouter::resolve_realtime_provider` lookup, so the
    /// registry and the orchestrator agree on what's configured.
    pub fn with_realtime_voice(mut self, provider: Arc<dyn RealtimeProvider>) -> Self {
        self.realtime_voice = Some(provider);
        self
    }

    pub fn with_realtime_voice_profiles(
        mut self,
        default_profile: Option<String>,
        profiles: Vec<RealtimeVoiceProfileInfo>,
    ) -> Self {
        self.realtime_voice_default_profile = default_profile;
        self.realtime_voice_profiles = profiles;
        self
    }

    pub fn realtime_voice(&self) -> Option<Arc<dyn RealtimeProvider>> {
        self.realtime_voice.clone()
    }

    pub fn has_realtime_voice(&self) -> bool {
        self.realtime_voice.is_some()
    }

    /// Primary TTS provider, or `None` when none are configured.
    /// Kept for compat with callers that don't need the chain.
    pub fn tts(&self) -> Option<Arc<dyn TtsProvider>> {
        self.tts_chain.first().cloned()
    }

    /// Full ordered TTS chain — primary first, fallbacks after.
    pub fn tts_chain(&self) -> Vec<Arc<dyn TtsProvider>> {
        self.tts_chain.clone()
    }

    /// Ordered TTS chain honoring an optional request-level preference.
    /// `None`, `auto`, and `default` keep boot order. A concrete provider id
    /// moves that provider to the front while preserving the remaining
    /// providers as fallbacks.
    pub fn tts_chain_for(&self, preferred_provider: Option<&str>) -> Vec<Arc<dyn TtsProvider>> {
        let preferred = preferred_provider
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| !value.eq_ignore_ascii_case("auto"))
            .filter(|value| !value.eq_ignore_ascii_case("default"));
        let Some(preferred) = preferred else {
            return self.tts_chain();
        };
        let Some(primary) = self
            .tts_chain
            .iter()
            .find(|provider| provider.id().eq_ignore_ascii_case(preferred))
            .cloned()
        else {
            return self.tts_chain();
        };
        let mut chain = Vec::with_capacity(self.tts_chain.len());
        chain.push(primary);
        chain.extend(
            self.tts_chain
                .iter()
                .filter(|provider| !provider.id().eq_ignore_ascii_case(preferred))
                .cloned(),
        );
        chain
    }

    pub fn stt(&self) -> Option<Arc<dyn SttProvider>> {
        self.stt_chain.first().cloned()
    }

    pub fn stt_chain(&self) -> Vec<Arc<dyn SttProvider>> {
        self.stt_chain.clone()
    }

    pub fn streaming_stt_chain(&self) -> Vec<Arc<dyn StreamingSttProvider>> {
        self.streaming_stt_chain.clone()
    }

    pub fn vad_chain(&self) -> Vec<Arc<dyn VadProvider>> {
        self.vad_chain.clone()
    }

    pub fn diarization_chain(&self) -> Vec<Arc<dyn DiarizationProvider>> {
        self.diarization_chain.clone()
    }

    /// Ordered STT chain honoring an optional request-level
    /// preference. `None`, `auto`, and `default` keep boot order. A
    /// concrete provider id moves that provider to the front while
    /// preserving the remaining providers as fallbacks.
    pub fn stt_chain_for(&self, preferred_provider: Option<&str>) -> Vec<Arc<dyn SttProvider>> {
        let preferred = preferred_provider
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| !value.eq_ignore_ascii_case("auto"))
            .filter(|value| !value.eq_ignore_ascii_case("default"));
        let Some(preferred) = preferred else {
            return self.stt_chain();
        };
        let Some(primary) = self
            .stt_chain
            .iter()
            .find(|provider| provider.id().eq_ignore_ascii_case(preferred))
            .cloned()
        else {
            return self.stt_chain();
        };
        let mut chain = Vec::with_capacity(self.stt_chain.len());
        chain.push(primary);
        chain.extend(
            self.stt_chain
                .iter()
                .filter(|provider| !provider.id().eq_ignore_ascii_case(preferred))
                .cloned(),
        );
        chain
    }

    pub fn has_tts(&self) -> bool {
        !self.tts_chain.is_empty()
    }

    pub fn has_stt(&self) -> bool {
        !self.stt_chain.is_empty()
    }

    pub fn has_streaming_stt(&self) -> bool {
        !self.streaming_stt_chain.is_empty()
    }

    pub fn has_vad(&self) -> bool {
        !self.vad_chain.is_empty()
    }

    /// Snapshot suitable for the `/media/providers` introspection
    /// endpoint — pure data, no Arc reach-throughs. Exposes the
    /// primary plus the fallback chain so the UI can render
    /// "OpenAI (→ MiniMax fallback)".
    pub fn snapshot(&self) -> ProviderSnapshot {
        let tts_chain: Vec<ProviderInfo> = self
            .tts_chain
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().to_string(),
                label: p.label().map(str::to_owned),
                model: p.default_model().to_string(),
                voice: p.default_voice().map(str::to_owned),
                format: p.default_format().map(str::to_owned),
                voices: p.supported_voices(),
                formats: p.supported_formats(),
                streaming: p.supports_streaming(),
            })
            .collect();
        let stt_chain: Vec<ProviderInfo> = self
            .stt_chain
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().to_string(),
                label: p.label().map(str::to_owned),
                model: p.default_model().to_string(),
                voice: None,
                format: None,
                voices: Vec::new(),
                formats: Vec::new(),
                streaming: false,
            })
            .collect();
        let realtime_voice = self.realtime_voice.as_ref().map(|p| ProviderInfo {
            id: p.id().to_string(),
            label: None,
            model: p.default_model().to_string(),
            voice: None,
            format: None,
            voices: Vec::new(),
            formats: Vec::new(),
            streaming: true,
        });
        ProviderSnapshot {
            tts: tts_chain.first().cloned(),
            tts_fallbacks: if tts_chain.len() > 1 {
                Some(tts_chain[1..].to_vec())
            } else {
                None
            },
            stt: stt_chain.first().cloned(),
            stt_fallbacks: if stt_chain.len() > 1 {
                Some(stt_chain[1..].to_vec())
            } else {
                None
            },
            realtime_voice,
            realtime_voice_profiles: self.realtime_voice_profiles.clone(),
            realtime_voice_default_profile: self.realtime_voice_default_profile.clone(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default)]
    pub streaming: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RealtimeVoiceChoiceInfo {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RealtimeVoiceProfileInfo {
    pub profile_id: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub topology: RealtimeAudioTopology,
    pub mode: RealtimeVoiceMode,
    /// YAML / provider default. User override lives in media preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<RealtimeVoiceChoiceInfo>,
    /// Configured provider turn boundary. Clients use this only to seed a
    /// missing device-local preference; subsequent choices remain local.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_detection_mode: Option<String>,
    /// Input transcription configured on this realtime profile. `local` means
    /// Magician tees backend-proxied PCM through its call-scoped STT chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcription_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcription_fallback_model: Option<String>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_target_language: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts: Option<ProviderInfo>,
    /// Ordered fallback chain (excluding primary). `None` when no
    /// fallbacks are configured. UI uses this to render "→ X
    /// fallback".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_fallbacks: Option<Vec<ProviderInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt: Option<ProviderInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_fallbacks: Option<Vec<ProviderInfo>>,
    /// Realtime voice provider info (id + model). Serialised whenever
    /// the registry has one configured. Frontend's `VoiceCallButton`
    /// uses presence to gate the call affordance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_voice: Option<ProviderInfo>,
    /// Explicitly selectable profiles, including unavailable entries so the UI
    /// can explain a missing API key instead of silently removing a choice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub realtime_voice_profiles: Vec<RealtimeVoiceProfileInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_voice_default_profile: Option<String>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use async_trait::async_trait;
    use bytes::Bytes;
    use std::sync::Arc;

    use crate::magician_v2::media_seam::stt::{SttError, SttRequest, SttResponse};
    use crate::magician_v2::media_seam::tts::{TtsError, TtsProvider, TtsRequest, TtsResponse};
    use crate::magician_v2::media_seam::*;
    use crate::magician_v2::media_seam::{NoopDiarizationProvider, NoopVadProvider};

    struct FakeTtsProvider {
        id: &'static str,
        label: Option<&'static str>,
        default_model: &'static str,
        default_voice: Option<&'static str>,
        default_format: Option<&'static str>,
        voices: Vec<&'static str>,
        formats: Vec<&'static str>,
        streaming: bool,
    }

    struct FakeSttProvider {
        id: &'static str,
        default_model: &'static str,
    }

    #[async_trait]
    impl TtsProvider for FakeTtsProvider {
        fn id(&self) -> &str {
            self.id
        }

        fn label(&self) -> Option<&str> {
            self.label
        }

        fn default_voice(&self) -> Option<&str> {
            self.default_voice
        }

        fn default_model(&self) -> &str {
            self.default_model
        }

        fn default_format(&self) -> Option<&str> {
            self.default_format
        }

        fn supported_voices(&self) -> Vec<String> {
            if self.voices.is_empty() {
                self.default_voice.map(str::to_string).into_iter().collect()
            } else {
                self.voices
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect()
            }
        }

        fn supported_formats(&self) -> Vec<String> {
            if self.formats.is_empty() {
                self.default_format
                    .map(str::to_string)
                    .into_iter()
                    .collect()
            } else {
                self.formats
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect()
            }
        }

        fn supports_streaming(&self) -> bool {
            self.streaming
        }

        async fn synthesize(&self, _request: TtsRequest) -> Result<TtsResponse, TtsError> {
            Ok(TtsResponse {
                audio: Bytes::new(),
                content_type: "audio/mpeg".into(),
                model: self.default_model.into(),
                voice: self.default_voice.map(str::to_owned),
                message_id: None,
            })
        }
    }

    #[async_trait]
    impl SttProvider for FakeSttProvider {
        fn id(&self) -> &str {
            self.id
        }

        fn default_model(&self) -> &str {
            self.default_model
        }

        async fn transcribe(&self, _request: SttRequest) -> Result<SttResponse, SttError> {
            Ok(SttResponse {
                transcript: String::new(),
                model: self.default_model.to_string(),
                language: None,
                message_id: None,
                extras: None,
            })
        }
    }

    fn fake(
        id: &'static str,
        model: &'static str,
        voice: Option<&'static str>,
    ) -> Arc<FakeTtsProvider> {
        Arc::new(FakeTtsProvider {
            id,
            label: None,
            default_model: model,
            default_voice: voice,
            default_format: None,
            voices: Vec::new(),
            formats: Vec::new(),
            streaming: false,
        })
    }

    fn fake_labeled(
        id: &'static str,
        label: &'static str,
        model: &'static str,
        voice: Option<&'static str>,
    ) -> Arc<FakeTtsProvider> {
        Arc::new(FakeTtsProvider {
            id,
            label: Some(label),
            default_model: model,
            default_voice: voice,
            default_format: None,
            voices: Vec::new(),
            formats: Vec::new(),
            streaming: false,
        })
    }

    fn fake_with_capabilities() -> Arc<FakeTtsProvider> {
        Arc::new(FakeTtsProvider {
            id: "fluid-kokoro-en",
            label: Some("FluidAudio Kokoro"),
            default_model: "FluidInference/kokoro-82m-coreml",
            default_voice: Some("af_heart"),
            default_format: Some("wav"),
            voices: vec!["af_heart", "af_kore"],
            formats: vec!["wav"],
            streaming: false,
        })
    }

    fn fake_stt(id: &'static str, model: &'static str) -> Arc<FakeSttProvider> {
        Arc::new(FakeSttProvider {
            id,
            default_model: model,
        })
    }

    #[test]
    fn with_tts_sets_primary_and_preserves_fallbacks() {
        let registry = MediaProviderRegistry::new()
            .with_tts(fake("openai", "gpt-4o-mini-tts", Some("alloy")))
            .with_tts_fallback(fake(
                "minimax",
                "speech-02-hd",
                Some("female-yujie-jingpin"),
            ));
        let chain = registry.tts_chain();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id(), "openai");
        assert_eq!(chain[1].id(), "minimax");

        // Swap primary; fallback must stay in place.
        let registry = registry.with_tts(fake("kokoro", "kokoro", Some("af_bella")));
        let chain = registry.tts_chain();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id(), "kokoro");
        assert_eq!(chain[1].id(), "minimax");
    }

    #[test]
    fn with_tts_fallback_appends_in_order() {
        let registry = MediaProviderRegistry::new()
            .with_tts(fake("openai", "gpt-4o-mini-tts", None))
            .with_tts_fallback(fake("minimax", "speech-02-hd", None))
            .with_tts_fallback(fake("elevenlabs", "eleven_turbo_v2", None));
        let chain = registry.tts_chain();
        assert_eq!(
            chain.iter().map(|p| p.id().to_string()).collect::<Vec<_>>(),
            vec!["openai", "minimax", "elevenlabs"]
        );
    }

    #[test]
    fn tts_returns_primary_for_backcompat() {
        let registry = MediaProviderRegistry::new()
            .with_tts(fake("openai", "gpt-4o-mini-tts", Some("alloy")))
            .with_tts_fallback(fake("minimax", "speech-02-hd", None));
        let primary = registry.tts().expect("primary should exist");
        assert_eq!(primary.id(), "openai");
    }

    #[test]
    fn snapshot_separates_primary_from_fallbacks() {
        let registry = MediaProviderRegistry::new()
            .with_tts(fake("openai", "gpt-4o-mini-tts", Some("alloy")))
            .with_tts_fallback(fake(
                "minimax",
                "speech-02-hd",
                Some("female-yujie-jingpin"),
            ));
        let snap = registry.snapshot();
        let primary = snap.tts.expect("primary in snapshot");
        assert_eq!(primary.id, "openai");
        assert_eq!(primary.model, "gpt-4o-mini-tts");
        assert_eq!(primary.voice.as_deref(), Some("alloy"));

        let fallbacks = snap.tts_fallbacks.expect("fallbacks in snapshot");
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0].id, "minimax");
    }

    #[test]
    fn snapshot_omits_fallbacks_when_only_primary_registered() {
        let registry =
            MediaProviderRegistry::new().with_tts(fake("openai", "gpt-4o-mini-tts", Some("alloy")));
        let snap = registry.snapshot();
        assert!(snap.tts_fallbacks.is_none());
    }

    #[test]
    fn snapshot_advertises_selectable_realtime_profiles_and_default() {
        use magicllm::config::RealtimeVoiceMode;
        use magicllm::realtime::RealtimeAudioTopology;
        let profile = RealtimeVoiceProfileInfo {
            profile_id: "voice_realtime_openai_backend_mini".to_string(),
            label: "GPT Realtime 2.1 Mini".to_string(),
            provider: "openai_realtime_backend".to_string(),
            model: "gpt-realtime-2.1-mini".to_string(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: RealtimeVoiceMode::Assistant,
            turn_detection_mode: Some("server_vad".to_string()),
            transcription_model: Some("local".to_string()),
            transcription_fallback_model: Some("whisper-1".to_string()),
            available: true,
            unavailable_reason: None,
            translation_target_language: None,
            voice: None,
            voices: Vec::new(),
        };
        let snapshot = MediaProviderRegistry::new()
            .with_realtime_voice_profiles(Some("voice_realtime_default".to_string()), vec![profile])
            .snapshot();
        assert_eq!(
            snapshot.realtime_voice_default_profile.as_deref(),
            Some("voice_realtime_default")
        );
        assert_eq!(snapshot.realtime_voice_profiles.len(), 1);
        assert_eq!(
            snapshot.realtime_voice_profiles[0].profile_id,
            "voice_realtime_openai_backend_mini"
        );
        assert_eq!(
            snapshot.realtime_voice_profiles[0].topology,
            RealtimeAudioTopology::BackendProxied
        );
        assert_eq!(
            snapshot.realtime_voice_profiles[0]
                .turn_detection_mode
                .as_deref(),
            Some("server_vad")
        );
        assert_eq!(
            snapshot.realtime_voice_profiles[0]
                .transcription_model
                .as_deref(),
            Some("local")
        );
    }

    #[test]
    fn snapshot_surfaces_tts_labels_for_configured_providers() {
        let snap = MediaProviderRegistry::new()
            .with_tts(fake_labeled(
                "kokoro-local",
                "Kokoro Local",
                "kokoro",
                Some("af_bella"),
            ))
            .snapshot();
        let primary = snap.tts.expect("primary tts");
        assert_eq!(primary.id, "kokoro-local");
        assert_eq!(primary.label.as_deref(), Some("Kokoro Local"));
    }

    #[test]
    fn snapshot_surfaces_tts_voice_format_and_streaming_capabilities() {
        let snapshot = MediaProviderRegistry::new()
            .with_tts(fake_with_capabilities())
            .snapshot();
        let provider = snapshot.tts.expect("primary tts");
        assert_eq!(provider.format.as_deref(), Some("wav"));
        assert_eq!(provider.voices, ["af_heart", "af_kore"]);
        assert_eq!(provider.formats, ["wav"]);
        assert!(!provider.streaming);
    }

    #[test]
    fn tts_chain_for_honors_requested_provider_without_losing_fallbacks() {
        let registry = MediaProviderRegistry::new()
            .with_tts(fake("macos_tts", "av_speech_synthesizer", None))
            .with_tts_fallback(fake("openai", "gpt-4o-mini-tts", Some("alloy")))
            .with_tts_fallback(fake(
                "minimax",
                "speech-02-hd",
                Some("female-yujie-jingpin"),
            ));

        let default_chain = registry.tts_chain_for(None);
        assert_eq!(
            default_chain
                .iter()
                .map(|provider| provider.id().to_string())
                .collect::<Vec<_>>(),
            vec!["macos_tts", "openai", "minimax"]
        );

        let requested_chain = registry.tts_chain_for(Some(" openai "));
        assert_eq!(
            requested_chain
                .iter()
                .map(|provider| provider.id().to_string())
                .collect::<Vec<_>>(),
            vec!["openai", "macos_tts", "minimax"]
        );
    }

    #[test]
    fn tts_chain_for_auto_default_or_unknown_keeps_boot_order() {
        let registry = MediaProviderRegistry::new()
            .with_tts(fake("macos_tts", "av_speech_synthesizer", None))
            .with_tts_fallback(fake("openai", "gpt-4o-mini-tts", Some("alloy")));

        for preferred in [Some("auto"), Some("default"), Some("missing-provider")] {
            let chain = registry.tts_chain_for(preferred);
            assert_eq!(chain[0].id(), "macos_tts");
            assert_eq!(chain[1].id(), "openai");
        }
    }

    #[test]
    fn empty_registry_reports_no_chain() {
        let registry = MediaProviderRegistry::new();
        assert!(!registry.has_tts());
        assert!(registry.tts().is_none());
        assert!(registry.tts_chain().is_empty());
    }

    #[test]
    fn independent_stage_registries_preserve_primary_and_fallback_order() {
        let registry = MediaProviderRegistry::new()
            .with_vad(Arc::new(NoopVadProvider))
            .with_vad_fallback(Arc::new(NoopVadProvider))
            .with_diarization(Arc::new(NoopDiarizationProvider))
            .with_diarization_fallback(Arc::new(NoopDiarizationProvider));

        assert_eq!(registry.vad_chain().len(), 2);
        assert_eq!(registry.vad_chain()[0].id(), "noop-vad");
        assert_eq!(registry.diarization_chain().len(), 2);
        assert_eq!(registry.diarization_chain()[0].id(), "noop-diarization");
    }

    #[test]
    fn stt_chain_for_honors_requested_provider_without_losing_fallbacks() {
        let registry = MediaProviderRegistry::new()
            .with_stt(fake_stt("openai", "gpt-transcribe"))
            .with_stt_fallback(fake_stt("macos_speech", "macos_speech"))
            .with_stt_fallback(fake_stt("minimax", "speech-to-text"));

        let default_chain = registry.stt_chain_for(None);
        assert_eq!(
            default_chain
                .iter()
                .map(|provider| provider.id().to_string())
                .collect::<Vec<_>>(),
            vec!["openai", "macos_speech", "minimax"]
        );

        let requested_chain = registry.stt_chain_for(Some(" macos_speech "));
        assert_eq!(
            requested_chain
                .iter()
                .map(|provider| provider.id().to_string())
                .collect::<Vec<_>>(),
            vec!["macos_speech", "openai", "minimax"]
        );
    }

    #[test]
    fn stt_chain_for_auto_default_or_unknown_keeps_boot_order() {
        let registry = MediaProviderRegistry::new()
            .with_stt(fake_stt("openai", "gpt-transcribe"))
            .with_stt_fallback(fake_stt("macos_speech", "macos_speech"));

        for preferred in [Some("auto"), Some("default"), Some("missing-provider")] {
            let chain = registry.stt_chain_for(preferred);
            assert_eq!(chain[0].id(), "openai");
            assert_eq!(chain[1].id(), "macos_speech");
        }
    }
}
