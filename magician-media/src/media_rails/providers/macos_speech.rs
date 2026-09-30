//! Host macOS Speech one-shot STT adapter.
//!
//! This provider is intentionally scoped to recorded audio. It calls the
//! host-native gateway (`MAGICIAN_HOST_GATEWAY_URL`) which runs Apple's
//! Speech framework on the user's Mac, then returns the final transcript
//! through the same provider contract as cloud STT.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use super::stt::{SttError, SttProvider, SttRequest, SttResponse};
pub use magician::magician_v2::media_seam::{MACOS_SPEECH_DEFAULT_MODEL, MACOS_SPEECH_PROVIDER_ID};

pub const MACOS_SPEECH_DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:3017";
const MACOS_SPEECH_GATEWAY_TIMEOUT: Duration = Duration::from_secs(70);

#[derive(Debug, Clone)]
pub struct MacOsSpeechProvider {
    client: Client,
    gateway_url: String,
    default_model: String,
}

impl MacOsSpeechProvider {
    pub fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            client: default_http_client(),
            gateway_url: gateway_url.into(),
            default_model: MACOS_SPEECH_DEFAULT_MODEL.to_string(),
        }
    }

    pub fn with_client(client: Client, gateway_url: impl Into<String>) -> Self {
        Self {
            client,
            gateway_url: gateway_url.into(),
            default_model: MACOS_SPEECH_DEFAULT_MODEL.to_string(),
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "{}/host/speech/transcribe",
            self.gateway_url.trim_end_matches('/')
        )
    }
}

#[derive(Debug, Serialize)]
struct HostSpeechTranscribeRequest {
    audio_b64: String,
    content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HostSpeechTranscribeResponse {
    transcript: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    extras: Option<serde_json::Value>,
}

#[async_trait]
impl SttProvider for MacOsSpeechProvider {
    fn id(&self) -> &str {
        MACOS_SPEECH_PROVIDER_ID
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        if request.audio.is_empty() {
            return Err(SttError::BadRequest("empty audio".into()));
        }
        let endpoint = self.endpoint();
        let filename = request.filename.clone();
        let payload = HostSpeechTranscribeRequest {
            audio_b64: STANDARD.encode(&request.audio),
            content_type: request.content_type.clone(),
            filename,
            language: request.language.clone(),
            message_id: request.message_id.clone(),
        };
        info!(
            "[STT:macos_speech] transcribing via host gateway url={} bytes={} content_type={} lang={:?}",
            endpoint,
            request.audio.len(),
            request.content_type,
            request.language
        );
        let response = self
            .client
            .post(endpoint)
            .timeout(MACOS_SPEECH_GATEWAY_TIMEOUT)
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                error!("[STT:macos_speech] transport failure: {error}");
                SttError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            // Apple Speech reports silence as a 500 with a "No speech detected"
            // body. That is a definitive silence verdict, NOT a provider failure
            // — surface it as NoSpeech so the chain treats it as terminal and does
            // NOT fall through to Whisper (which hallucinates a foreign-language
            // transcript from silent audio).
            if body.to_lowercase().contains("no speech detected") {
                debug!("[STT:macos_speech] no speech detected (silence) — terminal, no fallback");
                return Err(SttError::NoSpeech);
            }
            error!(
                "[STT:macos_speech] host gateway rejected status={} body={}",
                status, body
            );
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: HostSpeechTranscribeResponse = response.json().await.map_err(|error| {
            error!("[STT:macos_speech] response decode failure: {error}");
            SttError::Transport(format!("decoding host gateway response: {error}"))
        })?;
        debug!(
            "[STT:macos_speech] transcribed text_len={} language={:?}",
            parsed.transcript.len(),
            parsed.language
        );
        Ok(SttResponse {
            transcript: parsed.transcript,
            model: parsed.model.unwrap_or_else(|| self.default_model.clone()),
            language: parsed.language.or(request.language),
            message_id: request.message_id,
            extras: parsed.extras,
        })
    }
}

fn default_http_client() -> Client {
    Client::builder()
        .user_agent("magician-media-rails/0.1 macos-speech")
        .build()
        .expect("reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn request(audio: &'static [u8]) -> SttRequest {
        SttRequest {
            audio: Bytes::from_static(audio),
            content_type: "audio/wav".to_string(),
            language: Some("en-US".to_string()),
            model: None,
            message_id: Some("msg-1".to_string()),
            filename: Some("voice-note.wav".to_string()),
            prompt: None,
        }
    }

    #[test]
    fn endpoint_trims_gateway_trailing_slashes() {
        let provider = MacOsSpeechProvider::new("http://127.0.0.1:3017///");
        assert_eq!(
            provider.endpoint(),
            "http://127.0.0.1:3017/host/speech/transcribe"
        );
    }

    #[tokio::test]
    async fn rejects_empty_audio_before_calling_gateway() {
        let server = MockServer::start().await;
        let provider = MacOsSpeechProvider::new(server.uri());

        let err = provider
            .transcribe(SttRequest {
                audio: Bytes::new(),
                ..request(b"ignored")
            })
            .await
            .expect_err("empty audio should fail locally");

        assert!(matches!(err, SttError::BadRequest(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn posts_recorded_audio_to_host_gateway_and_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/speech/transcribe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "transcript": "hello from macOS speech",
                "model": "macos_speech",
                "language": "en-US",
                "extras": {
                    "provider": "apple_speech",
                    "mode": "recorded_file"
                }
            })))
            .mount(&server)
            .await;

        let provider = MacOsSpeechProvider::new(format!("{}/", server.uri()));
        let response = provider
            .transcribe(request(b"audio-bytes"))
            .await
            .expect("host gateway response should parse");

        assert_eq!(response.transcript, "hello from macOS speech");
        assert_eq!(response.model, "macos_speech");
        assert_eq!(response.language.as_deref(), Some("en-US"));
        assert_eq!(response.message_id.as_deref(), Some("msg-1"));
        assert_eq!(
            response.extras.unwrap()["provider"],
            Value::String("apple_speech".to_string())
        );

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["audio_b64"], STANDARD.encode(b"audio-bytes"));
        assert_eq!(body["content_type"], "audio/wav");
        assert_eq!(body["filename"], "voice-note.wav");
        assert_eq!(body["language"], "en-US");
        assert_eq!(body["message_id"], "msg-1");
    }

    #[tokio::test]
    async fn upstream_rejection_preserves_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/speech/transcribe"))
            .respond_with(ResponseTemplate::new(403).set_body_string("speech denied"))
            .mount(&server)
            .await;

        let provider = MacOsSpeechProvider::new(server.uri());
        let err = provider
            .transcribe(request(b"audio-bytes"))
            .await
            .expect_err("gateway rejection should surface");

        match err {
            SttError::Upstream { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body, "speech denied");
            },
            other => panic!("expected upstream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_model_falls_back_to_provider_default_and_language_hint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/speech/transcribe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "transcript": "bonjour"
            })))
            .mount(&server)
            .await;

        let provider = MacOsSpeechProvider::new(server.uri());
        let response = provider
            .transcribe(request(b"audio-bytes"))
            .await
            .expect("minimal response should parse");

        assert_eq!(response.model, MACOS_SPEECH_DEFAULT_MODEL);
        assert_eq!(response.language.as_deref(), Some("en-US"));
    }
}
