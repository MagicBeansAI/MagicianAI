//! Grok text-to-speech adapter.
//!
//! `POST /v1/tts` returns raw audio bytes. Language is sent as `auto` so a
//! reply is not forced into English. Pace and rate become the API speed
//! multiplier (0.7–1.5). Whisper and announcement wrap the transcript in
//! Grok speech tags; other expression hints are left to the text itself so
//! director notes are not read aloud.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, info};

use super::tts::{TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse, TtsVoiceMode};

pub const GROK_TTS_DEFAULT_BASE_URL: &str = "https://api.x.ai";
pub const GROK_TTS_PROVIDER_ID: &str = "grok";
pub const GROK_TTS_DEFAULT_MODEL: &str = "grok-tts";
pub const GROK_TTS_DEFAULT_VOICE: &str = "eve";
pub const GROK_TTS_DEFAULT_FORMAT: &str = "mp3";
pub const GROK_TTS_DEFAULT_LANGUAGE: &str = "auto";
const GROK_TTS_MAX_CHARS: usize = 60_000;
const GROK_TTS_VOICES: &[&str] = &["eve", "ara", "leo", "rex", "sal"];
const GROK_TTS_FORMATS: &[&str] = &["mp3", "wav", "pcm"];

#[derive(Debug, Clone)]
pub struct GrokTtsProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
    default_voice: String,
    default_format: String,
    voices: Vec<String>,
    formats: Vec<String>,
}

impl GrokTtsProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, GROK_TTS_DEFAULT_BASE_URL)
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
            default_model: GROK_TTS_DEFAULT_MODEL.to_string(),
            default_voice: GROK_TTS_DEFAULT_VOICE.to_string(),
            default_format: GROK_TTS_DEFAULT_FORMAT.to_string(),
            voices: GROK_TTS_VOICES
                .iter()
                .map(|voice| (*voice).to_string())
                .collect(),
            formats: GROK_TTS_FORMATS
                .iter()
                .map(|format| (*format).to_string())
                .collect(),
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

    pub fn with_voices(mut self, voices: Vec<String>) -> Self {
        let voices = voices
            .into_iter()
            .map(|voice| voice.trim().to_string())
            .filter(|voice| !voice.is_empty())
            .collect::<Vec<_>>();
        if !voices.is_empty() {
            self.voices = voices;
        }
        self
    }

    pub fn with_formats(mut self, formats: Vec<String>) -> Self {
        let formats = formats
            .into_iter()
            .map(|format| format.trim().to_string())
            .filter(|format| !format.is_empty())
            .collect::<Vec<_>>();
        if !formats.is_empty() {
            self.formats = formats;
        }
        self
    }
}

#[async_trait]
impl TtsProvider for GrokTtsProvider {
    fn id(&self) -> &str {
        GROK_TTS_PROVIDER_ID
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

    fn supported_voices(&self) -> Vec<String> {
        self.voices.clone()
    }

    fn supported_formats(&self) -> Vec<String> {
        self.formats.clone()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        let text = request.text.trim();
        if text.is_empty() {
            return Err(TtsError::BadRequest("empty text".into()));
        }
        if text.chars().count() > GROK_TTS_MAX_CHARS {
            return Err(TtsError::BadRequest(format!(
                "text exceeds {GROK_TTS_MAX_CHARS} characters"
            )));
        }

        let model_owned = request
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| self.default_model.clone());
        let voice_owned = request
            .voice
            .clone()
            .filter(|voice| !voice.trim().is_empty())
            .unwrap_or_else(|| self.default_voice.clone());
        let format_owned = request
            .format
            .clone()
            .filter(|format| !format.trim().is_empty())
            .unwrap_or_else(|| self.default_format.clone());
        let spoken = apply_delivery(text, request.voice_mode, request.emphasis.as_deref());
        let body = request_body(&spoken, &voice_owned, &format_owned, &request);

        let url = tts_endpoint(&self.base_url);
        info!(
            "[TTS:grok] synthesizing url={} model={} voice={} format={}",
            url, model_owned, voice_owned, format_owned
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(120))
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                error!("[TTS:grok] transport failure: {error}");
                TtsError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[TTS:grok] upstream rejected status={} body={}",
                status, body
            );
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_string())
            .filter(|value| value.starts_with("audio/"))
            .unwrap_or_else(|| content_type_for_format(&format_owned));
        let audio = response.bytes().await.map_err(|error| {
            error!("[TTS:grok] body read failure: {error}");
            TtsError::Transport(format!("reading audio: {error}"))
        })?;
        if audio.is_empty() {
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body: "empty audio".into(),
            });
        }
        Ok(TtsResponse {
            audio,
            content_type,
            model: model_owned,
            voice: Some(voice_owned),
            message_id: request.message_id,
        })
    }
}

fn request_body(text: &str, voice: &str, format: &str, request: &TtsRequest) -> Value {
    let mut body = json!({
        "text": text,
        "voice_id": voice,
        "language": GROK_TTS_DEFAULT_LANGUAGE,
        "speed": speed_for(request),
    });
    if let Some(output_format) = output_format(format) {
        body["output_format"] = output_format;
    }
    body
}

fn output_format(format: &str) -> Option<Value> {
    let codec = match format.trim().to_ascii_lowercase().as_str() {
        "mp3" | "mpeg" | "audio/mpeg" => "mp3",
        "wav" | "audio/wav" => "wav",
        "pcm" | "s16le" | "raw" | "audio/pcm" => "pcm",
        "mulaw" | "audio/basic" => "mulaw",
        "alaw" | "audio/alaw" => "alaw",
        _ => return None,
    };
    Some(json!({
        "codec": codec,
        "sample_rate": 24000,
    }))
}

fn content_type_for_format(format: &str) -> String {
    match format.trim().to_ascii_lowercase().as_str() {
        "wav" | "audio/wav" => "audio/wav",
        "pcm" | "s16le" | "raw" | "audio/pcm" => "audio/pcm",
        "mulaw" | "audio/basic" => "audio/basic",
        "alaw" | "audio/alaw" => "audio/alaw",
        _ => "audio/mpeg",
    }
    .to_string()
}

fn speed_for(request: &TtsRequest) -> f32 {
    let speed = request
        .rate
        .unwrap_or(match request.pace.unwrap_or_default() {
            TtsPace::Slow => 0.85,
            TtsPace::Normal => 1.0,
            TtsPace::Fast => 1.25,
        });
    speed.clamp(0.7, 1.5)
}

fn apply_delivery(text: &str, mode: Option<TtsVoiceMode>, emphasis: Option<&str>) -> String {
    let mut spoken = text.to_string();
    if let Some(phrase) = emphasis.map(str::trim).filter(|phrase| {
        !phrase.is_empty() && !phrase.contains(['<', '>']) && spoken.contains(phrase)
    }) {
        let wrapped = format!("<emphasis>{phrase}</emphasis>");
        spoken = spoken.replacen(phrase, &wrapped, 1);
    }
    match mode.unwrap_or_default() {
        TtsVoiceMode::Whisper if !spoken.contains("<whisper>") => {
            format!("<whisper>{spoken}</whisper>")
        },
        TtsVoiceMode::Announcement if !spoken.contains("<loud>") => {
            format!("<loud>{spoken}</loud>")
        },
        _ => spoken,
    }
}

fn tts_endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1/tts") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/tts")
    } else {
        format!("{base}/v1/tts")
    }
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build Grok TTS HTTP client")
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn request(text: &str) -> TtsRequest {
        TtsRequest {
            text: text.to_string(),
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
        }
    }

    #[test]
    fn provider_metadata_advertises_builtin_voices() {
        let provider = GrokTtsProvider::with_base_url("test-key", "https://example.invalid")
            .with_defaults("grok-tts", "ara", "wav");
        assert_eq!(provider.id(), "grok");
        assert_eq!(provider.default_model(), "grok-tts");
        assert_eq!(provider.default_voice(), Some("ara"));
        assert_eq!(provider.default_format(), Some("wav"));
        assert_eq!(
            provider.supported_voices(),
            vec!["eve", "ara", "leo", "rex", "sal"]
        );
    }

    #[test]
    fn configured_voices_replace_the_builtin_list() {
        let provider =
            GrokTtsProvider::new("test-key").with_voices(vec!["eve".into(), " custom ".into()]);
        assert_eq!(provider.supported_voices(), vec!["eve", "custom"]);
    }

    #[tokio::test]
    async fn synthesize_posts_tts_json_and_returns_mp3_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tts"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(json!({
                "text": "hello",
                "voice_id": "eve",
                "language": "auto",
                "speed": 1.0,
                "output_format": { "codec": "mp3", "sample_rate": 24000 }
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/mpeg")
                    .set_body_bytes(b"mp3-bytes".to_vec()),
            )
            .mount(&server)
            .await;

        let provider = GrokTtsProvider::with_base_url("test-key", server.uri());
        let response = provider.synthesize(request("hello")).await.unwrap();
        assert_eq!(response.audio, Bytes::from_static(b"mp3-bytes"));
        assert_eq!(response.content_type, "audio/mpeg");
        assert_eq!(response.model, "grok-tts");
        assert_eq!(response.voice.as_deref(), Some("eve"));
    }

    #[tokio::test]
    async fn whisper_and_fast_pace_change_the_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tts"))
            .and(body_json(json!({
                "text": "<whisper><emphasis>secret</emphasis> note</whisper>",
                "voice_id": "leo",
                "language": "auto",
                "speed": 1.25,
                "output_format": { "codec": "mp3", "sample_rate": 24000 }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .mount(&server)
            .await;

        let provider = GrokTtsProvider::with_base_url("test-key", server.uri());
        let mut request = request("secret note");
        request.voice = Some("leo".into());
        request.pace = Some(TtsPace::Fast);
        request.voice_mode = Some(TtsVoiceMode::Whisper);
        request.emphasis = Some("secret".into());
        let response = provider.synthesize(request).await.unwrap();
        assert_eq!(response.audio, Bytes::from_static(b"ok"));
        assert_eq!(response.content_type, "audio/mpeg");
    }

    #[tokio::test]
    async fn upstream_rejection_is_an_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/tts"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = GrokTtsProvider::with_base_url("test-key", server.uri());
        let error = provider.synthesize(request("hello")).await.unwrap_err();
        assert!(matches!(error, TtsError::Upstream { status: 401, .. }));
    }

    #[tokio::test]
    async fn empty_text_is_rejected_before_the_request() {
        let provider = GrokTtsProvider::with_base_url("test-key", "https://example.invalid");
        let error = provider.synthesize(request("  ")).await.unwrap_err();
        assert!(matches!(error, TtsError::BadRequest(_)));
    }

    #[test]
    fn endpoint_accepts_a_host_or_a_full_tts_url() {
        assert_eq!(tts_endpoint("https://api.x.ai"), "https://api.x.ai/v1/tts");
        assert_eq!(
            tts_endpoint("https://api.x.ai/v1/"),
            "https://api.x.ai/v1/tts"
        );
        assert_eq!(
            tts_endpoint("https://example.invalid/v1/tts"),
            "https://example.invalid/v1/tts"
        );
    }
}
