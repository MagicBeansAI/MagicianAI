//! xAI (Grok) over the xAI Responses API.
//!
//! xAI recommends its Responses API, and it is the only one of its two
//! endpoints where both halves of context reuse work: `previous_response_id`
//! carries the conversation — encrypted reasoning included — server-side,
//! and `prompt_cache_key` routes the conversation's requests to the server
//! that holds its cache. On Chat Completions a reasoning model's cache misses
//! unless every prior `reasoning_content` is sent back, so this provider does
//! not offer that endpoint: a profile asking for `openai_api_mode: chat`
//! fails at the request, not with a quietly degraded run.
//!
//! The wire format is OpenAI's Responses protocol; the measured differences
//! live in [`ResponsesDialect::Xai`].

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    providers::openai_responses::{OpenAIResponsesProvider, ResponsesDialect},
    types::{LLMRequest, LLMResponse, StreamDelta},
};

pub const DEFAULT_BASE_URL_XAI_RESPONSES: &str = "https://api.x.ai/v1/responses";

/// Provider for xAI's Grok models.
pub struct XaiProvider {
    inner: OpenAIResponsesProvider,
}

/// Grok 4.5 and later take image input. Older and specialised models
/// (`grok-4.3`, the `grok-4.20-*` line, `grok-build-*`) are text-only here
/// until xAI documents otherwise; a profile may still assert vision
/// explicitly.
fn xai_model_supports_vision(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    let model = model.rsplit('/').next().unwrap_or_default();
    let Some(version) = model.strip_prefix("grok-") else {
        return false;
    };
    let mut parts = version.split(['.', '-']);
    let major = parts.next().and_then(|part| part.parse::<u32>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u32>().ok());
    match (major, minor) {
        (Some(major), _) if major > 4 => true,
        (Some(4), Some(minor)) => (5..20).contains(&minor),
        _ => false,
    }
}

impl XaiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL_XAI_RESPONSES)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            inner: OpenAIResponsesProvider::with_base_url_for_dialect(
                api_key,
                base_url,
                ResponsesDialect::Xai,
            ),
        }
    }

    /// Whether this Grok model accepts image input. Public so config
    /// validation gates `supports_vision` on the same predicate the runtime
    /// uses.
    pub fn model_supports_vision(model: &str) -> bool {
        xai_model_supports_vision(model)
    }

    pub fn capabilities_for_model(model: &str) -> LLMCapability {
        let mut modalities = vec![LLMModality::Text];
        if xai_model_supports_vision(model) {
            modalities.push(LLMModality::Vision);
        }
        LLMCapability {
            modalities,
            reasoning: LLMReasoning::Standard,
            tool_calling: true,
            json_mode: true,
            streaming: true,
            computer_use: false,
            web_search: false,
        }
    }

    fn reject_chat_mode(request: &LLMRequest) -> LLMResult<()> {
        let mode = request
            .extra_value()
            .and_then(|extra| extra.get("openai_api_mode"))
            .and_then(Value::as_str)
            .map(|mode| mode.trim().to_ascii_lowercase());
        match mode.as_deref() {
            None | Some("responses") | Some("auto") => Ok(()),
            Some(other) => Err(LLMError::Configuration(format!(
                "xai: `openai_api_mode: {other}` is not served; the xAI provider speaks only the \
                 Responses API (server-side continuation and per-conversation cache routing)"
            ))),
        }
    }
}

#[async_trait]
impl LLMProvider for XaiProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Xai
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        Self::reject_chat_mode(&request)?;
        self.inner.invoke(request).await
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        Self::reject_chat_mode(&request)?;
        self.inner.invoke_stream(request, tx).await
    }

    async fn health_check(&self) -> LLMResult<bool> {
        self.inner.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn grok_4_5_and_later_advertise_vision_and_older_lines_do_not() {
        for model in [
            "grok-4.7",
            "grok-4.6",
            "grok-4.5",
            "xai/grok-4.7",
            "GROK-4.7",
        ] {
            let caps = XaiProvider::capabilities_for_model(model);
            assert!(caps.modalities.contains(&LLMModality::Vision), "{model}");
            assert_eq!(caps.reasoning, LLMReasoning::Standard);
            assert!(caps.tool_calling);
            assert!(!caps.web_search, "no reviewed xAI server search");
        }
        for model in [
            "grok-4.3",
            "grok-4.20-0309-reasoning",
            "grok-build-0.1",
            "grok-4",
            "gpt-5.6-terra",
        ] {
            assert!(
                !XaiProvider::capabilities_for_model(model)
                    .modalities
                    .contains(&LLMModality::Vision),
                "{model}"
            );
        }
    }

    #[tokio::test]
    async fn a_chat_mode_request_is_refused_before_any_io() {
        let provider = XaiProvider::with_base_url("k", "http://127.0.0.1:9/unreachable");
        let mut request = LLMRequest {
            model: "grok-4.7".to_string(),
            ..Default::default()
        };
        request.set_extra(json!({ "openai_api_mode": "chat" }));
        let error = provider.invoke(request).await.expect_err("chat refused");
        assert!(matches!(error, LLMError::Configuration(_)), "{error}");
        assert_eq!(provider.provider_kind(), LLMProviderKind::Xai);
    }
}
