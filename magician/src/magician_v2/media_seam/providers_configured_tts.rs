//! Small decorator for config-defined TTS provider ids and labels.
//!
//! Concrete adapters such as `OpenAiTtsProvider` have natural adapter ids like
//! `openai`, but `magician-config.yaml` may define several OpenAI-compatible
//! endpoints or model variants. This wrapper gives each configured entry its
//! own stable request id while delegating synthesis to the real adapter.

use std::sync::Arc;

use async_trait::async_trait;

use crate::magician_v2::media_seam::tts::{
    TtsCacheStats, TtsError, TtsProvider, TtsRequest, TtsResponse,
};

pub struct ConfiguredTtsProvider {
    inner: Arc<dyn TtsProvider>,
    id: String,
    label: Option<String>,
    preferred_voice: Option<String>,
    voices: Option<Vec<String>>,
}

impl ConfiguredTtsProvider {
    pub fn new(inner: Arc<dyn TtsProvider>, id: impl Into<String>, label: Option<String>) -> Self {
        Self {
            inner,
            id: id.into(),
            label,
            preferred_voice: None,
            voices: None,
        }
    }

    /// Allow config-defined catalogs for compatible endpoints and account voices.
    pub fn with_voice_catalog(mut self, voices: &[String]) -> Self {
        if !voices.is_empty() {
            let mut catalog = Vec::new();
            for voice in voices
                .iter()
                .map(|voice| voice.trim())
                .filter(|voice| !voice.is_empty())
            {
                if !catalog.iter().any(|id| id == voice) {
                    catalog.push(voice.to_string());
                }
            }
            if let Some(default) = self.inner.default_voice() {
                if !catalog.iter().any(|id| id == default) {
                    catalog.push(default.to_string());
                }
            }
            self.voices = Some(catalog);
        }
        self
    }

    /// Capture a scope's choice outside the shared provider/cache. Ignore stale
    /// preferences after a model/catalog change; explicit request voices win.
    pub fn with_preferred_voice(mut self, voice: Option<&str>) -> Self {
        self.preferred_voice = voice
            .filter(|voice| self.supported_voices().iter().any(|id| id == voice))
            .map(str::to_string);
        self
    }
}

#[async_trait]
impl TtsProvider for ConfiguredTtsProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_voice(&self) -> Option<&str> {
        self.preferred_voice
            .as_deref()
            .or_else(|| self.inner.default_voice())
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn default_format(&self) -> Option<&str> {
        self.inner.default_format()
    }

    fn supported_voices(&self) -> Vec<String> {
        self.voices
            .clone()
            .unwrap_or_else(|| self.inner.supported_voices())
    }

    fn supported_formats(&self) -> Vec<String> {
        self.inner.supported_formats()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn synthesize(&self, mut request: TtsRequest) -> Result<TtsResponse, TtsError> {
        if request.voice.is_none()
            && request
                .model
                .as_deref()
                .is_none_or(|model| model.trim() == self.default_model())
        {
            request.voice.clone_from(&self.preferred_voice);
        }
        self.inner.synthesize(request).await
    }

    async fn clear_cache(&self) {
        self.inner.clear_cache().await;
    }

    async fn cache_len(&self) -> usize {
        self.inner.cache_len().await
    }

    fn cache_stats(&self) -> TtsCacheStats {
        self.inner.cache_stats()
    }
}

/// Bind each fallback's own preference, never a primary provider's voice ID.
pub fn with_tts_voice_preferences(
    chain: Vec<Arc<dyn TtsProvider>>,
    voices: &std::collections::BTreeMap<String, String>,
) -> Vec<Arc<dyn TtsProvider>> {
    chain
        .into_iter()
        .map(|provider| {
            let binding = ConfiguredTtsProvider::new(
                Arc::clone(&provider),
                provider.id(),
                provider.label().map(str::to_string),
            )
            .with_preferred_voice(voices.get(provider.id()).map(String::as_str));
            Arc::new(binding) as Arc<dyn TtsProvider>
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoVoice(&'static str);
    #[async_trait]
    impl TtsProvider for EchoVoice {
        fn id(&self) -> &str {
            self.0
        }
        fn default_voice(&self) -> Option<&str> {
            Some("default")
        }
        fn default_model(&self) -> &str {
            "test-model"
        }
        fn supported_voices(&self) -> Vec<String> {
            vec!["default".into(), format!("{}-voice", self.0)]
        }
        async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
            Ok(TtsResponse {
                audio: bytes::Bytes::new(),
                content_type: "audio/wav".into(),
                model: self.default_model().into(),
                voice: request.voice,
                message_id: None,
            })
        }
    }

    fn request(voice: Option<&str>) -> TtsRequest {
        TtsRequest {
            text: "hello".into(),
            voice: voice.map(str::to_string),
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

    #[tokio::test]
    async fn each_fallback_uses_its_own_voice_and_explicit_override_wins() {
        let voices = [
            ("a".into(), "a-voice".into()),
            ("b".into(), "b-voice".into()),
        ]
        .into();
        let chain = with_tts_voice_preferences(
            vec![Arc::new(EchoVoice("a")), Arc::new(EchoVoice("b"))],
            &voices,
        );
        assert_eq!(
            chain[0]
                .synthesize(request(None))
                .await
                .unwrap()
                .voice
                .as_deref(),
            Some("a-voice")
        );
        assert_eq!(
            chain[1]
                .synthesize(request(None))
                .await
                .unwrap()
                .voice
                .as_deref(),
            Some("b-voice")
        );
        assert_eq!(
            chain[0]
                .synthesize(request(Some("default")))
                .await
                .unwrap()
                .voice
                .as_deref(),
            Some("default")
        );
        // A different scope receives the original defaults, not a mutated provider.
        let other = with_tts_voice_preferences(vec![Arc::new(EchoVoice("a"))], &Default::default());
        assert!(other[0]
            .synthesize(request(None))
            .await
            .unwrap()
            .voice
            .is_none());
    }

    #[tokio::test]
    async fn stale_voice_does_not_break_a_changed_model() {
        let provider = ConfiguredTtsProvider::new(Arc::new(EchoVoice("a")), "a", None)
            .with_preferred_voice(Some("retired-model-voice"));
        assert_eq!(provider.default_voice(), Some("default"));
        assert!(provider
            .synthesize(request(None))
            .await
            .unwrap()
            .voice
            .is_none());
    }

    #[tokio::test]
    async fn request_model_override_does_not_inherit_another_models_saved_voice() {
        let provider = ConfiguredTtsProvider::new(Arc::new(EchoVoice("a")), "a", None)
            .with_preferred_voice(Some("a-voice"));
        let mut other_model = request(None);
        other_model.model = Some("other-model".into());
        assert!(provider
            .synthesize(other_model)
            .await
            .unwrap()
            .voice
            .is_none());
    }
}
