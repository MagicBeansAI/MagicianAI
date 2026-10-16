use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::media_rails::{SttError, SttProvider, SttRequest, SttResponse};

use super::engine_manager::FluidAudioEngineManager;

pub struct FluidAudioRecordingSttProvider {
    id: String,
    label: Option<String>,
    model: String,
    manager: Arc<FluidAudioEngineManager>,
}
impl FluidAudioRecordingSttProvider {
    pub fn new(
        id: impl Into<String>,
        label: Option<String>,
        model: impl Into<String>,
        manager: Arc<FluidAudioEngineManager>,
    ) -> Self {
        Self {
            id: id.into(),
            label,
            model: model.into(),
            manager,
        }
    }
}

#[async_trait]
impl SttProvider for FluidAudioRecordingSttProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        if let Some(requested) = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !requested.eq_ignore_ascii_case(&self.model)
                && !requested.eq_ignore_ascii_case(&self.id)
            {
                return Err(SttError::BadRequest(format!(
                    "FluidAudio provider {} is pinned to configured model {}",
                    self.id, self.model
                )));
            }
        }

        let response = self
            .manager
            .transcribe(
                &self.id,
                request.audio,
                &request.content_type,
                request.language.as_deref(),
            )
            .await
            .map_err(SttError::Transport)?;
        let transcript = response.transcript.trim().to_string();
        if transcript.is_empty() {
            return Err(SttError::NoSpeech);
        }

        Ok(SttResponse {
            transcript,
            model: response.model.clone(),
            language: response.language.clone(),
            message_id: request.message_id,
            extras: Some(json!({
                "engine": "fluid_audio",
                "provider_id": response.model_id,
                "model": response.model,
                "variant": response.variant,
                "audio_duration_ms": response.audio_duration_ms,
                "processing_duration_ms": response.processing_duration_ms,
                "confidence": response.confidence,
            })),
        })
    }
}
