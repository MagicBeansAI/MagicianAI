use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{
    capability::{LLMCapability, LLMProviderKind},
    error::{LLMError, LLMResult},
    types::{EmbeddingRequest, EmbeddingResponse, LLMRequest, LLMResponse, StreamDelta},
};

/// Trait implemented by provider-specific adapters.
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Identifier for this provider implementation.
    fn provider_kind(&self) -> LLMProviderKind;

    /// Returns the capability declaration for a given model identifier.
    fn capabilities(&self, model: &str) -> LLMCapability;

    /// Executes the provider invocation with a normalised request.
    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse>;

    /// Embedding invocation for providers with an embedding endpoint. The
    /// default refusal keeps remote generation providers untouched until one
    /// actually binds an embedding profile — refusing loudly beats silently
    /// routing vectors through a model that cannot produce them.
    async fn embed(&self, _request: EmbeddingRequest) -> LLMResult<EmbeddingResponse> {
        Err(LLMError::UnsupportedCapability(format!(
            "provider `{}` does not implement embeddings",
            self.provider_kind().as_str()
        )))
    }

    /// Stream invocation — sends deltas to the provided channel.
    /// Default: falls back to non-streaming invoke and sends Done.
    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let response = self.invoke(request).await?;
        let _ = tx.send(StreamDelta::Done(response)).await;
        Ok(())
    }

    /// Performs an optional health check against the underlying API.
    async fn health_check(&self) -> LLMResult<bool> {
        Ok(true)
    }
}
