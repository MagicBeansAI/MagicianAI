//! MiniMax text-to-speech adapter.
//!
//! Posts to `/v1/t2a_v2` against either `api.minimaxi.chat` (the public
//! global endpoint) or a private base-URL override. MiniMax uses a
//! `group_id` query parameter alongside the bearer key to scope the
//! request to a billing group — we plumb that through the
//! `MAGICIAN_MINIMAX_GROUP_ID` env var at boot.
//!
//! The MiniMax API differs from the OpenAI shape in three ways the
//! adapter has to handle:
//!
//! 1. **Response format** — MiniMax returns JSON whose `data.audio`
//!    field is hex-encoded audio bytes (not the raw audio body OpenAI
//!    returns). We decode hex on the way out.
//! 2. **Emotion is a typed enum** with values
//!    `happy | sad | angry | fearful | disgusted | surprised | neutral`
//!    rather than free-form. Our `TtsEmotion` set maps via a small
//!    lossy table for values MiniMax doesn't carry verbatim.
//! 3. **Pace + voice mode** flow through separate fields:
//!    `voice_setting.speed` (numeric multiplier) and (for whisper /
//!    announcement modes) a different `voice_id`. We default to the
//!    same base voice when no mode override is requested.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use super::tts::{
    TtsEmotion, TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse, TtsStyle, TtsVoiceMode,
};

pub const MINIMAX_TTS_DEFAULT_BASE_URL: &str = "https://api.minimaxi.chat/v1/t2a_v2";
pub const MINIMAX_TTS_DEFAULT_MODEL: &str = "speech-02-hd";
pub const MINIMAX_TTS_DEFAULT_VOICE: &str = "female-yujie-jingpin";
pub const MINIMAX_TTS_DEFAULT_FORMAT: &str = "mp3";

#[derive(Debug, Clone)]
pub struct MiniMaxTtsProvider {
    client: Client,
    api_key: String,
    group_id: String,
    base_url: String,
    default_model: String,
    default_voice: String,
    default_format: String,
}

impl MiniMaxTtsProvider {
    pub fn new(api_key: impl Into<String>, group_id: impl Into<String>) -> Self {
        Self::with_client(
            default_http_client(),
            api_key,
            group_id,
            MINIMAX_TTS_DEFAULT_BASE_URL,
        )
    }

    pub fn with_base_url(
        api_key: impl Into<String>,
        group_id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self::with_client(default_http_client(), api_key, group_id, base_url)
    }

    pub fn with_client(
        client: Client,
        api_key: impl Into<String>,
        group_id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            group_id: group_id.into(),
            base_url: base_url.into(),
            default_model: MINIMAX_TTS_DEFAULT_MODEL.to_string(),
            default_voice: MINIMAX_TTS_DEFAULT_VOICE.to_string(),
            default_format: MINIMAX_TTS_DEFAULT_FORMAT.to_string(),
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

// ─── Request shape ───────────────────────────────────────────────────

#[derive(Serialize)]
struct MiniMaxRequest<'a> {
    model: &'a str,
    text: &'a str,
    stream: bool,
    voice_setting: MiniMaxVoiceSetting<'a>,
    audio_setting: MiniMaxAudioSetting<'a>,
}

#[derive(Serialize)]
struct MiniMaxVoiceSetting<'a> {
    voice_id: &'a str,
    /// Speech speed multiplier (0.5 .. 2.0). 1.0 is normal.
    speed: f32,
    /// MiniMax native emotion enum: `happy | sad | angry | fearful |
    /// disgusted | surprised | neutral`. We skip the field when our
    /// hint maps to "let provider decide" so MiniMax's own model
    /// chooses (rather than us forcing `neutral` and overriding the
    /// model's natural inflection).
    #[serde(skip_serializing_if = "Option::is_none")]
    emotion: Option<&'static str>,
    /// Volume (0.0 .. 10.0). 1.0 is normal. Whisper mode dips this.
    #[serde(skip_serializing_if = "Option::is_none")]
    vol: Option<f32>,
}

#[derive(Serialize)]
struct MiniMaxAudioSetting<'a> {
    sample_rate: u32,
    bitrate: u32,
    format: &'a str,
    channel: u8,
}

// ─── Response shape ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct MiniMaxResponse {
    data: Option<MiniMaxResponseData>,
    base_resp: Option<MiniMaxBaseResp>,
}

#[derive(Deserialize)]
struct MiniMaxResponseData {
    /// Hex-encoded audio bytes. MiniMax doesn't return the raw body
    /// on the JSON path; we decode here.
    audio: String,
}

#[derive(Deserialize)]
struct MiniMaxBaseResp {
    status_code: i32,
    #[serde(default)]
    status_msg: Option<String>,
}

#[async_trait]
impl TtsProvider for MiniMaxTtsProvider {
    fn id(&self) -> &str {
        "minimax"
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
        let voice_owned = resolve_voice_id(&request, &self.default_voice);
        let format_owned = request
            .format
            .clone()
            .unwrap_or_else(|| self.default_format.clone());

        let speed = pace_to_speed_multiplier(request.pace, request.rate);
        let vol = voice_mode_volume(request.voice_mode);
        let emotion = emotion_to_minimax(request.emotion, request.style);

        let payload = MiniMaxRequest {
            model: &model_owned,
            text: &request.text,
            stream: false,
            voice_setting: MiniMaxVoiceSetting {
                voice_id: &voice_owned,
                speed,
                emotion,
                vol,
            },
            audio_setting: MiniMaxAudioSetting {
                sample_rate: 32000,
                bitrate: 128000,
                format: &format_owned,
                channel: 1,
            },
        };

        info!(
            "[TTS:minimax] synthesizing url={} model={} voice={} emotion={:?} speed={:.2}",
            self.base_url, model_owned, voice_owned, emotion, speed
        );

        let response = self
            .client
            .post(&self.base_url)
            .query(&[("GroupId", self.group_id.as_str())])
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(120))
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                error!("[TTS:minimax] transport failure: {e}");
                TtsError::Transport(e.to_string())
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[TTS:minimax] upstream rejected status={} body={}",
                status, body
            );
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: MiniMaxResponse = response.json().await.map_err(|e| {
            error!("[TTS:minimax] response decode failure: {e}");
            TtsError::Transport(format!("decoding response: {e}"))
        })?;

        // MiniMax embeds a per-request status object even on HTTP 200
        // — empty quota, model-not-allowed etc. land here as
        // status_code != 0. Treat those as Upstream errors with the
        // provider's message so the fallback chain rotates correctly.
        if let Some(base) = &parsed.base_resp {
            if base.status_code != 0 {
                let msg = base
                    .status_msg
                    .clone()
                    .unwrap_or_else(|| "minimax non-zero status_code".to_string());
                error!(
                    "[TTS:minimax] non-zero status_code={} msg={}",
                    base.status_code, msg
                );
                return Err(TtsError::Upstream {
                    status: 502,
                    body: format!("minimax status_code={}: {}", base.status_code, msg),
                });
            }
        }

        let audio_hex = parsed
            .data
            .map(|d| d.audio)
            .ok_or_else(|| TtsError::Upstream {
                status: 502,
                body: "minimax response missing data.audio".to_string(),
            })?;
        let audio_bytes = decode_hex(&audio_hex).map_err(|err| {
            error!("[TTS:minimax] hex decode failure: {err}");
            TtsError::Upstream {
                status: 502,
                body: format!("minimax hex decode failed: {err}"),
            }
        })?;
        debug!(
            "[TTS:minimax] synthesized bytes={} format={}",
            audio_bytes.len(),
            format_owned
        );

        Ok(TtsResponse {
            audio: bytes::Bytes::from(audio_bytes),
            content_type: format_mime(&format_owned),
            model: model_owned,
            voice: Some(voice_owned),
            message_id: request.message_id,
        })
    }
}

// ─── Hint → MiniMax field translation ────────────────────────────────

fn pace_to_speed_multiplier(pace: Option<TtsPace>, rate_override: Option<f32>) -> f32 {
    // Explicit numeric rate wins — used by power users / tests.
    if let Some(rate) = rate_override {
        return rate.clamp(0.5, 2.0);
    }
    match pace.unwrap_or_default() {
        TtsPace::Slow => 0.85,
        TtsPace::Normal => 1.0,
        TtsPace::Fast => 1.2,
    }
}

fn voice_mode_volume(mode: Option<TtsVoiceMode>) -> Option<f32> {
    match mode {
        Some(TtsVoiceMode::Whisper) => Some(0.6),
        Some(TtsVoiceMode::Announcement) => Some(1.4),
        _ => None,
    }
}

/// Map our typed `TtsEmotion` (10 values) to MiniMax's native 7-value
/// enum. We're lossy on purpose: `excited → happy`, `playful →
/// happy`, etc. — these read "close enough" in practice.
///
/// `style` is treated as a refinement on emotion: certain styles
/// nudge the emotion (e.g. `dramatic` lands as `surprised` when the
/// base emotion is neutral) so the styled hint isn't silently
/// discarded on MiniMax's side.
fn emotion_to_minimax(
    emotion: Option<TtsEmotion>,
    style: Option<TtsStyle>,
) -> Option<&'static str> {
    let base = match emotion? {
        TtsEmotion::Neutral => "neutral",
        TtsEmotion::Happy | TtsEmotion::Playful | TtsEmotion::Excited => "happy",
        TtsEmotion::Sad | TtsEmotion::Apologetic | TtsEmotion::Concerned => "sad",
        TtsEmotion::Confident => "neutral",
        TtsEmotion::Urgent | TtsEmotion::Confused => "surprised",
    };
    // Stylistic nudge — only kicks in when the base emotion is
    // neutral so we don't override a clear emotional signal.
    let final_emotion = match (base, style) {
        ("neutral", Some(TtsStyle::Dramatic)) => "surprised",
        ("neutral", Some(TtsStyle::Warm)) => "happy",
        _ => base,
    };
    Some(final_emotion)
}

fn resolve_voice_id(request: &TtsRequest, default_voice: &str) -> String {
    // Explicit override wins.
    if let Some(v) = request.voice.as_deref() {
        if !v.trim().is_empty() {
            return v.to_string();
        }
    }
    // Voice-mode override picks a MiniMax voice id tuned for the
    // mode. MiniMax's stock catalog includes whisper variants — we
    // pick a sensible default; operators can override per call.
    match request.voice_mode {
        Some(TtsVoiceMode::Whisper) => "female-tianmei-jingpin".to_string(),
        Some(TtsVoiceMode::Announcement) => "male-qn-jingying-jingpin".to_string(),
        _ => default_voice.to_string(),
    }
}

fn format_mime(format: &str) -> String {
    match format.to_ascii_lowercase().as_str() {
        "mp3" => "audio/mpeg".to_string(),
        "wav" => "audio/wav".to_string(),
        "flac" => "audio/flac".to_string(),
        "pcm" => "audio/pcm".to_string(),
        other => format!("audio/{other}"),
    }
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    if !input.len().is_multiple_of(2) {
        return Err("hex string has odd length".to_string());
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    for chunk in input.as_bytes().chunks(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(format!("invalid hex byte: {other:#x}")),
    }
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build MiniMax TTS HTTP client")
}
