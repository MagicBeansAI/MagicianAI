use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AudioSurface {
    #[default]
    Dictation,
    Meeting,
    Listening,
    HandsFree,
}

impl AudioSurface {
    pub const ALL: [Self; 4] = [
        Self::Dictation,
        Self::Meeting,
        Self::Listening,
        Self::HandsFree,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dictation => "dictation",
            Self::Meeting => "meeting",
            Self::Listening => "listening",
            Self::HandsFree => "hands_free",
        }
    }

    pub const fn supports_stage(self, stage: AudioStage) -> bool {
        match self {
            Self::Dictation => matches!(
                stage,
                AudioStage::Vad | AudioStage::RecordingStt | AudioStage::Tts
            ),
            Self::Meeting => matches!(
                stage,
                AudioStage::Vad
                    | AudioStage::StreamingStt
                    | AudioStage::Diarization
                    | AudioStage::Tts
            ),
            Self::Listening | Self::HandsFree => matches!(
                stage,
                AudioStage::Vad
                    | AudioStage::StreamingStt
                    | AudioStage::Diarization
                    | AudioStage::Tts
            ),
        }
    }
}

impl fmt::Display for AudioSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AudioSurface {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "dictation" | "recording" => Ok(Self::Dictation),
            "meeting" => Ok(Self::Meeting),
            "listening" | "listen" | "observe" => Ok(Self::Listening),
            "hands_free" | "handsfree" => Ok(Self::HandsFree),
            other => Err(format!("unknown audio surface: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioStage {
    Vad,
    RecordingStt,
    StreamingStt,
    Diarization,
    Tts,
}

impl AudioStage {
    pub const ALL: [Self; 5] = [
        Self::Vad,
        Self::RecordingStt,
        Self::StreamingStt,
        Self::Diarization,
        Self::Tts,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vad => "vad",
            Self::RecordingStt => "recording_stt",
            Self::StreamingStt => "streaming_stt",
            Self::Diarization => "diarization",
            Self::Tts => "tts",
        }
    }
}

impl fmt::Display for AudioStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AudioStage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "vad" => Ok(Self::Vad),
            "recording_stt" | "stt" => Ok(Self::RecordingStt),
            "streaming_stt" => Ok(Self::StreamingStt),
            "diarization" => Ok(Self::Diarization),
            "tts" => Ok(Self::Tts),
            other => Err(format!("unknown audio stage: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TurnBoundaryAuthority {
    #[default]
    PushToTalk,
    Vad,
    SttEou,
    ProviderServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VadMode {
    GateOnly,
    TurnAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioEngineStartupPolicy {
    Disabled,
    External,
    #[default]
    Lazy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioModelDownloadPolicy {
    #[default]
    Disabled,
    OnDemand,
    Prewarm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioEngineConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_macos_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token_env: Option<String>,
    #[serde(default)]
    pub startup: AudioEngineStartupPolicy,
    #[serde(default)]
    pub download_policy: AudioModelDownloadPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_cache_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_url: Option<String>,
    #[serde(default)]
    pub offline: bool,
    #[serde(default = "default_audio_engine_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_audio_engine_health_timeout_secs")]
    pub health_timeout_secs: u64,
    #[serde(default = "default_audio_engine_idle_process_secs")]
    pub idle_process_secs: u64,
    #[serde(default = "default_audio_engine_model_idle_secs")]
    pub default_model_idle_secs: u64,
    #[serde(default = "default_audio_engine_max_resident_models")]
    pub max_resident_models: usize,
    #[serde(default = "default_audio_engine_max_streaming_sessions")]
    pub max_streaming_sessions: usize,
    #[serde(default = "default_audio_engine_max_restart_attempts")]
    pub max_restart_attempts: usize,
    #[serde(default = "default_audio_engine_max_request_bytes")]
    pub max_request_bytes: usize,
    #[serde(default = "default_audio_engine_max_frame_bytes")]
    pub max_frame_bytes: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prewarm: Vec<String>,
}

impl Default for AudioEngineConfig {
    fn default() -> Self {
        Self {
            label: None,
            enabled: true,
            minimum_macos_version: None,
            endpoint: None,
            auth_token_env: None,
            startup: AudioEngineStartupPolicy::Lazy,
            download_policy: AudioModelDownloadPolicy::Disabled,
            model_cache_dir: None,
            registry_url: None,
            offline: false,
            request_timeout_secs: default_audio_engine_request_timeout_secs(),
            health_timeout_secs: default_audio_engine_health_timeout_secs(),
            idle_process_secs: default_audio_engine_idle_process_secs(),
            default_model_idle_secs: default_audio_engine_model_idle_secs(),
            max_resident_models: default_audio_engine_max_resident_models(),
            max_streaming_sessions: default_audio_engine_max_streaming_sessions(),
            max_restart_attempts: default_audio_engine_max_restart_attempts(),
            max_request_bytes: default_audio_engine_max_request_bytes(),
            max_frame_bytes: default_audio_engine_max_frame_bytes(),
            prewarm: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProviderBindingConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub engine_id: String,
    pub adapter: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub capabilities: BTreeSet<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_audio_engine_request_timeout_secs() -> u64 {
    120
}

fn default_audio_engine_health_timeout_secs() -> u64 {
    3
}

fn default_audio_engine_idle_process_secs() -> u64 {
    900
}

fn default_audio_engine_model_idle_secs() -> u64 {
    300
}

fn default_audio_engine_max_resident_models() -> usize {
    1
}

fn default_audio_engine_max_streaming_sessions() -> usize {
    2
}

fn default_audio_engine_max_restart_attempts() -> usize {
    2
}

fn default_audio_engine_max_request_bytes() -> usize {
    25 * 1024 * 1024
}

fn default_audio_engine_max_frame_bytes() -> usize {
    1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AudioProviderCatalogConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<AudioProviderBindingConfig>,
}

impl AudioProviderCatalogConfig {
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl AudioSurfaceProfilesConfig {
    pub fn is_empty(&self) -> bool {
        self.default_mapping.is_empty() && self.profiles.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AudioStageProfileConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<VadMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_speech_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silence_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_roll_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hangover_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_utterance_ms: Option<u64>,
}

impl AudioStageProfileConfig {
    pub fn enabled_with(providers: Vec<String>) -> Self {
        Self {
            enabled: !providers.is_empty(),
            providers,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSurfaceProfileConfig {
    pub surface: AudioSurface,
    #[serde(default)]
    pub turn_boundary: TurnBoundaryAuthority,
    #[serde(default)]
    pub vad: AudioStageProfileConfig,
    #[serde(default)]
    pub recording_stt: AudioStageProfileConfig,
    #[serde(default)]
    pub streaming_stt: AudioStageProfileConfig,
    #[serde(default)]
    pub diarization: AudioStageProfileConfig,
    #[serde(default)]
    pub tts: AudioStageProfileConfig,
}

impl AudioSurfaceProfileConfig {
    pub fn stage(&self, stage: AudioStage) -> &AudioStageProfileConfig {
        match stage {
            AudioStage::Vad => &self.vad,
            AudioStage::RecordingStt => &self.recording_stt,
            AudioStage::StreamingStt => &self.streaming_stt,
            AudioStage::Diarization => &self.diarization,
            AudioStage::Tts => &self.tts,
        }
    }

    pub fn stage_mut(&mut self, stage: AudioStage) -> &mut AudioStageProfileConfig {
        match stage {
            AudioStage::Vad => &mut self.vad,
            AudioStage::RecordingStt => &mut self.recording_stt,
            AudioStage::StreamingStt => &mut self.streaming_stt,
            AudioStage::Diarization => &mut self.diarization,
            AudioStage::Tts => &mut self.tts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AudioSurfaceProfilesConfig {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_mapping: BTreeMap<AudioSurface, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, AudioSurfaceProfileConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAvailability {
    Available,
    Unavailable,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioStageCapabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub recording: bool,
    #[serde(default)]
    pub word_timestamps: bool,
    #[serde(default)]
    pub end_of_utterance: bool,
    #[serde(default)]
    pub speaker_attribution: bool,
    #[serde(default)]
    pub voice_cloning: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub features: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStageOption {
    pub option_id: String,
    pub stage: AudioStage,
    pub provider_id: String,
    pub engine_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub label: String,
    #[serde(default)]
    pub capabilities: AudioStageCapabilities,
    pub availability: ProviderAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioProfileSource {
    ExplicitRequest,
    ScopedPreference,
    ConfiguredDefault,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAudioStage {
    pub stage: AudioStage,
    pub enabled: bool,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<VadMode>,
    #[serde(default)]
    pub providers: Vec<AudioStageOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<AudioStageOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_speech_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_silence_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_roll_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hangover_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_utterance_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAudioProfile {
    pub surface: AudioSurface,
    pub profile_id: String,
    pub revision: String,
    pub source: AudioProfileSource,
    pub turn_boundary: TurnBoundaryAuthority,
    pub stages: BTreeMap<AudioStage, ResolvedAudioStage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degradations: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;

    #[test]
    fn surface_and_stage_wire_names_are_stable() {
        assert_eq!(
            serde_json::to_string(&AudioSurface::HandsFree).expect("surface"),
            "\"hands_free\""
        );
        assert_eq!(
            "observe".parse::<AudioSurface>().expect("legacy alias"),
            AudioSurface::Listening
        );
        assert_eq!(
            "streaming-stt".parse::<AudioStage>().expect("stage"),
            AudioStage::StreamingStt
        );
        assert_eq!(
            serde_json::to_string(&ProviderAvailability::Available).expect("availability"),
            "\"available\""
        );
    }

    #[test]
    fn surface_stage_matrix_distinguishes_recording_and_streaming_workflows() {
        assert!(AudioSurface::Dictation.supports_stage(AudioStage::RecordingStt));
        assert!(!AudioSurface::Dictation.supports_stage(AudioStage::StreamingStt));
        assert!(AudioSurface::Meeting.supports_stage(AudioStage::StreamingStt));
        assert!(!AudioSurface::Meeting.supports_stage(AudioStage::RecordingStt));
        assert!(AudioSurface::HandsFree.supports_stage(AudioStage::Tts));
    }
}
