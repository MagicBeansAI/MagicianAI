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
}

impl ConfiguredTtsProvider {
    pub fn new(inner: Arc<dyn TtsProvider>, id: impl Into<String>, label: Option<String>) -> Self {
        Self {
            inner,
            id: id.into(),
            label,
        }
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
        self.inner.default_voice()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn default_format(&self) -> Option<&str> {
        self.inner.default_format()
    }

    fn supported_voices(&self) -> Vec<String> {
        self.inner.supported_voices()
    }

    fn supported_formats(&self) -> Vec<String> {
        self.inner.supported_formats()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, TtsError> {
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
