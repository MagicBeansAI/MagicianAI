use std::{collections::HashMap, sync::Arc};

use tokio::sync::mpsc;

use crate::{
    capability::LLMProviderKind,
    config::{LLMProfile, LLMRouterConfig},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    providers::{
        AnthropicMessagesProvider, DeepSeekProvider, GeminiProvider, MinimaxProvider,
        OllamaProvider, OpenAIMetaProvider, OpenRouterProvider, SarvamProvider, XaiProvider, YutoriN1Provider,
        DEFAULT_BASE_URL_CHAT, DEFAULT_BASE_URL_RESPONSES,
    },
    router::MultiLLMRouter,
    types::{EmbeddingRequest, EmbeddingResponse, LLMRequest, LLMResponse, StreamDelta},
};

/// A fully configured router with providers registered from profile settings.
pub struct ConfiguredRouter {
    router: MultiLLMRouter,
}

impl ConfiguredRouter {
    /// Build a configured router from a router config.
    pub fn from_router_config(config: LLMRouterConfig) -> LLMResult<Self> {
        let profiles = config.profiles.clone();
        let mut router = MultiLLMRouter::new(config)?;
        register_providers_from_profiles(&mut router, &profiles)?;
        Ok(Self { router })
    }

    /// Access the underlying multi-provider router.
    pub fn router(&self) -> &MultiLLMRouter {
        &self.router
    }

    /// Return true if the resolved profile for this operation has a registered provider.
    pub fn has_provider_for_operation(&self, operation: &str) -> bool {
        self.router
            .profile_name_for_operation(operation)
            .is_some_and(|name| self.router.has_provider_for_profile(&name))
    }

    /// Return the resolved provider kind for an operation.
    pub fn provider_for_operation(&self, operation: &str) -> Option<LLMProviderKind> {
        self.router
            .profile_for_operation(operation)
            .map(|profile| profile.provider.clone())
    }

    /// Route an embedding request through the configured router. Deliberately
    /// skips `route()`'s valid-trace-context requirement: embedding callers
    /// (vector-index) have no scope/trace plumbed, and embedding telemetry
    /// stays caller-side (`llm_embeddings` sink lives in magician).
    pub async fn embed_for_operation(
        &self,
        operation: &str,
        request: EmbeddingRequest,
    ) -> LLMResult<EmbeddingResponse> {
        self.router.embed_for_operation(operation, request).await
    }

    /// Route a request through the configured router.
    pub async fn route(&self, mut request: LLMRequest) -> LLMResult<LLMResponse> {
        let context = request
            .metadata
            .ensure_trace_context(None, crate::trace::LlmWorkloadClass::System);
        if !context.is_valid() {
            return Err(LLMError::Validation(
                "direct LLM routing requires a valid trace context and scope".to_string(),
            ));
        }
        let attempt_counter = request.metadata.ensure_provider_attempt_counter();
        let mut response = self.router.route(request).await?;
        let attempt_count = attempt_counter.load(std::sync::atomic::Ordering::Relaxed);
        response.trace_receipt.get_or_insert_with(|| {
            crate::trace::LlmTraceReceipt::direct_with_attempt_count(context, attempt_count)
        });
        let route_identity = response.route_identity.clone();
        response
            .into_retained_bounded()
            .map_err(|error| match route_identity {
                Some(identity) => {
                    error.with_route(identity.profile, identity.provider, identity.model)
                },
                None => error,
            })
    }

    /// Route a streaming request through the configured router.
    pub async fn route_stream(
        &self,
        mut request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let context = request
            .metadata
            .ensure_trace_context(None, crate::trace::LlmWorkloadClass::System);
        if !context.is_valid() {
            return Err(LLMError::Validation(
                "direct streaming LLM routing requires a valid trace context and scope".to_string(),
            ));
        }
        let attempt_counter = request.metadata.ensure_provider_attempt_counter();
        let (provider_tx, mut provider_rx) = mpsc::channel(32);
        let forward = tokio::spawn(async move {
            while let Some(delta) = provider_rx.recv().await {
                let delta = match delta {
                    StreamDelta::Done(mut response) => {
                        let attempt_count =
                            attempt_counter.load(std::sync::atomic::Ordering::Relaxed);
                        response.trace_receipt =
                            Some(crate::trace::LlmTraceReceipt::direct_with_attempt_count(
                                context.clone(),
                                attempt_count,
                            ));
                        let route_identity = response.route_identity.clone();
                        match response.into_retained_bounded() {
                            Ok(response) => StreamDelta::Done(response),
                            Err(error) => {
                                let error = match route_identity {
                                    Some(identity) => error.with_route(
                                        identity.profile,
                                        identity.provider,
                                        identity.model,
                                    ),
                                    None => error,
                                };
                                let _ = tx.send(StreamDelta::Error(error.to_string())).await;
                                return Err(error);
                            },
                        }
                    },
                    other => other,
                };
                if tx.send(delta).await.is_err() {
                    break;
                }
            }
            Ok::<(), LLMError>(())
        });
        let result = self.router.route_stream(request, provider_tx).await;
        let forward_result = forward.await.map_err(|error| {
            LLMError::Other(format!("direct stream forwarding task failed: {error}"))
        })?;
        result.and(forward_result)
    }
}

fn register_providers_from_profiles(
    router: &mut MultiLLMRouter,
    profiles: &HashMap<String, LLMProfile>,
) -> LLMResult<()> {
    let mut profile_names = profiles.keys().cloned().collect::<Vec<_>>();
    profile_names.sort();
    let mut clients: HashMap<
        (LLMProviderKind, Option<String>, Option<String>),
        Arc<dyn LLMProvider>,
    > = HashMap::new();
    let mut registered = 0usize;
    let mut skipped: Vec<(String, String)> = Vec::new();
    for profile_name in profile_names {
        let profile = profiles
            .get(&profile_name)
            .expect("profile key came from map");
        let connection_key = (
            profile.provider.clone(),
            profile.api_key_env.clone(),
            profile.api_base_url.clone(),
        );
        let provider = if let Some(provider) = clients.get(&connection_key) {
            Arc::clone(provider)
        } else {
            // One profile's missing credential must not cost every other
            // provider its registration. A deployment routinely carries
            // profiles for keys an operator has not supplied, and aborting here
            // took the whole runtime down with "LLM service is not available"
            // even when OpenAI, Anthropic and a key-free Ollama were all ready.
            // Skip the profile, say which one and why, and let the caller judge
            // the result by what actually registered.
            match instantiate_provider(profile) {
                Ok(provider) => {
                    clients.insert(connection_key, Arc::clone(&provider));
                    provider
                },
                Err(error) => {
                    tracing::warn!(
                        target: "magicllm.bootstrap",
                        profile = %profile_name,
                        provider = ?profile.provider,
                        %error,
                        "skipping an LLM profile this deployment cannot instantiate;                          routing continues with the profiles that registered"
                    );
                    skipped.push((profile_name.clone(), error.to_string()));
                    continue;
                },
            }
        };
        router.register_profile_provider(profile_name, provider)?;
        registered += 1;
    }

    // Zero is the real failure: a router with no provider cannot serve any
    // operation, and reporting that as success would defer the error to the
    // first request instead of naming it at boot.
    if registered == 0 {
        let detail = if skipped.is_empty() {
            "the configuration declares no LLM profiles".to_owned()
        } else {
            let reasons = skipped
                .iter()
                .map(|(name, error)| format!("{name}: {error}"))
                .collect::<Vec<_>>()
                .join("; ");
            format!("every declared LLM profile failed to instantiate ({reasons})")
        };
        return Err(LLMError::Configuration(detail));
    }
    if !skipped.is_empty() {
        tracing::warn!(
            target: "magicllm.bootstrap",
            registered,
            skipped = skipped.len(),
            "some LLM profiles were skipped; the router serves the rest"
        );
    }

    Ok(())
}

fn instantiate_provider(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    match profile.provider {
        LLMProviderKind::OpenAI => instantiate_openai(profile),
        LLMProviderKind::Anthropic => instantiate_anthropic(profile),
        LLMProviderKind::Minimax => instantiate_minimax(profile),
        LLMProviderKind::DeepSeek => instantiate_deepseek(profile),
        LLMProviderKind::OpenRouter => instantiate_openrouter(profile),
        LLMProviderKind::Ollama => instantiate_ollama(profile),
        LLMProviderKind::Gemini => instantiate_gemini(profile),
        LLMProviderKind::Yutori => instantiate_yutori(profile),
        LLMProviderKind::Xai => instantiate_xai(profile),
        LLMProviderKind::Sarvam => instantiate_sarvam(profile),
        LLMProviderKind::Custom(ref provider) => instantiate_custom(profile, provider),
    }
}

/// The one planned `Custom` backend: `harness-<engine>` rides an installed
/// CLI subscription as a text-in/text-out model (see the 2026-08-31 plan's
/// no-override guarantee — nothing routes here unless an operation's
/// profile says so explicitly). Everything else remains unimplemented and
/// refuses loudly rather than guessing.
fn instantiate_custom(profile: &LLMProfile, provider: &str) -> LLMResult<Arc<dyn LLMProvider>> {
    if let Some(suffix) = provider.strip_prefix("harness-") {
        let kind = crate::providers::HarnessCliKind::from_profile_suffix(suffix).ok_or_else(
            || {
                let roster = crate::providers::HARNESS_CLI_KINDS
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                LLMError::Configuration(format!(
                    "unknown harness `{suffix}` for provider `{provider}` (profile `{}`); the roster is: {roster}. Note codex_app_server is deliberately not a provider — its differentiator is thread continuity, worthless for stateless model calls.",
                    profile.model
                ))
            },
        )?;
        return Ok(std::sync::Arc::new(
            crate::providers::HarnessCliProvider::new(kind),
        ));
    }
    Err(LLMError::Configuration(format!(
        "Custom provider `{provider}` not implemented yet"
    )))
}

/// Read one provider credential, refusing an absent one and flagging a value
/// that is obviously not a real key.
///
/// The placeholder check exists because a poisoned environment is invisible
/// otherwise: the runtime boots, registers every provider, and then fails at
/// the first request with a 401 from the vendor. Two separate debugging
/// sessions on 2026-09-07 began with `Incorrect API key provided: test-dummy` —
/// the fallback `scripts/run-rust-tests-with-report.sh` exports for the test
/// lane, leaked into the shell that launched the supervisor. `dotenvy` will not
/// overwrite a variable the environment already carries, and it is right not
/// to, so the runtime's own `.env` could not save it.
///
/// This warns rather than refuses. A placeholder is a near-certain
/// misconfiguration, but "looks like a placeholder" is a heuristic, and a
/// heuristic must not be able to take a working deployment down.
fn read_provider_api_key(api_key_env: &str, provider_label: &str) -> LLMResult<String> {
    let api_key = std::env::var(api_key_env).map_err(|_| {
        LLMError::Configuration(format!(
            "Failed to read {provider_label} API key from environment variable `{api_key_env}`"
        ))
    })?;
    if let Some(reason) = placeholder_api_key_reason(&api_key) {
        tracing::warn!(
            target: "magicllm.bootstrap",
            provider = provider_label,
            variable = api_key_env,
            reason,
            "this credential looks like a placeholder, not a real key; the \
             provider will accept the request and the vendor will reject it. A \
             value already in the environment wins over the runtime `.env`, so \
             check the launching shell"
        );
    }
    Ok(api_key)
}

/// Why a credential looks like a placeholder, or `None` if it looks real.
///
/// Deliberately conservative: it matches only shapes no vendor issues, so a
/// real key can never be flagged.
fn placeholder_api_key_reason(api_key: &str) -> Option<&'static str> {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        return Some("empty");
    }
    let lowered = trimmed.to_ascii_lowercase();
    // The exact fallbacks the repo's own test lane exports.
    if lowered == "test-dummy" || lowered == "dummy" || lowered == "test" {
        return Some("a test placeholder");
    }
    // The shape shipped in `.env.example` and vendor docs.
    if lowered.starts_with("sk-your")
        || lowered.starts_with("your-")
        || lowered.starts_with("your_")
        || lowered.starts_with("<")
        || lowered.contains("changeme")
        || lowered.contains("replace-me")
        || lowered.contains("xxxxxxxx")
    {
        return Some("an unfilled template value");
    }
    None
}

fn instantiate_openai(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "OpenAI")?;

    // Use the meta-provider which delegates per-request to Chat Completions or
    // Responses API based on `openai_api_mode` in request.extra (merged from
    // profile metadata), with a compatibility shim for deprecated `use_chat`.
    //
    // When a custom api_base_url is set, determine which provider it targets based on
    // whether the URL contains "/chat/" (Chat Completions) or not (Responses API).
    // The other provider uses its default URL. This prevents misconfiguring both
    // sub-providers with the same endpoint URL.
    let (chat_url, responses_url) = match &profile.api_base_url {
        Some(custom_url) if custom_url.contains("/chat/") => {
            (custom_url.clone(), DEFAULT_BASE_URL_RESPONSES.to_string())
        },
        Some(custom_url) => (DEFAULT_BASE_URL_CHAT.to_string(), custom_url.clone()),
        None => (
            DEFAULT_BASE_URL_CHAT.to_string(),
            DEFAULT_BASE_URL_RESPONSES.to_string(),
        ),
    };

    let provider = OpenAIMetaProvider::new(api_key, chat_url, responses_url);

    Ok(Arc::new(provider))
}

fn instantiate_anthropic(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile
        .api_key_env
        .as_deref()
        .unwrap_or("ANTHROPIC_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "Anthropic")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        AnthropicMessagesProvider::with_base_url(api_key, url)
    } else {
        AnthropicMessagesProvider::new(api_key)
    };
    Ok(Arc::new(provider))
}

fn instantiate_openrouter(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile
        .api_key_env
        .as_deref()
        .unwrap_or("OPENROUTER_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "OpenRouter")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        OpenRouterProvider::with_base_url(api_key, url)
    } else {
        OpenRouterProvider::new(api_key)
    };
    Ok(Arc::new(provider))
}

fn instantiate_minimax(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("MINIMAX_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "MiniMax")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        MinimaxProvider::with_base_url(api_key, url)
    } else {
        MinimaxProvider::new(api_key)
    };
    Ok(Arc::new(provider))
}

fn instantiate_deepseek(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("DEEPSEEK_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "DeepSeek")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        DeepSeekProvider::with_base_url(api_key, url)
    } else {
        DeepSeekProvider::new(api_key)
    };
    Ok(Arc::new(provider))
}

fn instantiate_sarvam(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("SARVAM_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "Sarvam")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        SarvamProvider::with_base_url(api_key, url)
    } else {
        SarvamProvider::new(api_key)
    };
    Ok(Arc::new(provider))
}

/// xAI speaks only its Responses endpoint here, so an `api_base_url` that
/// names Chat Completions is a misconfiguration, refused at boot rather than
/// sent a Responses body it cannot parse.
fn instantiate_xai(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("XAI_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "xAI")?;
    let provider = match profile.api_base_url.clone() {
        Some(url) if url.contains("/chat/") => {
            return Err(LLMError::Configuration(format!(
                "xai profile api_base_url `{url}` names Chat Completions; the xAI provider \
                 speaks the Responses API (`.../v1/responses`)"
            )));
        },
        Some(url) => XaiProvider::with_base_url(api_key, url),
        None => XaiProvider::new(api_key),
    };
    Ok(Arc::new(provider))
}

fn instantiate_ollama(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let provider = if let Some(url) = profile.api_base_url.clone() {
        OllamaProvider::with_base_url(url)
    } else {
        OllamaProvider::new()
    };
    Ok(Arc::new(provider))
}

fn instantiate_gemini(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("GEMINI_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "Gemini")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        GeminiProvider::with_base_url(api_key, url)
    } else {
        GeminiProvider::new(api_key)
    };
    Ok(Arc::new(provider))
}

fn instantiate_yutori(profile: &LLMProfile) -> LLMResult<Arc<dyn LLMProvider>> {
    let api_key_env = profile.api_key_env.as_deref().unwrap_or("YUTORI_API_KEY");
    let api_key = read_provider_api_key(api_key_env, "Yutori")?;

    let provider = if let Some(url) = profile.api_base_url.clone() {
        YutoriN1Provider::with_base_url(api_key, url)
    } else {
        YutoriN1Provider::new(api_key)
    };
    Ok(Arc::new(provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{LLMCapability, LLMModality};
    use crate::config::OperationProfileSelector;
    use crate::types::RequestMetadata;
    use async_trait::async_trait;

    struct DirectTestProvider;

    struct FallbackTestProvider {
        calls: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl LLMProvider for DirectTestProvider {
        fn provider_kind(&self) -> LLMProviderKind {
            LLMProviderKind::Custom("direct-test".to_string())
        }

        fn capabilities(&self, _model: &str) -> LLMCapability {
            LLMCapability {
                modalities: vec![LLMModality::Text],
                ..Default::default()
            }
        }

        async fn invoke(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
            Ok(LLMResponse {
                text: Some(Arc::<str>::from("direct")),
                ..Default::default()
            })
        }

        async fn invoke_stream(
            &self,
            request: LLMRequest,
            tx: mpsc::Sender<StreamDelta>,
        ) -> LLMResult<()> {
            let response = self.invoke(request).await?;
            let _ = tx.send(StreamDelta::Done(response)).await;
            Ok(())
        }
    }

    #[async_trait]
    impl LLMProvider for FallbackTestProvider {
        fn provider_kind(&self) -> LLMProviderKind {
            LLMProviderKind::Custom("fallback-test".to_string())
        }

        fn capabilities(&self, _model: &str) -> LLMCapability {
            LLMCapability {
                modalities: vec![LLMModality::Text],
                ..Default::default()
            }
        }

        async fn invoke(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
            let ordinal = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            if ordinal == 1 {
                return Err(LLMError::Provider {
                    provider: "fallback-test".to_string(),
                    message: "force fallback".to_string(),
                });
            }
            Ok(LLMResponse {
                text: Some(Arc::<str>::from("fallback")),
                ..Default::default()
            })
        }
    }

    /// The heuristic must never flag a real key — a false positive here would
    /// train people to ignore the warning, which is worse than not having it.
    #[test]
    fn placeholder_detection_catches_the_known_shapes_and_nothing_else() {
        for (value, why) in [
            ("test-dummy", "the repo's own test-lane fallback"),
            ("TEST-DUMMY", "case-insensitive"),
            ("  test-dummy  ", "surrounded by whitespace"),
            ("", "empty"),
            ("   ", "whitespace only"),
            ("sk-your-key-here", "the .env.example shape"),
            ("your-api-key", "an unfilled template"),
            ("<paste-key>", "an angle-bracket template"),
            ("changeme", "a changeme marker"),
        ] {
            assert!(
                placeholder_api_key_reason(value).is_some(),
                "{value:?} should be flagged ({why})"
            );
        }

        // Real-shaped credentials from the providers this router speaks to.
        for value in [
            "sk-proj-abc123def456ghi789jkl012mno345pqr678stu901vwx234yz",
            "sk-ant-api03-abc123def456ghi789",
            "AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r",
            "sk-3b3f9a1c2d4e5f6a7b8c9d0e1f2a3b4c",
        ] {
            assert!(
                placeholder_api_key_reason(value).is_none(),
                "{value:?} is a real-shaped key and must not be flagged"
            );
        }
    }

    fn profile_needing(env_var: &str, provider: LLMProviderKind) -> LLMProfile {
        LLMProfile {
            provider,
            model: "test-model".to_string(),
            api_key_env: Some(env_var.to_string()),
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            context_window_tokens: None,
            chunking: None,
            default_modality: Some(LLMModality::Text),
            reasoning: None,
            metadata: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            timeout_secs: None,
        }
    }

    /// One profile whose credential is absent must cost that profile only.
    ///
    /// A deployment routinely declares providers an operator has not supplied
    /// keys for. Aborting registration on the first of them took the whole
    /// runtime down with "LLM service is not available" while other providers —
    /// and a key-free Ollama — were ready to serve.
    #[test]
    fn a_profile_with_no_credential_does_not_cost_the_others_their_registration() {
        let mut profiles = HashMap::new();
        // Ollama needs no credential, so it registers regardless.
        profiles.insert(
            "local".to_string(),
            profile_needing("MAGICLLM_TEST_UNSET_KEY_LOCAL", LLMProviderKind::Ollama),
        );
        // This one cannot instantiate: the variable is deliberately never set.
        profiles.insert(
            "deepseek".to_string(),
            profile_needing(
                "MAGICLLM_TEST_DEFINITELY_UNSET_KEY",
                LLMProviderKind::DeepSeek,
            ),
        );

        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            "default".to_string(),
            OperationProfileSelector::Simple("local".to_string()),
        );
        let config = LLMRouterConfig {
            profiles: profiles.clone(),
            operation_mapping,
            default_profile: "local".to_string(),
            ..Default::default()
        };
        let mut router = MultiLLMRouter::new(config).expect("test router");
        register_providers_from_profiles(&mut router, &profiles)
            .expect("a partially credentialed deployment still registers");
        assert!(
            router.has_provider_for_profile("local"),
            "the key-free profile must survive its neighbour's missing credential"
        );
        assert!(
            !router.has_provider_for_profile("deepseek"),
            "the uninstantiable profile is skipped, not faked"
        );
    }

    /// Zero registrations is still a hard failure, named at boot.
    #[test]
    fn a_deployment_where_no_profile_can_instantiate_still_fails() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "deepseek".to_string(),
            profile_needing(
                "MAGICLLM_TEST_DEFINITELY_UNSET_KEY",
                LLMProviderKind::DeepSeek,
            ),
        );
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            "default".to_string(),
            OperationProfileSelector::Simple("deepseek".to_string()),
        );
        let config = LLMRouterConfig {
            profiles: profiles.clone(),
            operation_mapping,
            default_profile: "deepseek".to_string(),
            ..Default::default()
        };
        let mut router = MultiLLMRouter::new(config).expect("test router");
        let error = register_providers_from_profiles(&mut router, &profiles)
            .expect_err("a router with no provider cannot serve any operation");
        let rendered = error.to_string();
        assert!(
            rendered.contains("deepseek"),
            "the failure must name the profile that failed: {rendered}"
        );
    }

    fn direct_test_router() -> ConfiguredRouter {
        let provider_kind = LLMProviderKind::Custom("direct-test".to_string());
        let mut profiles = HashMap::new();
        profiles.insert(
            "direct".to_string(),
            LLMProfile {
                provider: provider_kind.clone(),
                model: "test-model".to_string(),
                api_key_env: None,
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                context_window_tokens: None,
                chunking: None,
                default_modality: Some(LLMModality::Text),
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
            },
        );
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            "default".to_string(),
            OperationProfileSelector::Simple("direct".to_string()),
        );
        let config = LLMRouterConfig {
            profiles,
            operation_mapping,
            default_profile: "direct".to_string(),
            ..Default::default()
        };
        let mut router = MultiLLMRouter::new(config).expect("test router");
        router.register_provider(Arc::new(DirectTestProvider));
        ConfiguredRouter { router }
    }

    fn traced_request() -> (LLMRequest, crate::trace::LlmTraceContext) {
        let context = crate::trace::LlmTraceContext::new(
            crate::trace::LlmScope::new("principal", "workspace"),
            crate::trace::LlmWorkloadClass::ForegroundChat,
        );
        let mut metadata = RequestMetadata {
            operation: "default".to_string(),
            ..Default::default()
        };
        metadata.set_trace_context(context.clone());
        (
            LLMRequest {
                model: "test-model".to_string(),
                metadata,
                ..Default::default()
            },
            context,
        )
    }

    #[tokio::test]
    async fn direct_route_returns_the_supplied_logical_call_identity() {
        let router = direct_test_router();
        let (request, context) = traced_request();
        let response = router.route(request).await.expect("direct response");
        let receipt = response.trace_receipt.expect("trace receipt");

        assert_eq!(receipt.context, context);
        assert_eq!(receipt.provider_attempt_count, 1);
        assert!(receipt.dispatch_job_id.is_none());
    }

    #[test]
    fn provider_availability_uses_the_resolved_concrete_profile() {
        let router = direct_test_router();

        assert!(router.has_provider_for_operation("default"));
        // Unmapped operations intentionally resolve through the configured
        // default profile and therefore share its provider availability.
        assert!(router.has_provider_for_operation("unmapped-operation"));
    }

    #[tokio::test]
    async fn direct_stream_done_returns_the_supplied_logical_call_identity() {
        let router = direct_test_router();
        let (request, context) = traced_request();
        let (tx, mut rx) = mpsc::channel(4);
        router
            .route_stream(request, tx)
            .await
            .expect("stream route");
        let StreamDelta::Done(response) = rx.recv().await.expect("done delta") else {
            panic!("expected done delta");
        };
        let receipt = response.trace_receipt.expect("trace receipt");

        assert_eq!(receipt.context, context);
        assert_eq!(receipt.provider_attempt_count, 1);
        assert!(receipt.dispatch_job_id.is_none());
    }

    #[tokio::test]
    async fn direct_routes_reject_a_malformed_supplied_trace_context_before_provider_use() {
        let router = direct_test_router();
        let (mut request, _) = traced_request();
        request
            .metadata
            .trace_context
            .as_mut()
            .expect("trace context")
            .scope
            .principal = "owner/team".to_string();

        let error = router
            .route(request.clone())
            .await
            .expect_err("invalid direct trace must fail closed");
        assert!(matches!(error, LLMError::Validation(_)));

        let (tx, _rx) = mpsc::channel(4);
        let error = router
            .route_stream(request, tx)
            .await
            .expect_err("invalid streaming trace must fail closed");
        assert!(matches!(error, LLMError::Validation(_)));
    }

    #[tokio::test]
    async fn direct_route_counts_each_physical_fallback_invocation() {
        let provider_kind = LLMProviderKind::Custom("fallback-test".to_string());
        let fallback_metadata = HashMap::from([(
            "fallback_profile".to_string(),
            serde_json::Value::String("fallback".to_string()),
        )]);
        let profile =
            |model: &str, metadata: Option<HashMap<String, serde_json::Value>>| LLMProfile {
                provider: provider_kind.clone(),
                model: model.to_string(),
                api_key_env: None,
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                context_window_tokens: None,
                chunking: None,
                default_modality: Some(LLMModality::Text),
                reasoning: None,
                metadata,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
            };
        let config = LLMRouterConfig {
            profiles: HashMap::from([
                (
                    "primary".to_string(),
                    profile("primary-model", Some(fallback_metadata)),
                ),
                ("fallback".to_string(), profile("fallback-model", None)),
            ]),
            operation_mapping: HashMap::from([(
                "default".to_string(),
                OperationProfileSelector::Simple("primary".to_string()),
            )]),
            default_profile: "primary".to_string(),
            ..Default::default()
        };
        let mut inner = MultiLLMRouter::new(config).expect("fallback router");
        inner.register_provider(Arc::new(FallbackTestProvider {
            calls: std::sync::atomic::AtomicU32::new(0),
        }));
        let router = ConfiguredRouter { router: inner };
        let (request, context) = traced_request();

        let response = router.route(request).await.expect("fallback response");
        let route_identity = response
            .route_identity
            .as_ref()
            .expect("fallback success must expose the route that actually ran");
        assert_eq!(route_identity.profile, "fallback");
        assert_eq!(route_identity.provider, provider_kind);
        assert_eq!(route_identity.model, "fallback-model");
        let receipt = response.trace_receipt.expect("trace receipt");

        assert_eq!(receipt.context, context);
        assert_eq!(receipt.provider_attempt_count, 2);
        assert_eq!(
            receipt.provider_attempt_id.as_deref(),
            Some(context.provider_attempt_id(2).as_str())
        );
    }
}
