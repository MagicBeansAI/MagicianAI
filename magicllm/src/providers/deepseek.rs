use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    error::LLMResult,
    provider::LLMProvider,
    providers::AnthropicMessagesProvider,
    types::{LLMRequest, LLMResponse, StreamDelta},
};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/anthropic/v1/messages";

/// Provider implementation for DeepSeek's Anthropic-compatible Messages API.
pub struct DeepSeekProvider {
    inner: AnthropicMessagesProvider,
}

fn deepseek_model_supports_vision(model: &str) -> bool {
    let model = model.to_ascii_lowercase().replace('_', "-");
    // V4.1 Flash is natively multimodal (`deepseek-flash`). Retired Flash
    // aliases are routed to it. V4 Pro stays text-only until DeepSeek
    // retires the name on 2026-09-14.
    if model.contains("pro") && !model.contains("flash") {
        return false;
    }
    model == "deepseek-flash"
        || model.contains("deepseek-v4.1-flash")
        || model.contains("deepseek-v4-flash")
        || model.contains("vision")
}

impl DeepSeekProvider {
    /// Whether this DeepSeek model accepts image input.
    ///
    /// Public so config validation can gate `supports_vision` on the *model*
    /// rather than on the provider. As of 2026-09-10, `deepseek-flash`
    /// (DeepSeek-V4.1-Flash) is natively multimodal and thinking-capable.
    /// Retired Flash aliases route to it. `deepseek-v4-pro` stays text-only
    /// until DeepSeek retires that name.
    ///
    /// This is the same predicate `capabilities_for_model` uses, so validation
    /// and runtime cannot disagree about which models see images.
    pub fn model_supports_vision(model: &str) -> bool {
        deepseek_model_supports_vision(model)
    }
}

impl DeepSeekProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            inner: AnthropicMessagesProvider::with_base_url_for_provider(
                api_key,
                base_url,
                LLMProviderKind::DeepSeek,
            ),
        }
    }

    pub fn capabilities_for_model(model: &str) -> LLMCapability {
        // V4.1 Flash (`deepseek-flash`) is natively multimodal and
        // thinking-capable (thinking default on; `thinking.type`
        // enabled|disabled). Images only — not video.
        // https://api-docs.deepseek.com/guides/vision
        // https://api-docs.deepseek.com/guides/thinking_mode
        let mut modalities = vec![LLMModality::Text];
        if deepseek_model_supports_vision(model) {
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
}

#[async_trait]
impl LLMProvider for DeepSeekProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::DeepSeek
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        // DeepSeek's chat-completions surface has no server-side web search;
        // fail the flag closed before the inner transport can drop it.
        crate::server_web_search::reject_unsupported("deepseek", &request)?;
        self.inner.invoke(request).await
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        crate::server_web_search::reject_unsupported("deepseek", &request)?;
        self.inner.invoke_stream(request, tx).await
    }

    async fn health_check(&self) -> LLMResult<bool> {
        self.inner.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_v4_pro_remains_text_only_until_retirement() {
        let caps = DeepSeekProvider::capabilities_for_model("deepseek-v4-pro");

        assert_eq!(caps.modalities, vec![LLMModality::Text]);
        assert_eq!(caps.reasoning, LLMReasoning::Standard);
        assert!(caps.tool_calling);
        assert!(caps.json_mode);
        assert!(caps.streaming);
        assert!(!caps.computer_use);
    }

    #[test]
    fn deepseek_v41_flash_advertises_vision_and_thinking() {
        for model in [
            "deepseek-flash",
            "DeepSeek-V4-Flash-0731",
            "deepseek-v4-flash",
            "deepseek-v4-flash-vision-exp",
            "DeepSeek-V4-Flash-Vision-Exp",
        ] {
            let caps = DeepSeekProvider::capabilities_for_model(model);
            assert!(
                caps.modalities.contains(&LLMModality::Text),
                "{model} must keep text"
            );
            assert!(
                caps.modalities.contains(&LLMModality::Vision),
                "{model} is served by V4.1 Flash and must advertise vision"
            );
            assert_eq!(caps.reasoning, LLMReasoning::Standard);
            assert!(caps.tool_calling);
        }
    }

    #[test]
    fn pre_v4_deepseek_models_remain_text_only() {
        let caps = DeepSeekProvider::capabilities_for_model("deepseek-v3-coder");
        assert_eq!(caps.modalities, vec![LLMModality::Text]);
        let caps = DeepSeekProvider::capabilities_for_model("deepseek-chat");
        assert_eq!(caps.modalities, vec![LLMModality::Text]);
    }
}
