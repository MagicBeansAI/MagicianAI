//! Gemini generateContent speech-to-text adapter.
//!
//! Gemini is not OpenAI-compatible for transcription. This adapter sends
//! recorded audio to `models/{model}:generateContent` with an explicit
//! transcription prompt and returns the generated text as the final transcript.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, error, info};

use super::stt::{SttError, SttProvider, SttRequest, SttResponse};

pub const GEMINI_STT_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_STT_PROVIDER_ID: &str = "gemini";
pub const GEMINI_STT_DEFAULT_MODEL: &str = "gemini-3.5-flash";
pub const GEMINI_STT_DEFAULT_INLINE_MAX_BYTES: usize = 14 * 1024 * 1024;
pub const GEMINI_STT_DEFAULT_PROMPT: &str =
    "Generate a clean verbatim transcript of the speech. Return only the transcript text.";

#[derive(Debug, Clone)]
pub struct GeminiSttProvider {
    client: Client,
    api_key: String,
    base_url: String,
    provider_id: String,
    label: Option<String>,
    default_model: String,
    default_prompt: String,
    inline_max_bytes: usize,
    generation_config: Option<Value>,
}

impl GeminiSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, GEMINI_STT_DEFAULT_BASE_URL)
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
            provider_id: GEMINI_STT_PROVIDER_ID.to_string(),
            label: None,
            default_model: GEMINI_STT_DEFAULT_MODEL.to_string(),
            default_prompt: GEMINI_STT_DEFAULT_PROMPT.to_string(),
            inline_max_bytes: GEMINI_STT_DEFAULT_INLINE_MAX_BYTES,
            generation_config: None,
        }
    }

    pub fn with_provider_id(mut self, provider_id: impl Into<String>) -> Self {
        self.provider_id = provider_id.into();
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

    pub fn with_default_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.default_prompt = prompt.into();
        self
    }

    pub fn with_inline_max_bytes(mut self, max_bytes: usize) -> Self {
        self.inline_max_bytes = max_bytes;
        self
    }

    pub fn with_generation_config(mut self, generation_config: Value) -> Self {
        self.generation_config = Some(generation_config);
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
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
}

#[async_trait]
impl SttProvider for GeminiSttProvider {
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
        if request.audio.len() > self.inline_max_bytes {
            return Err(SttError::BadRequest(format!(
                "audio is {} bytes, above Gemini inline limit {} bytes for this provider",
                request.audio.len(),
                self.inline_max_bytes
            )));
        }

        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let mut prompt = request
            .prompt
            .clone()
            .unwrap_or_else(|| self.default_prompt.clone());
        if let Some(language) = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            prompt.push_str("\nLanguage hint: ");
            prompt.push_str(language);
            prompt.push('.');
        }

        let audio_b64 = STANDARD.encode(&request.audio);
        let content_type = request.content_type.clone();
        let mut body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "text": prompt },
                    {
                        "inline_data": {
                            "mime_type": content_type,
                            "data": audio_b64
                        }
                    }
                ]
            }]
        });
        if let Some(generation_config) = &self.generation_config {
            body["generationConfig"] = generation_config.clone();
        }

        let url = generate_content_url(&self.base_url, &model);
        info!(
            "[STT-GEMINI] transcribing url={} model={} bytes={} content_type={} lang={:?}",
            url,
            model,
            request.audio.len(),
            request.content_type,
            request.language
        );
        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .timeout(Duration::from_secs(180))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                error!("[STT-GEMINI] transport failure: {e}");
                SttError::Transport(e.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[STT-GEMINI] upstream rejected status={} body={}",
                status, body
            );
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: GeminiGenerateContentResponse = response.json().await.map_err(|e| {
            error!("[STT-GEMINI] response decode failure: {e}");
            SttError::Transport(format!("decoding response: {e}"))
        })?;
        let transcript = collect_transcript(&parsed);
        if transcript.trim().is_empty() {
            return Err(SttError::NoSpeech);
        }
        debug!(
            "[STT-GEMINI] transcribed text_len={} candidate_count={}",
            transcript.len(),
            parsed.candidates.len()
        );
        Ok(SttResponse {
            transcript,
            model,
            language: request.language,
            message_id: request.message_id,
            extras: Some(json!({
                "provider": self.provider_id,
                "candidate_count": parsed.candidates.len(),
                "finish_reasons": parsed
                    .candidates
                    .iter()
                    .filter_map(|candidate| candidate.finish_reason.clone())
                    .collect::<Vec<_>>(),
            })),
        })
    }
}

fn collect_transcript(response: &GeminiGenerateContentResponse) -> String {
    response
        .candidates
        .iter()
        .filter_map(|candidate| candidate.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn generate_content_url(base_url: &str, model: &str) -> String {
    let model = model.trim().strip_prefix("models/").unwrap_or(model.trim());
    format!(
        "{}/models/{}:generateContent",
        base_url.trim_end_matches('/'),
        model
    )
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build Gemini STT HTTP client")
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use wiremock::{
        matchers::{body_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    #[test]
    fn provider_metadata_is_configurable() {
        let provider = GeminiSttProvider::with_base_url("test-key", "https://example.invalid")
            .with_provider_id("gemini-fast")
            .with_label("Gemini Fast")
            .with_default_model("gemini-test")
            .with_default_prompt("transcribe")
            .with_inline_max_bytes(32);

        assert_eq!(provider.id(), "gemini-fast");
        assert_eq!(provider.label(), Some("Gemini Fast"));
        assert_eq!(provider.default_model(), "gemini-test");
        assert_eq!(provider.inline_max_bytes, 32);
    }

    #[tokio::test]
    async fn transcribe_parses_generate_content_text_parts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-test:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {
                        "parts": [
                            { "text": "hello" },
                            { "text": "world" }
                        ]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = GeminiSttProvider::with_base_url("test-key", server.uri())
            .with_default_model("gemini-test");
        let response = provider
            .transcribe(SttRequest {
                audio: Bytes::from_static(b"audio"),
                content_type: "audio/webm".to_string(),
                language: Some("en-US".to_string()),
                model: None,
                message_id: Some("msg_1".to_string()),
                filename: None,
                prompt: None,
            })
            .await
            .expect("transcription should parse");

        assert_eq!(response.transcript, "hello\nworld");
        assert_eq!(response.model, "gemini-test");
        assert_eq!(response.language.as_deref(), Some("en-US"));
        assert_eq!(response.message_id.as_deref(), Some("msg_1"));
    }

    #[tokio::test]
    async fn transcribe_sends_rest_generation_config_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-test:generateContent"))
            .and(body_json(json!({
                "contents": [{
                    "role": "user",
                    "parts": [
                        { "text": "transcribe" },
                        {
                            "inline_data": {
                                "mime_type": "audio/webm",
                                "data": "YXVkaW8="
                            }
                        }
                    ]
                }],
                "generationConfig": {
                    "temperature": 0
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "finishReason": "STOP",
                    "content": {
                        "parts": [
                            { "text": "hello" }
                        ]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = GeminiSttProvider::with_base_url("test-key", server.uri())
            .with_default_model("gemini-test")
            .with_default_prompt("transcribe")
            .with_generation_config(json!({ "temperature": 0 }));
        let response = provider
            .transcribe(SttRequest {
                audio: Bytes::from_static(b"audio"),
                content_type: "audio/webm".to_string(),
                language: None,
                model: None,
                message_id: None,
                filename: None,
                prompt: None,
            })
            .await
            .expect("transcription should parse");

        assert_eq!(response.transcript, "hello");
    }
}
