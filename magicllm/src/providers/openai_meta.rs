use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    providers::{OpenAIChatProvider, OpenAIResponsesProvider},
    types::{ContentBlock, LLMRequest, LLMResponse, StreamDelta},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIApiMode {
    Auto,
    Chat,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIApiSelection {
    Chat,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIApiModeSource {
    OpenAIApiMode,
    LegacyUseChat,
    LegacyDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAIApiModeResolution {
    pub requested_mode: OpenAIApiMode,
    pub selected_api: OpenAIApiSelection,
    pub source: OpenAIApiModeSource,
}

/// Meta-provider that delegates each request to either the Chat Completions or
/// Responses API provider based on request policy.
pub struct OpenAIMetaProvider {
    chat: OpenAIChatProvider,
    responses: OpenAIResponsesProvider,
}

impl OpenAIMetaProvider {
    pub fn new(
        api_key: impl Into<String>,
        chat_url: impl Into<String>,
        responses_url: impl Into<String>,
    ) -> Self {
        let key: String = api_key.into();
        Self {
            chat: OpenAIChatProvider::with_base_url(key.clone(), chat_url),
            responses: OpenAIResponsesProvider::with_base_url(key, responses_url),
        }
    }
}

pub fn resolve_openai_api_mode(request: &LLMRequest) -> OpenAIApiModeResolution {
    let extra = request.extra.as_deref().and_then(Value::as_object);

    if let Some(mode) = extra
        .and_then(|map| map.get("openai_api_mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let requested_mode = match mode.to_ascii_lowercase().as_str() {
            "auto" => OpenAIApiMode::Auto,
            "chat" => OpenAIApiMode::Chat,
            "responses" => OpenAIApiMode::Responses,
            invalid => {
                warn!(
                    provider = "openai_meta",
                    operation = %request.metadata.operation,
                    invalid_mode = invalid,
                    "invalid openai_api_mode; falling back to legacy default"
                );
                return OpenAIApiModeResolution {
                    requested_mode: OpenAIApiMode::Responses,
                    selected_api: OpenAIApiSelection::Responses,
                    source: OpenAIApiModeSource::LegacyDefault,
                };
            },
        };

        if extra.and_then(|map| map.get("use_chat")).is_some() {
            warn!(
                provider = "openai_meta",
                operation = %request.metadata.operation,
                "openai_api_mode takes precedence over deprecated use_chat"
            );
        }

        return OpenAIApiModeResolution {
            requested_mode,
            selected_api: match requested_mode {
                OpenAIApiMode::Auto => select_openai_api_for_auto(request),
                OpenAIApiMode::Chat => OpenAIApiSelection::Chat,
                OpenAIApiMode::Responses => OpenAIApiSelection::Responses,
            },
            source: OpenAIApiModeSource::OpenAIApiMode,
        };
    }

    if let Some(use_chat) = extra
        .and_then(|map| map.get("use_chat"))
        .and_then(Value::as_bool)
    {
        warn!(
            provider = "openai_meta",
            operation = %request.metadata.operation,
            "use_chat is deprecated; prefer openai_api_mode=chat|responses|auto"
        );
        return OpenAIApiModeResolution {
            requested_mode: if use_chat {
                OpenAIApiMode::Chat
            } else {
                OpenAIApiMode::Responses
            },
            selected_api: if use_chat {
                OpenAIApiSelection::Chat
            } else {
                OpenAIApiSelection::Responses
            },
            source: OpenAIApiModeSource::LegacyUseChat,
        };
    }

    OpenAIApiModeResolution {
        requested_mode: OpenAIApiMode::Responses,
        selected_api: OpenAIApiSelection::Responses,
        source: OpenAIApiModeSource::LegacyDefault,
    }
}

pub fn request_requires_openai_responses(request: &LLMRequest) -> bool {
    request.modality != LLMModality::Text
        || request.media.is_some()
        || request
            .input_media
            .as_ref()
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        || request
            .extra
            .as_deref()
            .and_then(Value::as_object)
            .and_then(|extra| extra.get("openai_previous_response_id"))
            .and_then(Value::as_str)
            .is_some()
        || (request.metadata.operation == "chat_completion" && !request.tools.is_empty())
        || request_messages_require_openai_responses(request)
}

pub fn request_prefers_openai_responses(request: &LLMRequest) -> bool {
    request.stream || request_requires_openai_responses(request)
}

fn request_messages_require_openai_responses(request: &LLMRequest) -> bool {
    request.messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. }
            )
        })
    })
}

fn select_openai_api_for_auto(request: &LLMRequest) -> OpenAIApiSelection {
    if request_prefers_openai_responses(request) {
        OpenAIApiSelection::Responses
    } else {
        OpenAIApiSelection::Chat
    }
}

#[async_trait]
impl LLMProvider for OpenAIMetaProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::OpenAI
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        // Request-sensitive capability resolution happens in the router. The
        // provider-level default stays permissive here so existing non-router
        // callers continue to see the Responses superset.
        self.responses.capabilities(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let resolution = resolve_openai_api_mode(&request);

        if matches!(resolution.selected_api, OpenAIApiSelection::Chat)
            && request_requires_openai_responses(&request)
        {
            return Err(LLMError::UnsupportedCapability(
                "OpenAI Chat mode cannot satisfy this request; use openai_api_mode=responses or auto"
                    .to_string(),
            ));
        }

        match resolution.selected_api {
            OpenAIApiSelection::Chat => {
                debug!(
                    provider = "openai_meta",
                    model = %request.model,
                    operation = %request.metadata.operation,
                    requested_mode = ?resolution.requested_mode,
                    source = ?resolution.source,
                    "delegating to Chat Completions"
                );
                self.chat.invoke(request).await
            },
            OpenAIApiSelection::Responses => {
                debug!(
                    provider = "openai_meta",
                    model = %request.model,
                    operation = %request.metadata.operation,
                    requested_mode = ?resolution.requested_mode,
                    source = ?resolution.source,
                    "delegating to Responses API"
                );
                self.responses.invoke(request).await
            },
        }
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let resolution = resolve_openai_api_mode(&request);

        if matches!(resolution.selected_api, OpenAIApiSelection::Chat)
            && request_requires_openai_responses(&request)
        {
            return Err(LLMError::UnsupportedCapability(
                "OpenAI Chat mode cannot satisfy this request; use openai_api_mode=responses or auto"
                    .to_string(),
            ));
        }

        match resolution.selected_api {
            OpenAIApiSelection::Chat => {
                debug!(
                    provider = "openai_meta",
                    model = %request.model,
                    operation = %request.metadata.operation,
                    requested_mode = ?resolution.requested_mode,
                    source = ?resolution.source,
                    "delegating stream to Chat Completions"
                );
                self.chat.invoke_stream(request, tx).await
            },
            OpenAIApiSelection::Responses => {
                debug!(
                    provider = "openai_meta",
                    model = %request.model,
                    operation = %request.metadata.operation,
                    requested_mode = ?resolution.requested_mode,
                    source = ?resolution.source,
                    "delegating stream to Responses API"
                );
                self.responses.invoke_stream(request, tx).await
            },
        }
    }

    async fn health_check(&self) -> LLMResult<bool> {
        self.responses.health_check().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, LLMMessage, LLMRequest, MessageRole};
    use serde_json::json;

    #[test]
    fn routes_to_chat_when_openai_api_mode_chat() {
        let mut request = LLMRequest::default();
        request.extra = Some(json!({"openai_api_mode": "chat"}).into());
        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.requested_mode, OpenAIApiMode::Chat);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Chat);
        assert_eq!(resolution.source, OpenAIApiModeSource::OpenAIApiMode);
    }

    #[test]
    fn routes_to_chat_for_auto_text_only_requests() {
        let mut request = LLMRequest::default();
        request.extra = Some(json!({"openai_api_mode": "auto"}).into());
        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.requested_mode, OpenAIApiMode::Auto);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Chat);
    }

    #[test]
    fn routes_to_responses_for_auto_streaming_requests() {
        let mut request = LLMRequest::default();
        request.stream = true;
        request.extra = Some(json!({"openai_api_mode": "auto"}).into());
        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Responses);
    }

    #[test]
    fn routes_to_responses_for_auto_requests_with_images() {
        let mut request = LLMRequest::default();
        request.extra = Some(json!({"openai_api_mode": "auto"}).into());
        request.messages = vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::ImageUrl {
                url: "https://example.com/screenshot.png".to_string(),
                prompt: None,
            }],
        }]
        .into();

        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Responses);
        assert!(request_requires_openai_responses(&request));
    }

    #[test]
    fn legacy_use_chat_true_routes_to_chat() {
        let mut request = LLMRequest::default();
        request.extra = Some(json!({"use_chat": true}).into());
        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.requested_mode, OpenAIApiMode::Chat);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Chat);
        assert_eq!(resolution.source, OpenAIApiModeSource::LegacyUseChat);
    }

    #[test]
    fn legacy_default_routes_to_responses() {
        let request = LLMRequest::default();
        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.requested_mode, OpenAIApiMode::Responses);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Responses);
        assert_eq!(resolution.source, OpenAIApiModeSource::LegacyDefault);
    }

    #[test]
    fn openai_api_mode_takes_precedence_over_use_chat() {
        let mut request = LLMRequest::default();
        request.extra = Some(
            json!({
                "openai_api_mode": "responses",
                "use_chat": true
            })
            .into(),
        );
        let resolution = resolve_openai_api_mode(&request);
        assert_eq!(resolution.requested_mode, OpenAIApiMode::Responses);
        assert_eq!(resolution.selected_api, OpenAIApiSelection::Responses);
        assert_eq!(resolution.source, OpenAIApiModeSource::OpenAIApiMode);
    }
}
