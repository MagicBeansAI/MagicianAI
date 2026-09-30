//! Gemini 3.5 Transcribe file-STT adapter (Dictation / recordings).
//!
//! Dedicated unary ASR via the Interactions API. Distinct from
//! [`super::gemini_stt::GeminiSttProvider`], which prompts a generateContent
//! chat model to transcribe.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, info};

use super::stt::{SttError, SttProvider, SttRequest, SttResponse};

pub const GEMINI_TRANSCRIBE_DEFAULT_BASE_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_TRANSCRIBE_PROVIDER_ID: &str = "gemini-transcribe";
pub const GEMINI_TRANSCRIBE_DEFAULT_MODEL: &str = "gemini-3.5-transcribe";
const INLINE_MAX_BYTES: usize = 14 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct GeminiTranscribeSttProvider {
    client: Client,
    api_key: String,
    base_url: String,
    provider_id: String,
    label: Option<String>,
    default_model: String,
}

impl GeminiTranscribeSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, GEMINI_TRANSCRIBE_DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::builder()
                .build()
                .expect("failed to build Gemini Transcribe HTTP client"),
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_id: GEMINI_TRANSCRIBE_PROVIDER_ID.to_string(),
            label: None,
            default_model: GEMINI_TRANSCRIBE_DEFAULT_MODEL.to_string(),
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
impl SttProvider for GeminiTranscribeSttProvider {
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
        if request.audio.len() > INLINE_MAX_BYTES {
            return Err(SttError::BadRequest(format!(
                "audio is {} bytes, above Gemini Transcribe inline limit {} bytes",
                request.audio.len(),
                INLINE_MAX_BYTES
            )));
        }

        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let mut generation_config = json!({
            "transcription_config": {
                "language_codes": [],
                "mode": "verbatim"
            }
        });
        if let Some(language) = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            generation_config["transcription_config"]["language_codes"] = json!([language]);
        }

        let body = json!({
            "model": model,
            "input": [{
                "type": "audio",
                "mime_type": request.content_type,
                "data": STANDARD.encode(&request.audio)
            }],
            "generation_config": generation_config
        });

        let url = format!("{}/interactions", self.base_url.trim_end_matches('/'));
        info!(
            "[STT-GEMINI-TRANSCRIBE] transcribing url={} model={} bytes={} content_type={}",
            url,
            model,
            request.audio.len(),
            request.content_type
        );
        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .timeout(Duration::from_secs(180))
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                error!("[STT-GEMINI-TRANSCRIBE] transport failure: {error}");
                SttError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[STT-GEMINI-TRANSCRIBE] upstream rejected status={} body={}",
                status, body
            );
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: Value = response.json().await.map_err(|error| {
            error!("[STT-GEMINI-TRANSCRIBE] response decode failure: {error}");
            SttError::Transport(format!("decoding response: {error}"))
        })?;
        let transcript = collect_interaction_text(&parsed);
        if transcript.trim().is_empty() {
            return Err(SttError::NoSpeech);
        }
        Ok(SttResponse {
            transcript,
            model,
            language: request.language,
            message_id: request.message_id,
            extras: Some(json!({
                "provider": self.provider_id,
                "interaction_id": parsed.get("id"),
            })),
        })
    }
}

fn collect_interaction_text(response: &Value) -> String {
    if let Some(text) = response
        .get("output_text")
        .or_else(|| response.get("outputText"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return text.to_string();
    }
    let mut parts = Vec::new();
    if let Some(steps) = response.get("steps").and_then(Value::as_array) {
        for step in steps {
            let Some(content) = step.get("content").and_then(Value::as_array) else {
                continue;
            };
            for item in content {
                if let Some(text) = item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    parts.push(text.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn transcribe_reads_interaction_output_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/interactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "interactions/abc",
                "output_text": "hello there"
            })))
            .mount(&server)
            .await;

        let provider = GeminiTranscribeSttProvider::with_base_url("test-key", server.uri());
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
        assert_eq!(response.transcript, "hello there");
        assert_eq!(response.model, GEMINI_TRANSCRIBE_DEFAULT_MODEL);
    }

    #[test]
    fn collect_interaction_text_falls_back_to_steps() {
        let response = json!({
            "steps": [{
                "content": [
                    { "type": "text", "text": "first" },
                    { "type": "text", "text": "second" }
                ]
            }]
        });
        assert_eq!(collect_interaction_text(&response), "first\nsecond");
    }
}
