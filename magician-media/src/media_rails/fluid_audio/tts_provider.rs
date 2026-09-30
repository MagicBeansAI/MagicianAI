use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;

use crate::media_rails::{TtsError, TtsPace, TtsProvider, TtsRequest, TtsResponse};

use super::engine_manager::FluidAudioEngineManager;
use super::protocol::SidecarSpeechRequest;

pub struct FluidAudioTtsProvider {
    id: String,
    label: Option<String>,
    model: String,
    default_voice: String,
    voices: Vec<String>,
    formats: Vec<String>,
    manager: Arc<FluidAudioEngineManager>,
}

impl FluidAudioTtsProvider {
    pub fn new(
        id: impl Into<String>,
        label: Option<String>,
        model: impl Into<String>,
        default_voice: impl Into<String>,
        voices: Vec<String>,
        formats: Vec<String>,
        manager: Arc<FluidAudioEngineManager>,
    ) -> Result<Self, String> {
        let id = id.into();
        let model = model.into();
        let default_voice = default_voice.into();
        let voices = normalized_values(voices);
        let formats = normalized_values(formats)
            .into_iter()
            .map(|format| format.to_ascii_lowercase())
            .collect::<Vec<_>>();
        if id.trim().is_empty() || model.trim().is_empty() || default_voice.trim().is_empty() {
            return Err(
                "FluidAudio TTS id, model, and default voice must be non-empty".to_string(),
            );
        }
        if !voices.iter().any(|voice| voice == &default_voice) {
            return Err(
                "FluidAudio TTS default voice is absent from configured voices".to_string(),
            );
        }
        if formats != ["wav"] {
            return Err("FluidAudio Kokoro currently requires formats: [wav]".to_string());
        }
        Ok(Self {
            id,
            label,
            model,
            default_voice,
            voices,
            formats,
            manager,
        })
    }
}

#[async_trait]
impl TtsProvider for FluidAudioTtsProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_voice(&self) -> Option<&str> {
        Some(&self.default_voice)
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn default_format(&self) -> Option<&str> {
        self.formats.first().map(String::as_str)
    }

    fn supported_voices(&self) -> Vec<String> {
        self.voices.clone()
    }

    fn supported_formats(&self) -> Vec<String> {
        self.formats.clone()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
        if request.text.trim().is_empty() {
            return Err(TtsError::BadRequest("empty text".to_string()));
        }
        if request
            .model
            .as_deref()
            .is_some_and(|model| !model.eq_ignore_ascii_case(&self.model))
        {
            return Err(TtsError::BadRequest(format!(
                "model is not configured for TTS provider {}",
                self.id
            )));
        }
        let voice = request
            .voice
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.default_voice)
            .to_string();
        if !self.voices.iter().any(|candidate| candidate == &voice) {
            return Err(TtsError::BadRequest(format!(
                "voice {voice} is not configured for TTS provider {}",
                self.id
            )));
        }
        let speed = resolve_speed(request.rate, request.pace);
        if !(0.5..=2.0).contains(&speed) {
            return Err(TtsError::BadRequest(
                "FluidAudio TTS rate must be between 0.5 and 2.0".to_string(),
            ));
        }
        let response = self
            .manager
            .synthesize_speech(
                &self.id,
                &SidecarSpeechRequest {
                    input: request.text,
                    voice: Some(voice.clone()),
                    response_format: "wav".to_string(),
                    speed: Some(speed),
                },
            )
            .await
            .map_err(|body| TtsError::Upstream { status: 502, body })?;
        Ok(TtsResponse {
            audio: response.audio,
            content_type: "audio/wav".to_string(),
            model: self.model.clone(),
            voice: Some(response.voice),
            message_id: request.message_id,
        })
    }
}

fn normalized_values(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn resolve_speed(rate: Option<f32>, pace: Option<TtsPace>) -> f32 {
    rate.unwrap_or_else(|| match pace.unwrap_or_default() {
        TtsPace::Slow => 0.85,
        TtsPace::Normal => 1.0,
        TtsPace::Fast => 1.15,
    })
}

#[cfg(test)]
mod tests {
    use super::{normalized_values, resolve_speed};
    use crate::media_rails::TtsPace;

    #[test]
    fn configured_inventory_is_trimmed_and_deduplicated() {
        assert_eq!(
            normalized_values(vec![
                " af_heart ".to_string(),
                "".to_string(),
                "af_heart".to_string(),
                "am_michael".to_string(),
            ]),
            ["af_heart", "am_michael"]
        );
    }

    #[test]
    fn explicit_rate_wins_and_pace_maps_conservatively() {
        assert_eq!(resolve_speed(Some(1.3), Some(TtsPace::Slow)), 1.3);
        assert_eq!(resolve_speed(None, Some(TtsPace::Slow)), 0.85);
        assert_eq!(resolve_speed(None, Some(TtsPace::Fast)), 1.15);
    }
}
