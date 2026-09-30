//! The media seam — lib-side vocabulary and services consumed by chat,
//! execution, and artifact_v2 from the former `magician_v2::media_rails`.
//! The heavy media engines (voice orchestrator, streaming STT providers,
//! fluid audio, runtime config) live in the magician-media crate.

pub mod audio_surface;
pub mod binding;
pub mod browser_join;
pub mod dev_server;
pub mod dev_server_manager;
pub mod diarized_streaming_stt;
pub mod host_automation;
pub mod meeting_audio;
pub mod meeting_bridge_linux;
pub mod meeting_bridge_macos;
pub mod meeting_calendar;
pub mod meeting_capture_reservation;
pub mod meeting_control_audit;
pub mod meeting_llm_tts_responder;
pub mod meeting_magician_agent_responder;
pub mod meeting_manager;
pub mod meeting_markers;
pub mod meeting_memory;
pub mod meeting_orchestrator_voice_responder;
pub mod meeting_passive;
pub mod meeting_pushed;
pub mod meeting_realtime_responder;
pub mod meeting_responder;
pub mod meeting_session;
pub mod meeting_session_engine;
pub mod meeting_summarizer;
pub mod meeting_transcript_sink;
pub mod openai_streaming_stt;
pub mod openai_tts_provider;
pub mod preferences;
pub mod providers_cached_tts;
pub mod providers_configured_tts;
pub mod providers_diarization;
pub mod providers_registry;
pub mod providers_stt;
pub mod providers_vad;
pub mod runtime_config;
pub mod runtime_validation;
pub mod speech_segments;
pub mod streaming_fallback;
pub mod streaming_stt_types;
pub mod tts_types;
pub mod vad_gate;
pub mod voice_fanout;

/// Historical `media_rails::providers` name — the provider type layer.
pub mod providers {
    pub use crate::magician_v2::media_seam::openai_streaming_stt::*;
    pub use crate::magician_v2::media_seam::openai_tts_provider as openai_tts;
    pub use crate::magician_v2::media_seam::providers_cached_tts::*;
    pub use crate::magician_v2::media_seam::providers_configured_tts::*;
    pub use crate::magician_v2::media_seam::providers_diarization::*;
    pub use crate::magician_v2::media_seam::providers_stt::*;
    pub use crate::magician_v2::media_seam::providers_vad::*;
    pub use crate::magician_v2::media_seam::streaming_stt_types::*;
    pub use crate::magician_v2::media_seam::tts_types::*;
    pub use crate::magician_v2::media_seam::*;
}

/// System agent id used by media-originated runtime events.
pub const MEDIA_SYSTEM_AGENT: &str = "__media__";

/// Adapter id of the crate-side fluid-audio TTS engine.
pub const FLUID_AUDIO_TTS_ADAPTER: &str = "fluid_audio_kokoro_tts";

/// Historical `media_rails::meeting` path — the whole meeting engine.
pub mod meeting {
    pub use crate::magician_v2::media_seam::binding::*;
    pub use crate::magician_v2::media_seam::browser_join::*;
    pub use crate::magician_v2::media_seam::meeting_audio as audio;
    pub use crate::magician_v2::media_seam::meeting_audio::*;
    pub use crate::magician_v2::media_seam::meeting_bridge_linux as bridge_linux;
    pub use crate::magician_v2::media_seam::meeting_bridge_linux::*;
    pub use crate::magician_v2::media_seam::meeting_bridge_macos as bridge_macos;
    pub use crate::magician_v2::media_seam::meeting_bridge_macos::*;
    pub use crate::magician_v2::media_seam::meeting_calendar as calendar;
    pub use crate::magician_v2::media_seam::meeting_calendar::*;
    pub use crate::magician_v2::media_seam::meeting_capture_reservation as capture_reservation;
    pub use crate::magician_v2::media_seam::meeting_capture_reservation::*;
    pub use crate::magician_v2::media_seam::meeting_control_audit as control_audit;
    pub use crate::magician_v2::media_seam::meeting_control_audit::*;
    pub use crate::magician_v2::media_seam::meeting_llm_tts_responder as llm_tts_responder;
    pub use crate::magician_v2::media_seam::meeting_llm_tts_responder::*;
    pub use crate::magician_v2::media_seam::meeting_magician_agent_responder as magician_agent_responder;
    pub use crate::magician_v2::media_seam::meeting_magician_agent_responder::*;
    pub use crate::magician_v2::media_seam::meeting_manager::*;
    pub use crate::magician_v2::media_seam::meeting_markers as markers;
    pub use crate::magician_v2::media_seam::meeting_markers::*;
    pub use crate::magician_v2::media_seam::meeting_memory as memory;
    pub use crate::magician_v2::media_seam::meeting_memory::*;
    pub use crate::magician_v2::media_seam::meeting_orchestrator_voice_responder as orchestrator_voice_responder;
    pub use crate::magician_v2::media_seam::meeting_orchestrator_voice_responder::*;
    pub use crate::magician_v2::media_seam::meeting_passive as passive;
    pub use crate::magician_v2::media_seam::meeting_passive::*;
    pub use crate::magician_v2::media_seam::meeting_pushed as pushed;
    pub use crate::magician_v2::media_seam::meeting_pushed::*;
    pub use crate::magician_v2::media_seam::meeting_realtime_responder as realtime_responder;
    pub use crate::magician_v2::media_seam::meeting_realtime_responder::*;
    pub use crate::magician_v2::media_seam::meeting_responder as responder;
    pub use crate::magician_v2::media_seam::meeting_responder::*;
    pub use crate::magician_v2::media_seam::meeting_session::*;
    pub use crate::magician_v2::media_seam::meeting_session_engine as session;
    pub use crate::magician_v2::media_seam::meeting_session_engine::*;
    pub use crate::magician_v2::media_seam::meeting_summarizer as summarizer;
    pub use crate::magician_v2::media_seam::meeting_summarizer::*;
    pub use crate::magician_v2::media_seam::meeting_transcript_sink as transcript_sink;
    pub use crate::magician_v2::media_seam::meeting_transcript_sink::*;
}

// Historical sibling-name aliases so old media_rails paths resolve.
pub use meeting_audio as audio;
pub use meeting_markers as markers;
pub use meeting_pushed as pushed;
pub use meeting_responder as responder;
pub use meeting_summarizer as summarizer;
pub use meeting_transcript_sink as transcript_sink;
pub use providers_diarization as diarization;
pub use providers_stt as stt;
pub use tts_types as tts;

pub use audio_surface::*;
pub use binding::*;
pub use browser_join::*;
pub use dev_server::*;
pub use dev_server_manager::*;
pub use diarized_streaming_stt::*;
pub use host_automation::*;
pub use meeting_audio::*;
pub use meeting_bridge_linux::*;
pub use meeting_bridge_macos::*;
pub use meeting_llm_tts_responder::*;
pub use meeting_magician_agent_responder::*;
pub use meeting_manager::*;
pub use meeting_markers::*;
pub use meeting_memory::*;
pub use meeting_orchestrator_voice_responder::*;
pub use meeting_passive::*;
pub use meeting_pushed::*;
pub use meeting_realtime_responder::*;
pub use meeting_responder::*;
pub use meeting_session::*;
pub use meeting_session_engine::*;
pub use meeting_summarizer::*;
pub use meeting_transcript_sink::*;
pub use openai_streaming_stt::*;
pub use openai_tts_provider::*;
pub use preferences::*;
pub use providers_cached_tts::*;
pub use providers_configured_tts::*;
pub use providers_diarization::*;
pub use providers_registry::*;
pub use providers_stt::*;
pub use providers_vad::*;
pub use runtime_config::*;
pub use runtime_validation::*;
pub use speech_segments::*;
pub use streaming_fallback::*;
pub use streaming_stt_types::*;
pub use tts_types::*;
pub use vad_gate::*;
pub use voice_fanout::*;
