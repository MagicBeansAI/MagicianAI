//! OpenAI text-to-speech adapter.
//!
//! Posts to `/v1/audio/speech` with `application/json` and reads the
//! returned audio bytes straight into a `bytes::Bytes` buffer. The
//! endpoint is non-streaming (the whole audio file is returned in one
//! response body), which matches the `TtsProvider` non-streaming
//! contract.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;

use crate::magician_v2::media_seam::tts::{
    TtsEmotion, TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse, TtsStyle, TtsVoiceMode,
};

pub const OPENAI_TTS_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/audio/speech";

/// Default TTS model. `gpt-4o-mini-tts` shipped 2025-03 and is the
/// current production successor to `tts-1` / `tts-1-hd`. It supports
/// the same `voice` + `response_format` shape, plus a new
/// `instructions` field for steerable delivery style (not wired yet).
///
/// Override via `MAGICIAN_TTS_MODEL`. Valid alternatives:
///   - `gpt-4o-mini-tts` (default, steerable, $0.60/M tokens)
///   - `tts-1` (legacy, low latency)
///   - `tts-1-hd` (legacy, higher quality)
pub const OPENAI_TTS_DEFAULT_MODEL: &str = "gpt-4o-mini-tts";

/// Default voice. Eleven voices are supported across all TTS models:
/// `alloy`, `ash`, `ballad`, `coral`, `echo`, `fable`, `nova`, `onyx`,
/// `sage`, `shimmer`, `verse`. Override via `MAGICIAN_TTS_VOICE`.
pub const OPENAI_TTS_DEFAULT_VOICE: &str = "alloy";

/// Default response format. Override via `MAGICIAN_TTS_FORMAT`.
/// Valid values: `mp3` (default), `opus`, `aac`, `flac`, `wav`, `pcm`.
pub const OPENAI_TTS_DEFAULT_FORMAT: &str = "mp3";

#[derive(Debug, Clone)]
pub struct OpenAiTtsProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
    default_voice: String,
    default_format: String,
}

impl OpenAiTtsProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, OPENAI_TTS_DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, base_url)
    }

    pub fn with_client(
        client: Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            base_url: base_url.into(),
            default_model: OPENAI_TTS_DEFAULT_MODEL.to_string(),
            default_voice: OPENAI_TTS_DEFAULT_VOICE.to_string(),
            default_format: OPENAI_TTS_DEFAULT_FORMAT.to_string(),
        }
    }

    pub fn with_defaults(
        mut self,
        model: impl Into<String>,
        voice: impl Into<String>,
        format: impl Into<String>,
    ) -> Self {
        self.default_model = model.into();
        self.default_voice = voice.into();
        self.default_format = format.into();
        self
    }
}

#[derive(Serialize)]
struct OpenAiTtsPayload<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
    /// `gpt-4o-mini-tts` accepts a free-form natural-language
    /// instruction string that steers tone, pacing, and persona.
    /// We compile our typed expression hints into a sentence the
    /// model can interpret. `tts-1` and `tts-1-hd` ignore this
    /// field (they pre-date instructions), so the same payload
    /// works against legacy models — they just don't shape output.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
}

/// Compile our provider-agnostic typed hints into the
/// natural-language `instructions` string OpenAI's
/// `gpt-4o-mini-tts` accepts. Returns `None` when no hints are
/// supplied — keeps the payload shape backward-compatible.
fn compile_openai_instructions(req: &TtsRequest) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(em) = req.emotion {
        let phrase = match em {
            TtsEmotion::Neutral => None,
            TtsEmotion::Happy => Some("Speak in a warm, upbeat tone."),
            TtsEmotion::Excited => Some("Speak with energy and excitement."),
            TtsEmotion::Concerned => Some("Speak with quiet concern."),
            TtsEmotion::Apologetic => Some("Speak in an apologetic, gentle tone."),
            TtsEmotion::Confident => Some("Speak with calm confidence."),
            TtsEmotion::Playful => Some("Speak in a playful, light-hearted tone."),
            TtsEmotion::Urgent => Some("Speak with urgency, without rushing."),
            TtsEmotion::Sad => Some("Speak with subdued, sad delivery."),
            TtsEmotion::Confused => Some("Speak as if mildly puzzled."),
        };
        if let Some(p) = phrase {
            parts.push(p.to_string());
        }
    }
    if let Some(style) = req.style {
        let phrase = match style {
            TtsStyle::Casual => "Use casual, conversational phrasing.",
            TtsStyle::Formal => "Use formal, professional phrasing.",
            TtsStyle::Dramatic => "Use a dramatic delivery with weighty pauses.",
            TtsStyle::Deadpan => "Use a deadpan, flat delivery.",
            TtsStyle::Warm => "Use a warm, friendly delivery.",
            TtsStyle::Clinical => "Use a measured, clinical delivery.",
        };
        parts.push(phrase.to_string());
    }
    match req.pace.unwrap_or_default() {
        TtsPace::Slow => parts.push("Speak deliberately, with measured pacing.".into()),
        TtsPace::Fast => parts.push("Speak briskly.".into()),
        TtsPace::Normal => {},
    }
    if let Some(mode) = req.voice_mode {
        let phrase = match mode {
            TtsVoiceMode::Default => None,
            TtsVoiceMode::Whisper => Some("Speak in a low, conspiratorial whisper."),
            TtsVoiceMode::Announcement => {
                Some("Speak with the projection and clarity of a public announcement.")
            },
        };
        if let Some(p) = phrase {
            parts.push(p.to_string());
        }
    }
    if let Some(emph) = req.emphasis.as_deref() {
        let trimmed = emph.trim();
        if !trimmed.is_empty() {
            // Quote inside the instruction so the model can recover
            // the exact phrase even if it contains punctuation. Short
            // emphasis strings only — long ones turn into noise.
            let snippet: String = trimmed.chars().take(80).collect();
            parts.push(format!("Lean weight onto the concept \"{snippet}\"."));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[async_trait]
impl TtsProvider for OpenAiTtsProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn default_voice(&self) -> Option<&str> {
        Some(&self.default_voice)
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn default_format(&self) -> Option<&str> {
        Some(&self.default_format)
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        if request.text.trim().is_empty() {
            return Err(TtsError::BadRequest("empty text".into()));
        }
        let model_owned = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let voice_owned = request
            .voice
            .clone()
            .unwrap_or_else(|| self.default_voice.clone());
        let format_owned = request
            .format
            .clone()
            .unwrap_or_else(|| self.default_format.clone());
        let instructions = compile_openai_instructions(&request);
        let payload = OpenAiTtsPayload {
            model: &model_owned,
            input: &request.text,
            voice: &voice_owned,
            response_format: &format_owned,
            speed: request.rate,
            instructions,
        };
        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(120))
            .json(&payload)
            .send()
            .await
            .map_err(|e| TtsError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(&format_mime(&format_owned))
            .to_string();
        let audio = response
            .bytes()
            .await
            .map_err(|e| TtsError::Transport(e.to_string()))?;
        Ok(TtsResponse {
            audio,
            content_type,
            model: model_owned,
            voice: Some(voice_owned),
            message_id: request.message_id,
        })
    }
}

fn format_mime(format: &str) -> String {
    match format.to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg".to_string(),
        "wav" => "audio/wav".to_string(),
        "flac" => "audio/flac".to_string(),
        "opus" => "audio/ogg".to_string(),
        "aac" => "audio/aac".to_string(),
        "pcm" => "audio/pcm".to_string(),
        other => format!("audio/{other}"),
    }
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build OpenAI TTS HTTP client")
}
