//! Google Cloud Speech-to-Text V2 adapter.
//!
//! This is a recorded-audio STT adapter for the synchronous
//! `projects.locations.recognizers.recognize` REST method. It is not a
//! realtime voice model; it only returns transcription results.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use serde_json::{json, Map, Value};
use tracing::{debug, error, info};

use super::stt::{SttError, SttProvider, SttRequest, SttResponse};

pub const GOOGLE_CLOUD_SPEECH_STT_DEFAULT_BASE_URL: &str = "https://speech.googleapis.com/v2";
pub const GOOGLE_CLOUD_SPEECH_STT_PROVIDER_ID: &str = "google_cloud_speech";
pub const GOOGLE_CLOUD_SPEECH_STT_DEFAULT_MODEL: &str = "chirp_3";
pub const GOOGLE_CLOUD_SPEECH_STT_DEFAULT_LOCATION: &str = "global";
pub const GOOGLE_CLOUD_SPEECH_STT_DEFAULT_RECOGNIZER: &str = "_";

#[derive(Debug, Clone)]
pub struct GoogleCloudSpeechSttProvider {
    client: Client,
    access_token: String,
    base_url: String,
    provider_id: String,
    label: Option<String>,
    default_model: String,
    recognizer: String,
    language_codes: Vec<String>,
    cloud_features: Option<Value>,
    cloud_config: Option<Value>,
}

impl GoogleCloudSpeechSttProvider {
    pub fn new(access_token: impl Into<String>, recognizer: impl Into<String>) -> Self {
        Self::with_client(
            default_http_client(),
            access_token,
            GOOGLE_CLOUD_SPEECH_STT_DEFAULT_BASE_URL,
            recognizer,
        )
    }

    pub fn with_base_url(
        access_token: impl Into<String>,
        base_url: impl Into<String>,
        recognizer: impl Into<String>,
    ) -> Self {
        Self::with_client(default_http_client(), access_token, base_url, recognizer)
    }

    pub fn with_client(
        client: Client,
        access_token: impl Into<String>,
        base_url: impl Into<String>,
        recognizer: impl Into<String>,
    ) -> Self {
        Self {
            client,
            access_token: access_token.into(),
            base_url: base_url.into(),
            provider_id: GOOGLE_CLOUD_SPEECH_STT_PROVIDER_ID.to_string(),
            label: None,
            default_model: GOOGLE_CLOUD_SPEECH_STT_DEFAULT_MODEL.to_string(),
            recognizer: recognizer.into(),
            language_codes: Vec::new(),
            cloud_features: None,
            cloud_config: None,
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

    pub fn with_language_codes(mut self, language_codes: Vec<String>) -> Self {
        self.language_codes = language_codes;
        self
    }

    pub fn with_cloud_features(mut self, features: Value) -> Self {
        self.cloud_features = Some(features);
        self
    }

    pub fn with_cloud_config(mut self, config: Value) -> Self {
        self.cloud_config = Some(config);
        self
    }
}

#[async_trait]
impl SttProvider for GoogleCloudSpeechSttProvider {
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
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let language_codes = if !self.language_codes.is_empty() {
            self.language_codes.clone()
        } else if let Some(language) = request
            .language
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            vec![language.to_string()]
        } else {
            vec!["auto".to_string()]
        };

        let mut recognition_config = Map::new();
        recognition_config.insert("model".to_string(), json!(model));
        recognition_config.insert("languageCodes".to_string(), json!(language_codes));
        recognition_config.insert("autoDecodingConfig".to_string(), json!({}));
        if let Some(features) = &self.cloud_features {
            recognition_config.insert("features".to_string(), features.clone());
        }
        if let Some(extra_config) = &self.cloud_config {
            merge_object(&mut recognition_config, extra_config);
        }

        let body = json!({
            "config": Value::Object(recognition_config),
            "content": STANDARD.encode(&request.audio),
        });
        let url = recognize_url(&self.base_url, &self.recognizer);
        info!(
            "[STT-GCLOUD] transcribing url={} model={} recognizer={} bytes={} content_type={} lang={:?}",
            url,
            model,
            self.recognizer,
            request.audio.len(),
            request.content_type,
            request.language
        );
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.access_token)
            .timeout(Duration::from_secs(180))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                error!("[STT-GCLOUD] transport failure: {e}");
                SttError::Transport(e.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[STT-GCLOUD] upstream rejected status={} body={}",
                status, body
            );
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: Value = response.json().await.map_err(|e| {
            error!("[STT-GCLOUD] response decode failure: {e}");
            SttError::Transport(format!("decoding response: {e}"))
        })?;
        let (transcript, detected_language, confidence_values) = extract_transcript(&parsed);
        if transcript.trim().is_empty() {
            return Err(SttError::NoSpeech);
        }
        let language = detected_language.or(request.language);
        debug!(
            "[STT-GCLOUD] transcribed text_len={} confidence_count={}",
            transcript.len(),
            confidence_values.len()
        );
        Ok(SttResponse {
            transcript,
            model,
            language,
            message_id: request.message_id,
            extras: Some(json!({
                "provider": self.provider_id,
                "recognizer": self.recognizer,
                "metadata": parsed.get("metadata").cloned().unwrap_or(Value::Null),
                "mean_confidence": mean(&confidence_values),
            })),
        })
    }
}

fn extract_transcript(response: &Value) -> (String, Option<String>, Vec<f64>) {
    let mut segments = Vec::new();
    let mut detected_language = None;
    let mut confidences = Vec::new();
    if let Some(results) = response.get("results").and_then(Value::as_array) {
        for result in results {
            if detected_language.is_none() {
                detected_language = result
                    .get("languageCode")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            let Some(alternatives) = result.get("alternatives").and_then(Value::as_array) else {
                continue;
            };
            let Some(alternative) = alternatives.first() else {
                continue;
            };
            if let Some(transcript) = alternative.get("transcript").and_then(Value::as_str) {
                let transcript = transcript.trim();
                if !transcript.is_empty() {
                    segments.push(transcript.to_string());
                }
            }
            if let Some(confidence) = alternative.get("confidence").and_then(Value::as_f64) {
                if confidence > 0.0 {
                    confidences.push(confidence);
                }
            }
        }
    }
    (segments.join(" "), detected_language, confidences)
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn merge_object(base: &mut Map<String, Value>, extra: &Value) {
    let Some(extra) = extra.as_object() else {
        return;
    };
    for (key, value) in extra {
        base.insert(key.clone(), value.clone());
    }
}

fn recognize_url(base_url: &str, recognizer: &str) -> String {
    format!(
        "{}/{}:recognize",
        base_url.trim_end_matches('/'),
        recognizer.trim_start_matches('/')
    )
}

pub fn recognizer_name(
    project_id: &str,
    location: &str,
    recognizer: Option<&str>,
) -> Option<String> {
    let recognizer = recognizer
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(GOOGLE_CLOUD_SPEECH_STT_DEFAULT_RECOGNIZER);
    if recognizer.starts_with("projects/") {
        return Some(recognizer.to_string());
    }
    let project_id = project_id.trim();
    let location = location.trim();
    if project_id.is_empty() || location.is_empty() {
        return None;
    }
    Some(format!(
        "projects/{project_id}/locations/{location}/recognizers/{recognizer}"
    ))
}

pub fn default_base_url_for_location(location: &str) -> String {
    let location = location.trim();
    if location.is_empty() || location.eq_ignore_ascii_case("global") {
        return GOOGLE_CLOUD_SPEECH_STT_DEFAULT_BASE_URL.to_string();
    }
    format!("https://{location}-speech.googleapis.com/v2")
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build Google Cloud Speech STT HTTP client")
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    #[test]
    fn recognizer_name_builds_implicit_or_accepts_full_resource() {
        assert_eq!(
            recognizer_name("proj", "us", None).as_deref(),
            Some("projects/proj/locations/us/recognizers/_")
        );
        assert_eq!(
            recognizer_name(
                "ignored",
                "ignored",
                Some("projects/p/locations/global/recognizers/r1")
            )
            .as_deref(),
            Some("projects/p/locations/global/recognizers/r1")
        );
    }

    #[test]
    fn default_base_url_tracks_location() {
        assert_eq!(
            default_base_url_for_location("global"),
            GOOGLE_CLOUD_SPEECH_STT_DEFAULT_BASE_URL
        );
        assert_eq!(
            default_base_url_for_location("us"),
            "https://us-speech.googleapis.com/v2"
        );
        assert_eq!(
            default_base_url_for_location("asia-southeast1"),
            "https://asia-southeast1-speech.googleapis.com/v2"
        );
    }

    #[test]
    fn provider_metadata_is_configurable() {
        let provider = GoogleCloudSpeechSttProvider::with_base_url(
            "test-token",
            "https://example.invalid/v2",
            "projects/proj/locations/us/recognizers/_",
        )
        .with_provider_id("google-chirp3")
        .with_label("Google Chirp 3")
        .with_default_model("chirp_3")
        .with_language_codes(vec!["auto".to_string()])
        .with_cloud_features(json!({ "enableAutomaticPunctuation": true }));

        assert_eq!(provider.id(), "google-chirp3");
        assert_eq!(provider.label(), Some("Google Chirp 3"));
        assert_eq!(provider.default_model(), "chirp_3");
        assert_eq!(provider.language_codes, vec!["auto"]);
    }

    #[tokio::test]
    async fn transcribe_parses_recognize_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/projects/proj/locations/us/recognizers/_:recognize"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "languageCode": "en-US",
                    "alternatives": [{
                        "transcript": "hello world",
                        "confidence": 0.91
                    }]
                }],
                "metadata": {
                    "requestId": "req-1",
                    "totalBilledDuration": "1s"
                }
            })))
            .mount(&server)
            .await;

        let provider = GoogleCloudSpeechSttProvider::with_base_url(
            "test-token",
            server.uri(),
            "projects/proj/locations/us/recognizers/_",
        )
        .with_default_model("chirp_3");
        let response = provider
            .transcribe(SttRequest {
                audio: Bytes::from_static(b"audio"),
                content_type: "audio/webm".to_string(),
                language: None,
                model: None,
                message_id: Some("msg_1".to_string()),
                filename: None,
                prompt: None,
            })
            .await
            .expect("transcription should parse");

        assert_eq!(response.transcript, "hello world");
        assert_eq!(response.model, "chirp_3");
        assert_eq!(response.language.as_deref(), Some("en-US"));
        assert_eq!(response.message_id.as_deref(), Some("msg_1"));
        assert_eq!(response.extras.unwrap()["mean_confidence"], json!(0.91));
    }
}
