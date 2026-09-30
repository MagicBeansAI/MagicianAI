//! Grok file speech-to-text adapter for Dictation.
//!
//! `POST /v1/stt` takes multipart form data and returns JSON `{ "text": ... }`.
//! The audio file must be the last form field. This is the completed-clip path,
//! not the live streaming socket.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{multipart, Client};
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info};

use super::stt::{SttError, SttProvider, SttRequest, SttResponse};

pub const GROK_STT_DEFAULT_BASE_URL: &str = "https://api.x.ai";
pub const GROK_STT_PROVIDER_ID: &str = "grok-transcribe";
pub const GROK_STT_DEFAULT_MODEL: &str = "grok-voice-transcribe-2.0";
const GROK_STT_MAX_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct GrokSttProvider {
    client: Client,
    api_key: String,
    base_url: String,
    provider_id: String,
    label: Option<String>,
    default_model: String,
}

impl GrokSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, GROK_STT_DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("failed to build Grok STT HTTP client"),
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: GROK_STT_PROVIDER_ID.to_string(),
            label: None,
            default_model: GROK_STT_DEFAULT_MODEL.to_string(),
        }
    }

    pub fn with_provider_id(mut self, id: impl Into<String>) -> Self {
        self.provider_id = id.into();
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }
}

#[async_trait]
impl SttProvider for GrokSttProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        if request.audio.is_empty() {
            return Err(SttError::BadRequest("empty audio".into()));
        }
        if request.audio.len() > GROK_STT_MAX_BYTES {
            return Err(SttError::BadRequest(format!(
                "audio is {} bytes, above the Grok transcribe limit of {GROK_STT_MAX_BYTES} bytes",
                request.audio.len()
            )));
        }

        let model = request
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| self.default_model.clone());
        let filename = request
            .filename
            .clone()
            .unwrap_or_else(|| filename_for(&request.content_type));
        let mime = request.content_type.trim();
        let mime = if mime.is_empty() { "application/octet-stream" } else { mime };
        let audio_part = multipart::Part::bytes(request.audio.to_vec())
            .file_name(filename.clone())
            .mime_str(mime)
            .map_err(|error| SttError::BadRequest(format!("invalid content type: {error}")))?;

        // Text fields first. Grok requires the file part to be last.
        let mut form = multipart::Form::new().text("model", model.clone());
        if let Some(language) = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"))
        {
            form = form.text("language", language.to_string()).text("format", "true");
        }
        if let Some(prompt) = request
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let keyterm: String = prompt.chars().take(200).collect();
            form = form.text("keyterm", keyterm);
        }
        form = form.part("file", audio_part);

        let url = stt_endpoint(&self.base_url);
        info!(
            "[STT:grok] transcribing url={} model={} bytes={} content_type={} filename={}",
            url,
            model,
            request.audio.len(),
            mime,
            filename
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(180))
            .multipart(form)
            .send()
            .await
            .map_err(|error| {
                error!("[STT:grok] transport failure: {error}");
                SttError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!("[STT:grok] upstream rejected status={} body={}", status, body);
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: GrokSttResponse = response.json().await.map_err(|error| {
            error!("[STT:grok] response decode failure: {error}");
            SttError::Transport(format!("decoding response: {error}"))
        })?;
        let extras = parsed.duration.map(|duration| json!({ "duration_seconds": duration }));
        Ok(SttResponse {
            transcript: parsed.text,
            model,
            language: request.language.clone(),
            message_id: request.message_id,
            extras,
        })
    }
}

#[derive(Debug, Deserialize)]
struct GrokSttResponse {
    text: String,
    #[serde(default)]
    duration: Option<f64>,
}

fn stt_endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1/stt") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/stt")
    } else {
        format!("{base}/v1/stt")
    }
}

fn filename_for(content_type: &str) -> String {
    let ext = match content_type.to_ascii_lowercase().as_str() {
        value if value.contains("wav") => "wav",
        value if value.contains("mpeg") || value.contains("mp3") => "mp3",
        value if value.contains("ogg") || value.contains("opus") => "ogg",
        value if value.contains("flac") => "flac",
        value if value.contains("mp4") || value.contains("m4a") => "m4a",
        value if value.contains("webm") => "webm",
        _ => "wav",
    };
    format!("audio.{ext}")
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn request(audio: &'static [u8]) -> SttRequest {
        SttRequest {
            audio: Bytes::from_static(audio),
            content_type: "audio/wav".into(),
            language: Some("en".into()),
            model: None,
            message_id: None,
            filename: None,
            prompt: None,
        }
    }

    #[tokio::test]
    async fn transcribe_posts_multipart_and_reads_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/stt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "hello from magician",
                "duration": 1.25
            })))
            .mount(&server)
            .await;

        let provider = GrokSttProvider::with_base_url("test-key", server.uri())
            .with_provider_id("grok-transcribe")
            .with_label("Grok Transcribe");
        let response = provider.transcribe(request(b"RIFF-fake")).await.unwrap();
        assert_eq!(response.transcript, "hello from magician");
        assert_eq!(response.model, GROK_STT_DEFAULT_MODEL);
        assert_eq!(
            response.extras,
            Some(serde_json::json!({ "duration_seconds": 1.25 }))
        );
    }

    #[tokio::test]
    async fn empty_audio_is_rejected_before_the_request() {
        let provider = GrokSttProvider::with_base_url("test-key", "https://example.invalid");
        let error = provider.transcribe(request(b"")).await.unwrap_err();
        assert!(matches!(error, SttError::BadRequest(_)));
    }

    #[tokio::test]
    async fn upstream_rejection_is_an_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/stt"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = GrokSttProvider::with_base_url("test-key", server.uri());
        let error = provider.transcribe(request(b"RIFF")).await.unwrap_err();
        assert!(matches!(error, SttError::Upstream { status: 401, .. }));
    }

    #[tokio::test]
    #[ignore = "calls api.x.ai; set XAI_API_KEY and GROK_STT_LIVE_WAV"]
    async fn live_transcribe_spoken_wav() {
        let key = std::env::var("XAI_API_KEY").expect("XAI_API_KEY");
        let path = std::env::var("GROK_STT_LIVE_WAV").expect("GROK_STT_LIVE_WAV");
        let audio = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
        let provider = GrokSttProvider::new(key);
        let response = provider
            .transcribe(SttRequest {
                audio: Bytes::from(audio),
                content_type: "audio/wav".into(),
                language: Some("en".into()),
                model: None,
                message_id: None,
                filename: Some("hello.wav".into()),
                prompt: None,
            })
            .await
            .expect("live transcribe");
        assert!(
            response.transcript.to_ascii_lowercase().contains("magician"),
            "transcript was {:?}",
            response.transcript
        );
    }

    #[test]
    fn endpoint_accepts_a_host_or_a_full_stt_url() {
        assert_eq!(stt_endpoint("https://api.x.ai"), "https://api.x.ai/v1/stt");
        assert_eq!(stt_endpoint("https://api.x.ai/v1/"), "https://api.x.ai/v1/stt");
        assert_eq!(
            stt_endpoint("https://example.invalid/v1/stt"),
            "https://example.invalid/v1/stt"
        );
    }
}
