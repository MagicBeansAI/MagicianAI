//! Host macOS text-to-speech adapter.
//!
//! This provider calls the trusted Tauri host gateway, which in turn invokes the
//! Swift `magician-macos-speech-helper synthesize` command backed by Apple's
//! AVFoundation speech synthesis APIs. Magician never shells arbitrary text into
//! `say`; it sends a structured JSON request to the loopback host gateway and
//! receives audio bytes back through the normal [`TtsProvider`] contract.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use super::tts::{TtsError, TtsProvider, TtsRequest, TtsResponse};
pub use magician::magician_v2::media_seam::{MACOS_TTS_DEFAULT_MODEL, MACOS_TTS_PROVIDER_ID};

pub const MACOS_TTS_DEFAULT_FORMAT: &str = "wav";
pub const MACOS_TTS_DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:3017";
const MACOS_TTS_GATEWAY_TIMEOUT: Duration = Duration::from_secs(70);

#[derive(Debug, Clone)]
pub struct MacOsTtsProvider {
    client: Client,
    gateway_url: String,
    default_model: String,
    default_format: String,
}

impl MacOsTtsProvider {
    pub fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            client: default_http_client(),
            gateway_url: gateway_url.into(),
            default_model: MACOS_TTS_DEFAULT_MODEL.to_string(),
            default_format: MACOS_TTS_DEFAULT_FORMAT.to_string(),
        }
    }

    pub fn with_client(client: Client, gateway_url: impl Into<String>) -> Self {
        Self {
            client,
            gateway_url: gateway_url.into(),
            default_model: MACOS_TTS_DEFAULT_MODEL.to_string(),
            default_format: MACOS_TTS_DEFAULT_FORMAT.to_string(),
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/host/speech/synthesize",
            self.gateway_url.trim_end_matches('/')
        )
    }
}

#[derive(Debug, Serialize)]
struct HostSpeechSynthesizeRequest {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HostSpeechSynthesizeResponse {
    audio_b64: String,
    content_type: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
}

#[async_trait]
impl TtsProvider for MacOsTtsProvider {
    fn id(&self) -> &str {
        MACOS_TTS_PROVIDER_ID
    }

    fn default_voice(&self) -> Option<&str> {
        None
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
        let endpoint = self.endpoint();
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let format = request
            .format
            .clone()
            .unwrap_or_else(|| self.default_format.clone());
        let payload = HostSpeechSynthesizeRequest {
            text: request.text.clone(),
            voice: request.voice.clone(),
            rate: request.rate,
            model: Some(model.clone()),
            format: Some(format),
            message_id: request.message_id.clone(),
        };
        info!(
            "[TTS:macos] synthesizing via host gateway url={} text_len={} voice={:?} rate={:?}",
            endpoint,
            request.text.len(),
            request.voice,
            request.rate
        );
        let response = self
            .client
            .post(endpoint)
            .timeout(MACOS_TTS_GATEWAY_TIMEOUT)
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                error!("[TTS:macos] transport failure: {error}");
                TtsError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[TTS:macos] host gateway rejected status={} body={}",
                status, body
            );
            return Err(TtsError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: HostSpeechSynthesizeResponse = response.json().await.map_err(|error| {
            error!("[TTS:macos] response decode failure: {error}");
            TtsError::Transport(format!("decoding host gateway response: {error}"))
        })?;
        let audio = STANDARD
            .decode(parsed.audio_b64.as_bytes())
            .map_err(|error| TtsError::Transport(format!("decoding host audio: {error}")))?;
        if audio.is_empty() {
            return Err(TtsError::Upstream {
                status: 502,
                body: "macOS TTS returned empty audio".to_string(),
            });
        }
        debug!(
            "[TTS:macos] synthesized bytes={} content_type={} voice={:?}",
            audio.len(),
            parsed.content_type,
            parsed.voice
        );
        Ok(TtsResponse {
            audio: bytes::Bytes::from(audio),
            content_type: parsed.content_type,
            model: parsed.model.unwrap_or(model),
            voice: parsed.voice.or(request.voice),
            message_id: parsed.message_id.or(request.message_id),
        })
    }
}

fn default_http_client() -> Client {
    Client::builder()
        .user_agent("magician-media-rails/0.1 macos-tts")
        .build()
        .expect("reqwest client")
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    fn request(text: &str) -> TtsRequest {
        TtsRequest {
            text: text.to_string(),
            voice: Some("com.apple.voice.compact.en-US.Samantha".to_string()),
            rate: Some(1.1),
            model: None,
            format: None,
            message_id: Some("msg-1".to_string()),
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }
    }

    #[test]
    fn endpoint_trims_gateway_trailing_slashes() {
        let provider = MacOsTtsProvider::new("http://127.0.0.1:3017///");
        assert_eq!(
            provider.endpoint(),
            "http://127.0.0.1:3017/host/speech/synthesize"
        );
    }

    #[tokio::test]
    async fn rejects_empty_text_before_calling_gateway() {
        let server = MockServer::start().await;
        let provider = MacOsTtsProvider::new(server.uri());

        let err = provider
            .synthesize(TtsRequest {
                text: "   ".to_string(),
                ..request("ignored")
            })
            .await
            .expect_err("empty text should fail locally");

        assert!(matches!(err, TtsError::BadRequest(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn posts_text_to_host_gateway_and_decodes_audio() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/speech/synthesize"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "audio_b64": STANDARD.encode(b"wav-bytes"),
                "content_type": "audio/wav",
                "model": "av_speech_synthesizer",
                "voice": "com.apple.voice.compact.en-US.Samantha",
                "message_id": "msg-1"
            })))
            .mount(&server)
            .await;

        let provider = MacOsTtsProvider::new(format!("{}/", server.uri()));
        let response = provider
            .synthesize(request("hello from macOS"))
            .await
            .expect("host gateway response should parse");

        assert_eq!(response.audio, bytes::Bytes::from_static(b"wav-bytes"));
        assert_eq!(response.content_type, "audio/wav");
        assert_eq!(response.model, "av_speech_synthesizer");
        assert_eq!(
            response.voice.as_deref(),
            Some("com.apple.voice.compact.en-US.Samantha")
        );
        assert_eq!(response.message_id.as_deref(), Some("msg-1"));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["text"], "hello from macOS");
        assert_eq!(body["voice"], "com.apple.voice.compact.en-US.Samantha");
        assert_eq!(body["rate"], json!(1.1));
        assert_eq!(body["model"], MACOS_TTS_DEFAULT_MODEL);
        assert_eq!(body["format"], MACOS_TTS_DEFAULT_FORMAT);
        assert_eq!(body["message_id"], "msg-1");
    }
}
