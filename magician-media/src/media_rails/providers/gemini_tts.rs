//! Gemini text-to-speech adapter.
//!
//! Gemini TTS is not OpenAI-compatible. It uses the Gemini
//! `models/{model}:generateContent` endpoint with `responseModalities: ["AUDIO"]`
//! and returns inline PCM audio. The media rails expect browser-playable bytes,
//! so this adapter wraps the default PCM output as WAV unless `format: pcm` is
//! explicitly requested.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, error, info};

use super::tts::{
    TtsEmotion, TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse, TtsStyle, TtsVoiceMode,
};

pub const GEMINI_TTS_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_TTS_PROVIDER_ID: &str = "gemini";
pub const GEMINI_TTS_DEFAULT_MODEL: &str = "gemini-3.1-flash-tts-preview";
pub const GEMINI_TTS_DEFAULT_VOICE: &str = "Kore";
pub const GEMINI_TTS_DEFAULT_FORMAT: &str = "wav";
const GEMINI_TTS_DEFAULT_SAMPLE_RATE: u32 = 24_000;
const GEMINI_TTS_DEFAULT_CHANNELS: u16 = 1;
const GEMINI_TTS_SAMPLE_WIDTH_BYTES: u16 = 2;

#[derive(Debug, Clone)]
pub struct GeminiTtsProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
    default_voice: String,
    default_format: String,
}

impl GeminiTtsProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, GEMINI_TTS_DEFAULT_BASE_URL)
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
            default_model: GEMINI_TTS_DEFAULT_MODEL.to_string(),
            default_voice: GEMINI_TTS_DEFAULT_VOICE.to_string(),
            default_format: GEMINI_TTS_DEFAULT_FORMAT.to_string(),
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

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    #[serde(default, rename = "inlineData", alias = "inline_data")]
    inline_data: Option<GeminiInlineData>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiInlineData {
    #[serde(default, rename = "mimeType", alias = "mime_type")]
    mime_type: Option<String>,
    data: String,
}

#[async_trait]
impl TtsProvider for GeminiTtsProvider {
    fn id(&self) -> &str {
        GEMINI_TTS_PROVIDER_ID
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
        let body = if uses_direct_voice(&model_owned) {
            direct_voice_body(&request, &voice_owned)
        } else {
            legacy_prebuilt_body(&compile_gemini_prompt(&request), &voice_owned)
        };

        let url = generate_content_url(&self.base_url, &model_owned);
        info!(
            "[TTS:gemini] synthesizing url={} model={} voice={} format={}",
            url, model_owned, voice_owned, format_owned
        );
        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .timeout(Duration::from_secs(120))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                error!("[TTS:gemini] transport failure: {e}");
                TtsError::Transport(e.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[TTS:gemini] upstream rejected status={} body={}",
                status, body
            );
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: GeminiGenerateContentResponse = response.json().await.map_err(|e| {
            error!("[TTS:gemini] response decode failure: {e}");
            TtsError::Transport(format!("decoding response: {e}"))
        })?;
        let (audio, source_mime) = collect_audio(&parsed)?;
        let source_mime = source_mime
            .unwrap_or_else(|| format!("audio/L16;rate={}", GEMINI_TTS_DEFAULT_SAMPLE_RATE));
        let sample_rate =
            sample_rate_from_mime(&source_mime).unwrap_or(GEMINI_TTS_DEFAULT_SAMPLE_RATE);
        let requested_format = format_owned.trim().to_ascii_lowercase();
        let source_is_wav = source_mime.to_ascii_lowercase().contains("wav");
        let (audio, content_type) = if requested_format == "pcm"
            || requested_format == "s16le"
            || requested_format == "raw"
        {
            (audio, format!("audio/pcm;rate={sample_rate}"))
        } else if source_is_wav {
            (audio, "audio/wav".to_string())
        } else {
            (
                pcm_s16le_to_wav(&audio, sample_rate, GEMINI_TTS_DEFAULT_CHANNELS),
                "audio/wav".to_string(),
            )
        };

        debug!(
            "[TTS:gemini] synthesized bytes={} content_type={} source_mime={}",
            audio.len(),
            content_type,
            source_mime
        );
        Ok(TtsResponse {
            audio,
            content_type,
            model: model_owned,
            voice: Some(voice_owned),
            message_id: request.message_id,
        })
    }
}

/// Gemini 3.8 TTS takes the voice on `voiceConfig.voice` and the transcript
/// verbatim. Earlier previews still use `prebuiltVoiceConfig.voiceName`.
fn uses_direct_voice(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("3.8") && model.contains("tts")
}

fn legacy_prebuilt_body(prompt: &str, voice: &str) -> serde_json::Value {
    json!({
        "contents": [{
            "parts": [{ "text": prompt }]
        }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice
                    }
                }
            }
        }
    })
}

fn direct_voice_body(request: &TtsRequest, voice: &str) -> serde_json::Value {
    let mut part = json!({ "text": request.text.trim() });
    if let Some(style) = speech_style(request) {
        part["speech_metadata"] = json!({ "style": style });
    }
    json!({
        "contents": [{
            "role": "user",
            "parts": [part]
        }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": { "voice": voice }
            }
        }
    })
}

fn speech_style(request: &TtsRequest) -> Option<String> {
    let mut notes: Vec<&str> = Vec::new();
    if let Some(emotion) = request.emotion {
        if let Some(note) = emotion_to_director_note(emotion) {
            notes.push(note);
        }
    }
    if let Some(style) = request.style {
        notes.push(style_to_director_note(style));
    }
    match request.pace.unwrap_or_default() {
        TtsPace::Slow => notes.push("Use a slow, deliberate pace."),
        TtsPace::Fast => notes.push("Use a brisk, energetic pace."),
        TtsPace::Normal => {},
    }
    if let Some(rate) = request.rate {
        if rate < 0.9 {
            notes.push("Use a slightly slower pace than normal.");
        } else if rate > 1.1 {
            notes.push("Use a slightly faster pace than normal.");
        }
    }
    if let Some(mode) = request.voice_mode {
        if let Some(note) = voice_mode_to_director_note(mode) {
            notes.push(note);
        }
    }
    let emphasis = request
        .emphasis
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if notes.is_empty() && emphasis.is_none() {
        return None;
    }
    let mut style = notes.join(" ");
    if let Some(phrase) = emphasis {
        let snippet: String = phrase.chars().take(80).collect();
        if !style.is_empty() {
            style.push(' ');
        }
        style.push_str("Emphasize this idea naturally: ");
        style.push_str(&snippet);
        style.push('.');
    }
    Some(style)
}

fn compile_gemini_prompt(request: &TtsRequest) -> String {
    let text = request.text.trim();
    let mut notes: Vec<&'static str> = Vec::new();
    if let Some(emotion) = request.emotion {
        if let Some(note) = emotion_to_director_note(emotion) {
            notes.push(note);
        }
    }
    if let Some(style) = request.style {
        notes.push(style_to_director_note(style));
    }
    match request.pace.unwrap_or_default() {
        TtsPace::Slow => notes.push("Use a slow, deliberate pace."),
        TtsPace::Fast => notes.push("Use a brisk, energetic pace."),
        TtsPace::Normal => {},
    }
    if let Some(rate) = request.rate {
        if rate < 0.9 {
            notes.push("Use a slightly slower pace than normal.");
        } else if rate > 1.1 {
            notes.push("Use a slightly faster pace than normal.");
        }
    }
    if let Some(mode) = request.voice_mode {
        if let Some(note) = voice_mode_to_director_note(mode) {
            notes.push(note);
        }
    }

    let emphasis = request
        .emphasis
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let snippet: String = value.chars().take(80).collect();
            format!("Emphasize this idea naturally: {snippet}.")
        });

    if notes.is_empty() && emphasis.is_none() {
        return text.to_string();
    }

    let mut prompt = String::from(
        "Synthesize speech for the transcript below. Do not read these instructions aloud.\n",
    );
    prompt.push_str("### DIRECTOR'S NOTES\n");
    for note in notes {
        prompt.push_str("- ");
        prompt.push_str(note);
        prompt.push('\n');
    }
    if let Some(note) = emphasis {
        prompt.push_str("- ");
        prompt.push_str(&note);
        prompt.push('\n');
    }
    prompt.push_str("### TRANSCRIPT\n");
    prompt.push_str(text);
    prompt
}

fn emotion_to_director_note(emotion: TtsEmotion) -> Option<&'static str> {
    match emotion {
        TtsEmotion::Neutral => None,
        TtsEmotion::Happy => Some("Use a warm, upbeat tone."),
        TtsEmotion::Excited => Some("Sound energetic and excited."),
        TtsEmotion::Concerned => Some("Sound quietly concerned and careful."),
        TtsEmotion::Apologetic => Some("Use an apologetic, gentle tone."),
        TtsEmotion::Confident => Some("Sound calm and confident."),
        TtsEmotion::Playful => Some("Use a playful, light-hearted delivery."),
        TtsEmotion::Urgent => Some("Sound urgent without rushing."),
        TtsEmotion::Sad => Some("Use a subdued, sad delivery."),
        TtsEmotion::Confused => Some("Sound mildly puzzled."),
    }
}

fn style_to_director_note(style: TtsStyle) -> &'static str {
    match style {
        TtsStyle::Casual => "Use casual, conversational phrasing.",
        TtsStyle::Formal => "Use formal, professional phrasing.",
        TtsStyle::Dramatic => "Use a dramatic delivery with weighty pauses.",
        TtsStyle::Deadpan => "Use a deadpan, flat delivery.",
        TtsStyle::Warm => "Use a warm, friendly delivery.",
        TtsStyle::Clinical => "Use a measured, clinical delivery.",
    }
}

fn voice_mode_to_director_note(mode: TtsVoiceMode) -> Option<&'static str> {
    match mode {
        TtsVoiceMode::Default => None,
        TtsVoiceMode::Whisper => Some("Use a low whisper where appropriate."),
        TtsVoiceMode::Announcement => Some("Use clear public-announcement projection."),
    }
}

fn collect_audio(
    response: &GeminiGenerateContentResponse,
) -> Result<(Bytes, Option<String>), TtsError> {
    let inline = response
        .candidates
        .iter()
        .filter_map(|candidate| candidate.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .find_map(|part| part.inline_data.as_ref())
        .ok_or_else(|| {
            let text_parts = response
                .candidates
                .iter()
                .filter_map(|candidate| candidate.content.as_ref())
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| part.text.as_deref())
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            TtsError::Upstream {
                status: 502,
                body: if text_parts.is_empty() {
                    "gemini response missing inline audio data".to_string()
                } else {
                    format!("gemini returned text instead of audio: {text_parts}")
                },
            }
        })?;
    let bytes = STANDARD
        .decode(&inline.data)
        .map_err(|error| TtsError::Upstream {
            status: 502,
            body: format!("gemini inline audio base64 decode failed: {error}"),
        })?;
    Ok((Bytes::from(bytes), inline.mime_type.clone()))
}

fn generate_content_url(base_url: &str, model: &str) -> String {
    let model = model.trim().strip_prefix("models/").unwrap_or(model.trim());
    format!(
        "{}/models/{}:generateContent",
        base_url.trim_end_matches('/'),
        model
    )
}

fn sample_rate_from_mime(mime: &str) -> Option<u32> {
    mime.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("rate=")
            .or_else(|| part.strip_prefix("sample_rate="))
            .and_then(|value| value.parse::<u32>().ok())
    })
}

fn pcm_s16le_to_wav(pcm: &Bytes, sample_rate: u32, channels: u16) -> Bytes {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * channels as u32 * GEMINI_TTS_SAMPLE_WIDTH_BYTES as u32;
    let block_align = channels * GEMINI_TTS_SAMPLE_WIDTH_BYTES;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(GEMINI_TTS_SAMPLE_WIDTH_BYTES * 8).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    Bytes::from(wav)
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build Gemini TTS HTTP client")
}

#[cfg(test)]
mod tests {
    use wiremock::{
        matchers::{body_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::super::tts::TtsStyle;
    use super::*;

    #[test]
    fn provider_metadata_is_configurable() {
        let provider = GeminiTtsProvider::with_base_url("test-key", "https://example.invalid")
            .with_defaults("gemini-test", "Puck", "pcm");

        assert_eq!(provider.id(), "gemini");
        assert_eq!(provider.default_model(), "gemini-test");
        assert_eq!(provider.default_voice(), Some("Puck"));
    }

    #[tokio::test]
    async fn synthesize_sends_generate_content_audio_config_and_wraps_pcm_as_wav() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-test:generateContent"))
            .and(body_json(json!({
                "contents": [{
                    "parts": [{ "text": "hello" }]
                }],
                "generationConfig": {
                    "responseModalities": ["AUDIO"],
                    "speechConfig": {
                        "voiceConfig": {
                            "prebuiltVoiceConfig": {
                                "voiceName": "Kore"
                            }
                        }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {
                        "parts": [{
                            "inlineData": {
                                "mimeType": "audio/L16;rate=24000",
                                "data": "AQIDBA=="
                            }
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = GeminiTtsProvider::with_base_url("test-key", server.uri()).with_defaults(
            "gemini-test",
            "Kore",
            "wav",
        );
        let response = provider
            .synthesize(TtsRequest {
                text: "hello".to_string(),
                voice: None,
                rate: None,
                model: None,
                format: None,
                message_id: Some("msg_1".to_string()),
                emotion: None,
                style: None,
                pace: None,
                voice_mode: None,
                emphasis: None,
            })
            .await
            .expect("synthesis should parse");

        assert_eq!(response.content_type, "audio/wav");
        assert_eq!(response.model, "gemini-test");
        assert_eq!(response.voice.as_deref(), Some("Kore"));
        assert_eq!(response.message_id.as_deref(), Some("msg_1"));
        assert_eq!(&response.audio[..4], b"RIFF");
        assert_eq!(&response.audio[8..12], b"WAVE");
        assert_eq!(&response.audio[44..], &[1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn gemini_38_sends_the_transcript_verbatim_and_the_direct_voice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash-lite-tts:generateContent"))
            .and(body_json(json!({
                "contents": [{
                    "role": "user",
                    "parts": [{
                        "text": "hello",
                        "speech_metadata": { "style": "Use a warm, friendly delivery." }
                    }]
                }],
                "generationConfig": {
                    "responseModalities": ["AUDIO"],
                    "speechConfig": {
                        "voiceConfig": { "voice": "Kore" }
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "inlineData": {
                                "mimeType": "audio/wav",
                                "data": "UklGRg=="
                            }
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = GeminiTtsProvider::with_base_url("test-key", server.uri()).with_defaults(
            "gemini-3.8-flash-lite-tts",
            "Kore",
            "wav",
        );
        let response = provider
            .synthesize(TtsRequest {
                text: "hello".to_string(),
                voice: None,
                rate: None,
                model: None,
                format: None,
                message_id: None,
                emotion: None,
                style: Some(TtsStyle::Warm),
                pace: None,
                voice_mode: None,
                emphasis: None,
            })
            .await
            .expect("3.8 synthesis should parse");
        assert_eq!(response.model, "gemini-3.8-flash-lite-tts");
        assert_eq!(response.content_type, "audio/wav");
    }

    #[tokio::test]
    async fn synthesize_can_return_pcm_when_requested() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-test:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "inlineData": {
                                "mimeType": "audio/L16;rate=16000",
                                "data": "AQIDBA=="
                            }
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = GeminiTtsProvider::with_base_url("test-key", server.uri()).with_defaults(
            "gemini-test",
            "Kore",
            "pcm",
        );
        let response = provider
            .synthesize(TtsRequest {
                text: "hello".to_string(),
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
            })
            .await
            .expect("synthesis should parse");

        assert_eq!(response.content_type, "audio/pcm;rate=16000");
        assert_eq!(&response.audio[..], &[1, 2, 3, 4]);
    }
}
