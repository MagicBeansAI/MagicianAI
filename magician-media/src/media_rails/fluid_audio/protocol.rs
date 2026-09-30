use serde::{Deserialize, Serialize};

use super::super::StreamAudioFormat;

pub const FLUID_AUDIO_PROTOCOL_VERSION: u32 = 1;
pub const FLUID_AUDIO_VAD_ADAPTER: &str = "fluid_audio_vad";
pub const FLUID_AUDIO_RECORDING_STT_ADAPTER: &str = "fluid_audio_recording_stt";
pub const FLUID_AUDIO_STREAMING_STT_ADAPTER: &str = "fluid_audio_streaming_eou_stt";
pub const FLUID_AUDIO_DIARIZATION_ADAPTER: &str = "fluid_audio_streaming_sortformer";
pub use magician::magician_v2::media_seam::FLUID_AUDIO_TTS_ADAPTER;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluidAudioModelDefinition {
    pub id: String,
    pub adapter: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub idle_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FluidAudioSidecarConfig {
    pub protocol_version: u32,
    pub model_cache_dir: String,
    pub download_policy: String,
    pub registry_url: Option<String>,
    pub offline: bool,
    pub process_idle_secs: u64,
    pub max_resident_models: usize,
    pub max_streaming_sessions: usize,
    pub max_request_bytes: usize,
    pub max_frame_bytes: usize,
    pub prewarm: Vec<String>,
    pub models: Vec<FluidAudioModelDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarHealth {
    pub status: String,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarModelState {
    pub id: String,
    pub state: String,
    #[serde(default)]
    pub resident: bool,
    #[serde(default)]
    pub active_sessions: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarTranscriptionResponse {
    pub transcript: String,
    pub model_id: String,
    pub model: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    pub audio_duration_ms: u64,
    pub processing_duration_ms: u64,
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct SidecarSpeechRequest {
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    pub response_format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

#[derive(Debug)]
pub struct SidecarSpeechResponse {
    pub audio: bytes::Bytes,
    pub model_id: String,
    pub voice: String,
    pub format: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamClientControl {
    Start {
        protocol_version: u32,
        stage: &'static str,
        model_id: String,
        format: StreamAudioFormat,
        config: serde_json::Value,
    },
    Stop,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamServerEvent {
    Ready,
    Probability {
        value: f32,
        at_ms: u64,
    },
    SpeechStarted {
        at_ms: u64,
    },
    SpeechEnded {
        at_ms: u64,
    },
    TranscriptPartial {
        text: String,
        #[serde(rename = "turn_id")]
        _turn_id: String,
        #[serde(rename = "start_ms")]
        _start_ms: u64,
    },
    TranscriptFinal {
        text: String,
        #[serde(rename = "turn_id")]
        _turn_id: String,
        #[serde(default)]
        language: Option<String>,
        start_ms: u64,
    },
    SpeakerStarted {
        speaker_id: String,
        at_ms: u64,
    },
    SpeakerEnded {
        speaker_id: String,
        at_ms: u64,
    },
    SegmentRevised {
        speaker_id: String,
        start_ms: u64,
        end_ms: u64,
        #[serde(default)]
        confidence: Option<f32>,
    },
    Finished,
    Error {
        code: String,
        message: String,
    },
}
