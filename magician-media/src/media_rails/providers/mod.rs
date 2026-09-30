//! Concrete provider implementations over the seam type layer.

pub use magician::magician_v2::media_seam::providers::*;
pub use magician::magician_v2::media_seam::streaming_stt_types as streaming_stt;

pub mod gemini_live_transcribe_stt;
pub mod gemini_stt;
pub mod gemini_transcribe;
pub mod gemini_tts;
pub mod google_cloud_speech_stt;
pub mod grok_stt;
pub mod grok_tts;
pub mod macos_speech;
pub mod macos_speech_stt;
pub mod macos_tts;
pub mod minimax_tts;
pub mod openai_diarize_stt;
pub mod openai_streaming_stt;
pub mod openai_whisper;

pub use gemini_live_transcribe_stt::*;
pub use gemini_stt::*;
pub use gemini_transcribe::*;
pub use gemini_tts::*;
pub use google_cloud_speech_stt::*;
pub use grok_stt::*;
pub use grok_tts::*;
pub use macos_speech::*;
pub use macos_speech_stt::*;
pub use macos_tts::*;
pub use minimax_tts::*;
pub use openai_diarize_stt::*;
pub use openai_streaming_stt::*;
pub use openai_whisper::*;
