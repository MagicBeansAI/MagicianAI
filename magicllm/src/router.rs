use std::{
    collections::{HashMap, HashSet},
    io,
    sync::Arc,
};

use chrono::Utc;
use tokio::sync::mpsc;

use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    chunking::{preflight_request, ConservativeOllamaEstimator, ContextPreflight},
    config::{LLMProfile, LLMRouterConfig, RequestShape},
    context_reuse::{
        stable_prefix_fingerprint, strategy_for_provider, transport_cohort_fingerprint,
        ContextReuseStrategy,
    },
    error::{LLMError, LLMResult},
    pricing::{active_table, compute_cost_with_server_web_search_at, usd_to_microusd_ceil},
    provider::LLMProvider,
    providers::{
        openai_meta::{resolve_openai_api_mode, OpenAIApiSelection},
        OpenAIChatProvider, OpenAIResponsesProvider,
    },
    types::{
        clone_json_value_iteratively, EmbeddingRequest, EmbeddingResponse, LLMRequest, LLMResponse,
        LLMResponseFormat, LlmPhysicalAttemptObservation, LlmPhysicalAttemptPlan, LlmRouteIdentity,
        ReasoningConfig, StreamDelta,
    },
};
use serde_json::Value;
use tracing::{debug, error, info, warn};

/// Routes `LLMRequest`s to registered providers based on operation profiles.
pub struct MultiLLMRouter {
    config: LLMRouterConfig,
    /// Provider clients bound to their exact profile. Production bootstrap
    /// uses this map so per-profile endpoints and credential env names cannot
    /// be selected by `HashMap` iteration order.
    profile_providers: HashMap<String, Arc<dyn LLMProvider>>,
    /// Kind-wide providers retained for embedders/tests that register one
    /// provider implementation for every profile of a kind.
    providers: HashMap<LLMProviderKind, Arc<dyn LLMProvider>>,
}

fn with_last_attempted_route(error: LLMError, route: Option<&LlmRouteIdentity>) -> LLMError {
    let Some(route) = route else {
        return error;
    };
    error.with_route(
        route.profile.clone(),
        route.provider.clone(),
        route.model.clone(),
    )
}

impl MultiLLMRouter {
    /// Creates a router with the supplied configuration.
    pub fn new(config: LLMRouterConfig) -> LLMResult<Self> {
        validate_config(&config)?;
        Ok(Self {
            config,
            profile_providers: HashMap::new(),
            providers: HashMap::new(),
        })
    }

    /// Registers a provider implementation. Existing entries with the same key are replaced.
    pub fn register_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        let kind = provider.provider_kind();
        self.providers.insert(kind, provider);
    }

    /// Registers a provider client for one concrete profile. The provider kind
    /// must agree with the profile contract; routing never falls back to a
    /// differently configured client merely because its kind matches.
    pub fn register_profile_provider(
        &mut self,
        profile_name: impl Into<String>,
        provider: Arc<dyn LLMProvider>,
    ) -> LLMResult<()> {
        let profile_name = profile_name.into();
        let profile = self.config.profiles.get(&profile_name).ok_or_else(|| {
            LLMError::Configuration(format!(
                "cannot register provider for unknown profile `{profile_name}`"
            ))
        })?;
        if provider.provider_kind() != profile.provider {
            return Err(LLMError::Configuration(format!(
                "provider kind `{}` does not match profile `{profile_name}` kind `{}`",
                provider.provider_kind(),
                profile.provider
            )));
        }
        self.profile_providers.insert(profile_name, provider);
        Ok(())
    }

    /// Returns `true` when the router has a provider registered for the supplied kind.
    pub fn has_provider(&self, kind: &LLMProviderKind) -> bool {
        self.providers.contains_key(kind)
            || self
                .profile_providers
                .values()
                .any(|provider| provider.provider_kind() == *kind)
    }

    /// Returns true when the concrete profile has a usable provider client.
    pub fn has_provider_for_profile(&self, profile_name: &str) -> bool {
        self.profile_providers.contains_key(profile_name)
            || self
                .config
                .profiles
                .get(profile_name)
                .is_some_and(|profile| self.providers.contains_key(&profile.provider))
    }

    /// Returns the profile configured for the provided operation (or the default profile).
    pub fn profile_for_operation(&self, operation: &str) -> Option<&LLMProfile> {
        self.config.get_profile_for_operation(operation)
    }

    /// Embedding entry point beside generation routing. Resolves the
    /// explicitly-bound profile for `operation` and calls its registered
    /// provider's `embed` directly — deliberately skipping the
    /// generation-only machinery (chunking, tool-capability checks, model
    /// fallback), which does not apply to embeddings.
    ///
    /// Unlike generation routing there is no default-profile fallback: an
    /// embedding operation that is not explicitly mapped is a configuration
    /// error, not a request for the generation default profile (which would
    /// route vectors at a chat model). The profile's model is authoritative —
    /// it is what makes an embedding provider swap a config edit.
    pub async fn embed_for_operation(
        &self,
        operation: &str,
        mut request: EmbeddingRequest,
    ) -> LLMResult<EmbeddingResponse> {
        if !self.config.operation_mapping.contains_key(operation) {
            return Err(LLMError::Configuration(format!(
                "embedding operation `{operation}` has no explicit profile binding; \
                 map it in llm.router.operation_mapping"
            )));
        }
        let profile_name = self.profile_name_for_operation(operation).ok_or_else(|| {
            LLMError::Configuration(format!(
                "embedding operation `{operation}` resolves to unknown profile"
            ))
        })?;
        let profile = self.config.profiles.get(&profile_name).ok_or_else(|| {
            LLMError::Configuration(format!("unknown embedding profile `{profile_name}`"))
        })?;
        request.model = profile.model.clone();
        let provider = self
            .profile_providers
            .get(&profile_name)
            .or_else(|| self.providers.get(&profile.provider))
            .ok_or_else(|| {
                LLMError::Configuration(format!(
                    "no provider registered for embedding profile `{profile_name}`"
                ))
            })?;
        provider.embed(request).await
    }

    /// Health probe against the provider bound to an explicitly-mapped
    /// operation (same resolution rules as [`Self::embed_for_operation`]).
    pub async fn health_check_for_operation(&self, operation: &str) -> LLMResult<bool> {
        if !self.config.operation_mapping.contains_key(operation) {
            return Err(LLMError::Configuration(format!(
                "operation `{operation}` has no explicit profile binding; \
                 map it in llm.router.operation_mapping"
            )));
        }
        let profile_name = self.profile_name_for_operation(operation).ok_or_else(|| {
            LLMError::Configuration(format!(
                "operation `{operation}` resolves to unknown profile"
            ))
        })?;
        let profile =
            self.config.profiles.get(&profile_name).ok_or_else(|| {
                LLMError::Configuration(format!("unknown profile `{profile_name}`"))
            })?;
        let provider = self
            .profile_providers
            .get(&profile_name)
            .or_else(|| self.providers.get(&profile.provider))
            .ok_or_else(|| {
                LLMError::Configuration(format!(
                    "no provider registered for profile `{profile_name}`"
                ))
            })?;
        provider.health_check().await
    }

    /// Returns the concrete profile name selected for an operation, resolving
    /// adaptive composites to the transport-level fast profile.
    pub fn profile_name_for_operation(&self, operation: &str) -> Option<String> {
        let selected = self
            .config
            .operation_mapping
            .get(operation)
            .map(|selector| {
                selector.profile_for_locality(&RequestShape::NONE, self.config.locality)
            })
            .map(str::to_string)
            .unwrap_or_else(|| self.config.default_profile.clone());
        let selected = resolve_adaptive_to_fast(&self.config, &selected);
        self.config
            .profiles
            .contains_key(&selected)
            .then_some(selected)
    }

    /// Returns the concrete profile that routing will use for this request.
    ///
    /// Unlike [`Self::profile_for_operation`], this honours the router's
    /// per-request profile/provider overrides. Dispatch workers use this view
    /// so provider concurrency, cooldown, and timeout gates describe the same
    /// profile that the transport will actually call.
    pub fn profile_for_request(&self, request: &LLMRequest) -> Option<&LLMProfile> {
        if let Some(profile_name) = router_profile_override(request.extra_value()) {
            let profile_name = resolve_adaptive_to_fast(&self.config, &profile_name);
            return self.config.profiles.get(profile_name.as_str());
        }

        if let Some(provider) = router_provider_override_kind(request.extra_value()) {
            let operation = if request.metadata.operation.is_empty() {
                "default"
            } else {
                request.metadata.operation.as_str()
            };
            let current_profile = self
                .config
                .operation_mapping
                .get(operation)
                .map(|selector| {
                    selector
                        .profile_for_locality(&RequestShape::NONE, self.config.locality)
                        .to_string()
                })
                .unwrap_or_else(|| self.config.default_profile.clone());
            let current_profile = resolve_adaptive_to_fast(&self.config, &current_profile);
            let preferred_model = router_preserve_request_model(request.extra_value())
                .then(|| request.model.trim())
                .filter(|model| !model.is_empty());
            let profile_name = select_profile_for_provider(
                &self.config,
                &provider,
                preferred_model,
                Some(&current_profile),
            )?;
            return self.config.profiles.get(profile_name.as_str());
        }

        self.profile_for_operation(&request.metadata.operation)
    }

    /// Returns the list of configured operations.
    pub fn operations(&self) -> Vec<String> {
        self.config.operation_mapping.keys().cloned().collect()
    }

    /// Routes the request to the appropriate provider, applying profile defaults and capability checks.
    pub async fn route(&self, mut request: LLMRequest) -> LLMResult<LLMResponse> {
        if !request.json_payloads_are_bounded() {
            request.discard_json_payloads_iteratively();
            return Err(LLMError::Validation(
                "LLM request JSON exceeds the admitted depth/node ceiling".to_string(),
            ));
        }
        request.metadata.enforce_disclosure_capture_policy();
        let operation = if request.metadata.operation.is_empty() {
            "default".to_string()
        } else {
            request.metadata.operation.clone()
        };

        if request.metadata.operation.is_empty() {
            request.metadata.operation = operation.clone();
        }
        request.metadata.ensure_provider_attempt_counter();
        if !request.metadata.logical_content_capture_emitted {
            request.metadata.content_capture_sink.observe(
                crate::types::LlmContentCaptureEvent::LogicalRequest { request: &request },
            );
            request.metadata.logical_content_capture_emitted = true;
        }

        let trace_id = request.metadata.trace_id.clone();
        // Pre-dispatch breadcrumb. Every routed call also emits the terminal
        // `llm_call_completed` INFO line (or an ERROR on failure), so logging the
        // intent as well doubles INFO volume for no extra signal — keep at DEBUG.
        debug!(
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            stream = request.stream,
            "routing LLM request"
        );

        // The request is owned by this route call. Keep one immutable base
        // envelope and let fallback hops clone only its Arc-backed payload
        // lanes; cloning the full bootstrap here used to duplicate every
        // message, tool schema and inline-media buffer before the first hop.
        let base_request = request;
        let disclosure_bound = base_request.metadata.disclosure_guard().is_some();
        let requested_profile = router_profile_override(base_request.extra_value());
        let requested_provider_override = router_provider_override_kind(base_request.extra_value());
        let required_provider_kind = router_required_provider_kind(base_request.extra_value());
        let preserve_request_model = router_preserve_request_model(base_request.extra_value());
        let preferred_model = if preserve_request_model {
            let trimmed = base_request.model.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        } else {
            None
        };
        let locked_profile = requested_profile.clone();
        let mut locked_provider = requested_provider_override.clone();
        // The magicllm router is the low-level transport dispatcher and
        // does not see the request shape (e.g. `has_images`); it always
        // resolves to the operation's *unconditional* default profile.
        // Shape-aware selection — the per-call alternative used by the
        // outer/inner loop image swap — lives in the magician layer's
        // `operation_llm_router` which knows the request and computes
        // `has_images` before dispatch.
        let mut current_profile = self
            .config
            .operation_mapping
            .get(&operation)
            .map(|selector| {
                selector
                    .profile_for_locality(&RequestShape::NONE, self.config.locality)
                    .to_string()
            })
            .unwrap_or_else(|| self.config.default_profile.clone());
        // Adaptive composites at the operation_mapping or
        // default_profile layer transparently resolve to the
        // composite's `fast_profile` here. The chat-side runtime is the
        // adaptive-aware caller and pre-resolves to a concrete
        // standard profile before dispatch; non-adaptive callers
        // (autonomous loops, inner-loop, capability probes) see the
        // fast variant and never need to know about the composite.
        current_profile = resolve_adaptive_to_fast(&self.config, &current_profile);
        if let Some(profile_name) = requested_profile {
            // Allow `router_profile_override` itself to name an
            // adaptive composite — useful for callers that pass the
            // user-selected profile directly without resolving first.
            let profile_name = resolve_adaptive_to_fast(&self.config, &profile_name);
            if !self.config.profiles.contains_key(profile_name.as_str()) {
                return Err(LLMError::Configuration(format!(
                    "requested profile `{}` is not configured",
                    profile_name
                )));
            }
            if let Some(profile) = self.config.profiles.get(profile_name.as_str()) {
                locked_provider = Some(profile.provider.clone());
            }
            current_profile = profile_name;
        } else if let Some(provider) = requested_provider_override.as_ref() {
            current_profile = select_profile_for_provider(
                &self.config,
                provider,
                preferred_model.as_deref(),
                Some(&current_profile),
            )
            .ok_or_else(|| {
                LLMError::Configuration(format!(
                    "no profile available for requested provider `{}`",
                    provider
                ))
            })?;
        }

        let mut visited = HashSet::new();
        // When a provider fails and fallback selection itself later fails
        // (capability/preflight/config), the last physical attempt is still
        // known. Preserve it instead of returning a bare router error that
        // makes downstream accounting discard the terminal attempt.
        let mut last_attempted_route: Option<LlmRouteIdentity> = None;

        loop {
            let is_fallback_hop = !visited.is_empty();
            if !visited.insert(current_profile.clone()) {
                error!(
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    profile = %current_profile,
                    "detected fallback loop while routing request"
                );
                return Err(with_last_attempted_route(
                    LLMError::Configuration(format!(
                        "fallback loop detected while routing profile `{}`",
                        current_profile
                    )),
                    last_attempted_route.as_ref(),
                ));
            }

            let profile = match self.config.profiles.get(&current_profile) {
                Some(profile) => profile,
                None => {
                    error!(
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        profile = %current_profile,
                        "profile missing from router configuration"
                    );
                    return Err(with_last_attempted_route(
                        LLMError::Configuration(format!(
                            "profile `{}` not found in router configuration",
                            current_profile
                        )),
                        last_attempted_route.as_ref(),
                    ));
                },
            };

            let provider_kind = profile.provider.clone();
            // Hard dispatch-time provider lock (`router_required_provider_kind`):
            // enforced against THIS router's own immutable config snapshot on
            // every hop of the routing loop (initial profile AND any fallback
            // hop). This is what makes a caller-side guard's verdict binding —
            // even if the profile the caller verified was redefined by a
            // config hot-reload between its check and this dispatch, the
            // request cannot reach a provider of a different kind.
            if let Some(required) = required_provider_kind.as_ref() {
                if &provider_kind != required {
                    error!(
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        profile = %current_profile,
                        provider = %provider_kind,
                        required = %required,
                        "profile violates required provider kind lock"
                    );
                    return Err(with_last_attempted_route(
                        LLMError::Configuration(format!(
                            "profile `{}` resolves to provider `{}` but the request requires \
                             provider kind `{}` (provider lock)",
                            current_profile, provider_kind, required
                        )),
                        last_attempted_route.as_ref(),
                    ));
                }
            }
            let provider = match self
                .profile_providers
                .get(&current_profile)
                .or_else(|| self.providers.get(&provider_kind))
            {
                Some(provider) => provider,
                None => {
                    error!(
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        profile = %current_profile,
                        provider = %provider_kind,
                        "provider not registered with router"
                    );
                    return Err(with_last_attempted_route(
                        LLMError::Configuration(format!(
                            "provider `{}` not registered with router",
                            profile.provider
                        )),
                        last_attempted_route.as_ref(),
                    ));
                },
            };

            debug!(
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                profile = %current_profile,
                provider = %provider_kind,
                "selected routing profile"
            );

            let mut adjusted_request = base_request.clone();
            apply_profile_defaults(profile, &mut adjusted_request, preserve_request_model);
            enforce_disclosure_route(&current_profile, profile, &adjusted_request)
                .await
                .map_err(|error| with_last_attempted_route(error, last_attempted_route.as_ref()))?;
            normalize_context_reuse_for_profile(profile, &mut adjusted_request, is_fallback_hop);
            strip_router_only_extra(&mut adjusted_request);
            let physical_resource_bound = adjusted_request
                .metadata
                .disclosure_guard()
                .is_some_and(|guard| guard.requires_physical_resource_authority());
            if physical_resource_bound {
                // The app trust contract currently admits only
                // no-provider-storage profiles. Normalize the provider-state
                // lanes before the final bounded-request preflight so the
                // exact digested request and its JSON-admission authority
                // describe the same cold physical payload.
                adjusted_request.prompt_cache = Some(crate::types::PromptCacheConfig::Disabled);
                strip_protected_provider_state(&mut adjusted_request);
                adjusted_request.metadata.single_physical_attempt = true;
            }
            if !adjusted_request.json_payloads_are_bounded() {
                adjusted_request.discard_json_payloads_iteratively();
                return Err(with_last_attempted_route(
                    LLMError::Validation(
                        "effective LLM request JSON exceeds the admitted depth/node ceiling"
                            .to_string(),
                    ),
                    last_attempted_route.as_ref(),
                ));
            }
            let context_preflight =
                preflight_profile_request(&current_profile, profile, &adjusted_request).map_err(
                    |error| with_last_attempted_route(error, last_attempted_route.as_ref()),
                )?;

            debug!(
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                profile = %current_profile,
                provider = %provider_kind,
                model = %adjusted_request.model,
                stream = adjusted_request.stream,
                "prepared provider request"
            );

            let model_name = adjusted_request.model.clone();
            let capabilities = effective_capabilities_for_request(
                profile,
                &adjusted_request,
                provider.capabilities(&adjusted_request.model),
            );
            if let Err(err) = enforce_capabilities(&capabilities, &adjusted_request) {
                error!(
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    profile = %current_profile,
                    provider = %provider_kind,
                    model = %model_name,
                    error = ?err,
                    "provider capabilities do not satisfy request"
                );
                return Err(with_last_attempted_route(
                    err,
                    last_attempted_route.as_ref(),
                ));
            }

            if physical_resource_bound {
                if adjusted_request.stream {
                    return Err(with_last_attempted_route(
                        LLMError::Validation(
                            "app workflow LLM streaming requires a dedicated physical resource contract"
                                .to_owned(),
                        ),
                        last_attempted_route.as_ref(),
                    ));
                }
                if is_fallback_hop
                    || profile
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("fallback_profile"))
                        .is_some()
                    || adjusted_request.context_reuse.is_some()
                {
                    return Err(with_last_attempted_route(
                        LLMError::Validation(
                            "app workflow LLM dispatch requires one physical attempt without fallback or reuse"
                                .to_owned(),
                        ),
                        last_attempted_route.as_ref(),
                    ));
                }
            }

            debug!(
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                profile = %current_profile,
                provider = %provider_kind,
                model = %model_name,
                "capability check passed"
            );

            debug!(
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                profile = %current_profile,
                provider = %provider_kind,
                model = %model_name,
                "invoking provider"
            );

            let next_provider_attempt_index = adjusted_request
                .metadata
                .provider_attempt_count()
                .checked_add(1)
                .ok_or_else(|| {
                    LLMError::Validation("physical provider attempt counter overflow".to_owned())
                })?;
            let physical_started_at_ms = Utc::now().timestamp_millis();
            let physical_plan = if physical_resource_bound {
                Some(build_physical_attempt_plan(
                    &current_profile,
                    profile,
                    &provider_kind,
                    &adjusted_request,
                    next_provider_attempt_index,
                    physical_started_at_ms,
                )?)
            } else {
                None
            };
            let mut physical_permit = match (
                adjusted_request.metadata.disclosure_guard(),
                physical_plan.as_ref(),
            ) {
                (Some(guard), Some(plan)) => Some(
                    guard
                        .reserve_physical_attempt(plan.clone())
                        .await
                        .map_err(|_| {
                            LLMError::Validation(
                                "app workflow LLM resource admission failed closed".to_owned(),
                            )
                        })?,
                ),
                _ => None,
            };
            if physical_permit.is_some() {
                if let Err(error) =
                    enforce_disclosure_route(&current_profile, profile, &adjusted_request).await
                {
                    if let Some(permit) = physical_permit.take() {
                        permit
                            .release_pre_io(Utc::now().timestamp_millis())
                            .await
                            .map_err(|_| {
                                LLMError::Validation(
                                    "app workflow LLM pre-I/O release failed closed".to_owned(),
                                )
                            })?;
                    }
                    return Err(with_last_attempted_route(
                        error,
                        last_attempted_route.as_ref(),
                    ));
                }
            }
            let provider_attempt_index = adjusted_request.metadata.record_provider_attempt();
            if provider_attempt_index != next_provider_attempt_index {
                if let Some(permit) = physical_permit.take() {
                    permit
                        .release_pre_io(Utc::now().timestamp_millis())
                        .await
                        .map_err(|_| {
                            LLMError::Validation(
                                "app workflow LLM pre-I/O release failed closed".to_owned(),
                            )
                        })?;
                }
                return Err(LLMError::Validation(
                    "physical provider attempt identity changed before dispatch".to_owned(),
                ));
            }
            let content_capture_sink = adjusted_request.metadata.content_capture_sink.clone();
            let content_capture_context = adjusted_request.metadata.trace_context.clone();
            content_capture_sink.observe(crate::types::LlmContentCaptureEvent::EffectiveRequest {
                request: &adjusted_request,
                profile: &current_profile,
                provider: provider_kind.as_str(),
                provider_attempt_index,
            });
            let attempted_route = LlmRouteIdentity {
                profile: current_profile.clone(),
                provider: provider_kind.clone(),
                model: model_name.clone(),
            };
            let dispatch_mark_failed = match physical_permit.as_mut() {
                Some(permit) => permit.mark_dispatched().await.is_err(),
                None => false,
            };
            if dispatch_mark_failed {
                if let Some(permit) = physical_permit.take() {
                    permit
                        .release_pre_io(Utc::now().timestamp_millis())
                        .await
                        .map_err(|_| {
                            LLMError::Validation(
                                "app workflow LLM pre-I/O release failed closed".to_owned(),
                            )
                        })?;
                }
                return Err(LLMError::Validation(
                    "app workflow LLM physical dispatch identity changed".to_owned(),
                ));
            }
            if physical_permit.is_some() {
                if let Err(error) =
                    enforce_disclosure_route(&current_profile, profile, &adjusted_request).await
                {
                    if let Some(permit) = physical_permit.take() {
                        permit
                            .release_pre_io(Utc::now().timestamp_millis())
                            .await
                            .map_err(|_| {
                                LLMError::Validation(
                                    "app workflow LLM pre-I/O release failed closed".to_owned(),
                                )
                            })?;
                    }
                    return Err(with_last_attempted_route(
                        error,
                        last_attempted_route.as_ref(),
                    ));
                }
            }
            let physical_start = match physical_permit.as_mut() {
                Some(permit) => match permit.mark_physical_started() {
                    Ok(start) => Some(start),
                    Err(_) => {
                        if let Some(permit) = physical_permit.take() {
                            permit
                                .release_pre_io(Utc::now().timestamp_millis())
                                .await
                                .map_err(|_| {
                                    LLMError::Validation(
                                        "app workflow LLM pre-I/O release failed closed".to_owned(),
                                    )
                                })?;
                        }
                        return Err(LLMError::Validation(
                            "app workflow LLM physical start identity changed".to_owned(),
                        ));
                    },
                },
                None => None,
            };
            let provider_dispatch_started_at_ms = physical_start
                .map(|start| start.started_at_unix_ms())
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            let physical_result = if let Some(start) = physical_start {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(start.remaining_resource_ms()),
                    provider.invoke(adjusted_request),
                )
                .await
                {
                    Ok(result) => result,
                    // Physical I/O crossed its absolute resource window. The
                    // provider future is cancelled locally, but the remote
                    // outcome is unknown and is settled below through the
                    // ordinary post-start uncertain path.
                    Err(_) => Err(LLMError::Timeout),
                }
            } else {
                provider.invoke(adjusted_request).await
            };
            let physical_completed_at_ms = Utc::now().timestamp_millis();
            let routed_result = match (physical_result, physical_permit, physical_plan.as_ref()) {
                (Ok(mut response), Some(permit), Some(plan)) => {
                    let observation = match committed_physical_attempt_observation(
                        plan,
                        &response,
                        provider_dispatch_started_at_ms,
                        physical_completed_at_ms,
                    ) {
                        Ok(observation) => observation,
                        Err(error) => {
                            permit
                                .settle(LlmPhysicalAttemptObservation::outcome_uncertain(
                                    provider_dispatch_started_at_ms,
                                    physical_completed_at_ms,
                                ))
                                .await
                                .map_err(|_| {
                                    LLMError::Validation(
                                        "app workflow LLM uncertain settlement failed closed"
                                            .to_owned(),
                                    )
                                })?;
                            return Err(error);
                        },
                    };
                    permit.settle(observation).await.map_err(|_| {
                        LLMError::Validation(
                            "app workflow LLM resource settlement failed closed".to_owned(),
                        )
                    })?;
                    response.route_identity = Some(attempted_route.clone());
                    response.into_json_bounded()
                },
                (Err(error), Some(permit), Some(_plan)) => {
                    permit
                        .settle(LlmPhysicalAttemptObservation::outcome_uncertain(
                            provider_dispatch_started_at_ms,
                            physical_completed_at_ms,
                        ))
                        .await
                        .map_err(|_| {
                            LLMError::Validation(
                                "app workflow LLM uncertain settlement failed closed".to_owned(),
                            )
                        })?;
                    Err(error)
                },
                (Ok(mut response), None, _) => {
                    response.route_identity = Some(attempted_route.clone());
                    response.into_json_bounded()
                },
                (Err(error), None, _) => Err(error),
                (_, Some(_), None) => Err(LLMError::Validation(
                    "app workflow LLM resource identity is unavailable".to_owned(),
                )),
            };
            match routed_result {
                Ok(mut response) => {
                    // The caller's continuation slot is associated with the
                    // originally selected profile. A fallback may produce its
                    // own valid id, but returning it without its physical
                    // cohort would let the next turn misapply it to the
                    // primary profile. Fail closed until the caller persists
                    // the response route alongside the id.
                    if is_fallback_hop {
                        response.response_id = None;
                    }
                    content_capture_sink.observe(
                        crate::types::LlmContentCaptureEvent::NormalizedResponse {
                            response: &response,
                            trace_context: content_capture_context.as_ref(),
                            operation: &operation,
                            profile: &current_profile,
                            provider: provider_kind.as_str(),
                            model: &model_name,
                            provider_attempt_index,
                        },
                    );
                    // Structured post-dispatch log with cache + token
                    // metrics so cache state is observable in real time
                    // via `RUST_LOG=magicllm=info`. `cache_read_pct` is
                    // the share of `prompt_tokens` we got at the cached
                    // read rate; high values prove prefix caching is
                    // landing for this profile.
                    let (
                        prompt_tokens,
                        completion_tokens,
                        reasoning_tokens,
                        cached_tokens,
                        cache_creation_tokens,
                    ) = response
                        .usage
                        .as_ref()
                        .map(|u| {
                            (
                                u.prompt_tokens.unwrap_or(0),
                                u.completion_tokens.unwrap_or(0),
                                u.reasoning_tokens.unwrap_or(0),
                                u.cached_tokens.unwrap_or(0),
                                u.cache_creation_tokens.unwrap_or(0),
                            )
                        })
                        .unwrap_or((0, 0, 0, 0, 0));
                    let cache_read_pct = if prompt_tokens > 0 {
                        ((cached_tokens as f64) * 100.0 / (prompt_tokens as f64) * 10.0).round()
                            / 10.0
                    } else {
                        0.0
                    };
                    info!(
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        profile = %current_profile,
                        provider = %provider_kind,
                        model = %model_name,
                        finish_reason = response.finish_reason.as_deref().unwrap_or(""),
                        prompt_tokens,
                        cached_tokens,
                        cache_creation_tokens,
                        cache_read_pct,
                        completion_tokens,
                        reasoning_tokens,
                        "llm_call_completed"
                    );
                    record_estimator_observation(
                        &operation,
                        trace_id.as_deref(),
                        &current_profile,
                        &model_name,
                        context_preflight.as_ref(),
                        response
                            .usage
                            .as_ref()
                            .and_then(|usage| usage.prompt_tokens),
                    );
                    return Ok(response);
                },
                Err(err) => {
                    let err = if disclosure_bound {
                        err.redact_disclosure_details()
                    } else {
                        err
                    };
                    last_attempted_route = Some(attempted_route.clone());
                    if physical_resource_bound {
                        return Err(with_last_attempted_route(
                            err,
                            last_attempted_route.as_ref(),
                        ));
                    }
                    if let Some(fallback_name) = profile
                        .metadata
                        .as_ref()
                        .and_then(|meta| meta.get("fallback_profile"))
                        .and_then(Value::as_str)
                    {
                        let Some(fallback_profile) = self.config.profiles.get(fallback_name) else {
                            return Err(with_last_attempted_route(
                                LLMError::Configuration(format!(
                                    "profile `{}` references unknown fallback `{}`",
                                    current_profile, fallback_name
                                )),
                                last_attempted_route.as_ref(),
                            ));
                        };

                        if let Some(requested_profile_name) = locked_profile.as_ref() {
                            if fallback_name != requested_profile_name {
                                return Err(with_last_attempted_route(
                                    LLMError::Provider {
                                        provider: profile.provider.to_string(),
                                        message: format!(
                                            "requested profile disallows fallback to `{}`",
                                            fallback_name
                                        ),
                                    },
                                    last_attempted_route.as_ref(),
                                ));
                            }
                        } else if let Some(requested_provider) = locked_provider.as_ref() {
                            if &fallback_profile.provider != requested_provider {
                                return Err(with_last_attempted_route(
                                    LLMError::Provider {
                                        provider: requested_provider.to_string(),
                                        message: format!(
                                            "requested provider disallows fallback to `{}`",
                                            fallback_profile.provider
                                        ),
                                    },
                                    last_attempted_route.as_ref(),
                                ));
                            }
                        }

                        warn!(
                            operation = %operation,
                            trace_id = trace_id.as_deref().unwrap_or(""),
                            profile = %current_profile,
                            provider = %provider_kind,
                            fallback = %fallback_name,
                            error = ?err,
                            "routing request through fallback profile",
                        );

                        current_profile = fallback_name.to_string();
                        continue;
                    }

                    error!(
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        profile = %current_profile,
                        provider = %provider_kind,
                        model = %model_name,
                        error = ?err,
                        "provider call failed without fallback"
                    );
                    return Err(with_last_attempted_route(
                        err,
                        last_attempted_route.as_ref(),
                    ));
                },
            }
        }
    }

    /// Routes a streaming request to the appropriate provider, applying profile defaults
    /// and capability checks. Sends deltas via the provided channel.
    pub async fn route_stream(
        &self,
        mut request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        if !request.json_payloads_are_bounded() {
            request.discard_json_payloads_iteratively();
            return Err(LLMError::Validation(
                "LLM streaming request JSON exceeds the admitted depth/node ceiling".to_string(),
            ));
        }
        request.metadata.enforce_disclosure_capture_policy();
        let operation = if request.metadata.operation.is_empty() {
            "default".to_string()
        } else {
            request.metadata.operation.clone()
        };

        if request.metadata.operation.is_empty() {
            request.metadata.operation = operation.clone();
        }
        request.metadata.ensure_provider_attempt_counter();
        if !request.metadata.logical_content_capture_emitted {
            request.metadata.content_capture_sink.observe(
                crate::types::LlmContentCaptureEvent::LogicalRequest { request: &request },
            );
            request.metadata.logical_content_capture_emitted = true;
        }

        let trace_id = request.metadata.trace_id.clone();
        info!(
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "routing streaming LLM request"
        );

        // Streaming has no fallback traversal, so transfer ownership directly
        // into the provider-adjusted request below instead of cloning the
        // complete bootstrap once on entry.
        let base_request = request;
        let disclosure_bound = base_request.metadata.disclosure_guard().is_some();
        if base_request
            .metadata
            .disclosure_guard()
            .is_some_and(|guard| guard.requires_physical_resource_authority())
        {
            return Err(LLMError::Validation(
                "app workflow streaming LLM dispatch is unavailable until every physical stream attempt is metered"
                    .to_owned(),
            ));
        }
        let requested_profile = router_profile_override(base_request.extra_value());
        let requested_provider_override = router_provider_override_kind(base_request.extra_value());
        let required_provider_kind = router_required_provider_kind(base_request.extra_value());
        let preserve_request_model = router_preserve_request_model(base_request.extra_value());
        let preferred_model = if preserve_request_model {
            let trimmed = base_request.model.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        } else {
            None
        };
        // The magicllm router is the low-level transport dispatcher and
        // does not see the request shape (e.g. `has_images`); it always
        // resolves to the operation's *unconditional* default profile.
        // Shape-aware selection — the per-call alternative used by the
        // outer/inner loop image swap — lives in the magician layer's
        // `operation_llm_router` which knows the request and computes
        // `has_images` before dispatch.
        let mut current_profile = self
            .config
            .operation_mapping
            .get(&operation)
            .map(|selector| {
                selector
                    .profile_for_locality(&RequestShape::NONE, self.config.locality)
                    .to_string()
            })
            .unwrap_or_else(|| self.config.default_profile.clone());
        // Mirror the non-streaming path: collapse adaptive composites
        // to their `fast_profile` so this transport layer never has to
        // know about adaptive routing.
        current_profile = resolve_adaptive_to_fast(&self.config, &current_profile);
        if let Some(profile_name) = requested_profile {
            let profile_name = resolve_adaptive_to_fast(&self.config, &profile_name);
            if !self.config.profiles.contains_key(profile_name.as_str()) {
                return Err(LLMError::Configuration(format!(
                    "requested profile `{}` is not configured",
                    profile_name
                )));
            }
            current_profile = profile_name;
        } else if let Some(provider) = requested_provider_override.as_ref() {
            current_profile = select_profile_for_provider(
                &self.config,
                provider,
                preferred_model.as_deref(),
                Some(&current_profile),
            )
            .ok_or_else(|| {
                LLMError::Configuration(format!(
                    "no profile available for requested provider `{}`",
                    provider
                ))
            })?;
        }

        let profile = match self.config.profiles.get(&current_profile) {
            Some(profile) => profile,
            None => {
                return Err(LLMError::Configuration(format!(
                    "profile `{}` not found in router configuration",
                    current_profile
                )));
            },
        };

        let provider_kind = profile.provider.clone();
        // Same dispatch-time provider lock as the non-streaming path (the
        // streaming path has no fallback traversal, so the single check
        // covers the whole request).
        if let Some(required) = required_provider_kind.as_ref() {
            if &provider_kind != required {
                return Err(LLMError::Configuration(format!(
                    "profile `{}` resolves to provider `{}` but the request requires provider \
                     kind `{}` (provider lock)",
                    current_profile, provider_kind, required
                )));
            }
        }
        let provider = match self
            .profile_providers
            .get(&current_profile)
            .or_else(|| self.providers.get(&provider_kind))
        {
            Some(provider) => provider,
            None => {
                return Err(LLMError::Configuration(format!(
                    "provider `{}` not registered with router",
                    profile.provider
                )));
            },
        };

        let mut adjusted_request = base_request;
        apply_profile_defaults(profile, &mut adjusted_request, preserve_request_model);
        enforce_disclosure_route(&current_profile, profile, &adjusted_request).await?;
        normalize_context_reuse_for_profile(profile, &mut adjusted_request, false);
        strip_router_only_extra(&mut adjusted_request);
        if !adjusted_request.json_payloads_are_bounded() {
            adjusted_request.discard_json_payloads_iteratively();
            return Err(LLMError::Validation(
                "effective streaming LLM request JSON exceeds the admitted depth/node ceiling"
                    .to_string(),
            ));
        }
        let _context_preflight =
            preflight_profile_request(&current_profile, profile, &adjusted_request)?;

        let capabilities = effective_capabilities_for_request(
            profile,
            &adjusted_request,
            provider.capabilities(&adjusted_request.model),
        );
        enforce_capabilities(&capabilities, &adjusted_request)?;

        // Check streaming config gate — fall back to synchronous invoke when
        // the profile has not opted in to streaming.
        let streaming_enabled = profile
            .metadata
            .as_ref()
            .and_then(|m| m.get("streaming"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !streaming_enabled {
            debug!(
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                profile = %current_profile,
                provider = %provider_kind,
                model = %adjusted_request.model,
                "streaming not enabled for profile — falling back to synchronous invoke"
            );
            let provider_attempt_index = adjusted_request.metadata.record_provider_attempt();
            let content_capture_sink = adjusted_request.metadata.content_capture_sink.clone();
            let content_capture_context = adjusted_request.metadata.trace_context.clone();
            content_capture_sink.observe(crate::types::LlmContentCaptureEvent::EffectiveRequest {
                request: &adjusted_request,
                profile: &current_profile,
                provider: provider_kind.as_str(),
                provider_attempt_index,
            });
            let model = adjusted_request.model.clone();
            let identity = LlmRouteIdentity {
                profile: current_profile.clone(),
                provider: provider_kind.clone(),
                model: model.clone(),
            };
            let response = provider
                .invoke(adjusted_request)
                .await
                .and_then(|mut response| {
                    response.route_identity = Some(identity.clone());
                    response.into_json_bounded()
                })
                .map_err(|error| {
                    let error = error.with_route(
                        current_profile.clone(),
                        provider_kind.clone(),
                        model.clone(),
                    );
                    if disclosure_bound {
                        error.redact_disclosure_details()
                    } else {
                        error
                    }
                })?;
            content_capture_sink.observe(
                crate::types::LlmContentCaptureEvent::NormalizedResponse {
                    response: &response,
                    trace_context: content_capture_context.as_ref(),
                    operation: &operation,
                    profile: &current_profile,
                    provider: provider_kind.as_str(),
                    model: &model,
                    provider_attempt_index,
                },
            );
            let _ = tx.send(StreamDelta::Done(response)).await;
            return Ok(());
        }

        debug!(
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            profile = %current_profile,
            provider = %provider_kind,
            model = %adjusted_request.model,
            "invoking streaming provider"
        );

        let model = adjusted_request.model.clone();
        let provider_attempt_index = adjusted_request.metadata.record_provider_attempt();
        let content_capture_sink = adjusted_request.metadata.content_capture_sink.clone();
        let content_capture_context = adjusted_request.metadata.trace_context.clone();
        let content_capture_operation = operation.clone();
        content_capture_sink.observe(crate::types::LlmContentCaptureEvent::EffectiveRequest {
            request: &adjusted_request,
            profile: &current_profile,
            provider: provider_kind.as_str(),
            provider_attempt_index,
        });
        let (provider_tx, mut provider_rx) = mpsc::channel(32);
        let identity = LlmRouteIdentity {
            profile: current_profile,
            provider: provider_kind,
            model,
        };
        let forwarding_identity = identity.clone();
        let mut forward = tokio::spawn(async move {
            while let Some(delta) = provider_rx.recv().await {
                let delta = match delta {
                    StreamDelta::Done(mut response) => {
                        response.route_identity = Some(forwarding_identity.clone());
                        let response = response.into_json_bounded().map_err(|error| {
                            let error = error.with_route(
                                forwarding_identity.profile.clone(),
                                forwarding_identity.provider.clone(),
                                forwarding_identity.model.clone(),
                            );
                            if disclosure_bound {
                                error.redact_disclosure_details()
                            } else {
                                error
                            }
                        })?;
                        content_capture_sink.observe(
                            crate::types::LlmContentCaptureEvent::NormalizedResponse {
                                response: &response,
                                trace_context: content_capture_context.as_ref(),
                                operation: &content_capture_operation,
                                profile: &forwarding_identity.profile,
                                provider: forwarding_identity.provider.as_str(),
                                model: &forwarding_identity.model,
                                provider_attempt_index,
                            },
                        );
                        StreamDelta::Done(response)
                    },
                    other => other,
                };
                if tx.send(delta).await.is_err() {
                    break;
                }
            }
            Ok::<(), LLMError>(())
        });
        let mut provider_call = Box::pin(provider.invoke_stream(adjusted_request, provider_tx));
        tokio::select! {
            biased;
            forward_result = &mut forward => {
                let forward_result = forward_result.map_err(|error| {
                    LLMError::Other(format!("stream response forwarding task failed: {error}"))
                })?;
                // A rejected Done is terminal at the normalized-response
                // boundary. Returning here drops (and therefore cancels) a
                // provider future which emitted that invalid terminal value
                // but remained pending afterward.
                forward_result?;
                provider_call.await.map_err(|error| {
                    let error = error.with_route(
                        identity.profile.clone(),
                        identity.provider.clone(),
                        identity.model.clone(),
                    );
                    if disclosure_bound {
                        error.redact_disclosure_details()
                    } else {
                        error
                    }
                })
            },
            result = &mut provider_call => {
                let result = result.map_err(|error| {
                    let error = error.with_route(
                        identity.profile.clone(),
                        identity.provider.clone(),
                        identity.model.clone(),
                    );
                    if disclosure_bound {
                        error.redact_disclosure_details()
                    } else {
                        error
                    }
                });
                let forward_result = forward.await.map_err(|error| {
                    LLMError::Other(format!("stream response forwarding task failed: {error}"))
                })?;
                result.and(forward_result)
            },
        }
    }
}

/// Provider-native continuation/cache keys are not part of the app disclosure
/// partition. Remove them centrally before the exact physical request is
/// digested or reaches an adapter; reviewed adapters may then add only their
/// explicit stateless controls (for example `store: false`).
fn strip_protected_provider_state(request: &mut LLMRequest) {
    const STATEFUL_KEYS: &[&str] = &[
        "background",
        "cache_control",
        "cachedContent",
        "conversation",
        "idempotency_key",
        "interaction_id",
        "openai_previous_response_id",
        "previous_response_id",
        "prompt_cache_key",
        "prompt_cache_options",
        "session_id",
        "store",
    ];
    let Some(mut extra) = request.take_extra_value() else {
        return;
    };
    if let Value::Object(extra) = &mut extra {
        for key in STATEFUL_KEYS {
            extra.remove(*key);
        }
    }
    request.set_extra(extra);
}

fn effective_capabilities_for_request(
    profile: &LLMProfile,
    request: &LLMRequest,
    provider_capabilities: LLMCapability,
) -> LLMCapability {
    let mut capabilities = match profile.provider {
        LLMProviderKind::OpenAI => match resolve_openai_api_mode(request).selected_api {
            OpenAIApiSelection::Chat => OpenAIChatProvider::capabilities_for_model(&request.model),
            OpenAIApiSelection::Responses => {
                OpenAIResponsesProvider::capabilities_for_model(&request.model)
            },
        },
        _ => provider_capabilities,
    };

    if let Some(supports_vision) = profile.supports_vision {
        capabilities
            .modalities
            .retain(|m| *m != LLMModality::Vision);
        if supports_vision {
            capabilities.modalities.push(LLMModality::Vision);
        }
    }

    if let Some(supports_reasoning) = profile.supports_reasoning {
        capabilities.reasoning = if supports_reasoning {
            if matches!(capabilities.reasoning, LLMReasoning::None) {
                LLMReasoning::Standard
            } else {
                capabilities.reasoning
            }
        } else {
            LLMReasoning::None
        };
    }

    if let Some(supports_tool_calling) = profile.supports_tool_calling {
        capabilities.tool_calling = supports_tool_calling;
    }

    if let Some(supports_computer_use) = profile.supports_computer_use {
        capabilities.computer_use = supports_computer_use;
    }

    capabilities
}

fn validate_config(config: &LLMRouterConfig) -> LLMResult<()> {
    // Default profile must resolve to either a standard or an adaptive
    // composite. Adaptive composites resolve down to a standard via
    // `fast_profile`, so either is fine at this layer.
    if !config.profiles.contains_key(&config.default_profile)
        && !config
            .adaptive_profiles
            .contains_key(&config.default_profile)
    {
        return Err(LLMError::Configuration(format!(
            "default profile `{}` missing from router configuration (checked `profiles` and `adaptive_profiles`)",
            config.default_profile
        )));
    }

    // Validate every profile name an operation could resolve to —
    // including alternatives like `when_has_images`. Each branch must
    // exist in `profiles` OR in `adaptive_profiles`.
    if let Err(errors) = config.validate_operation_metadata() {
        return Err(LLMError::Configuration(format!(
            "operation catalog metadata invalid: {}",
            errors.join("; ")
        )));
    }
    for selector in config.operation_mapping.values() {
        let mut candidates: Vec<&str> = Vec::new();
        match selector {
            crate::config::OperationProfileSelector::Simple(name) => candidates.push(name),
            crate::config::OperationProfileSelector::Conditional {
                default,
                when_has_images,
                when_cloud,
                ..
            } => {
                candidates.push(default);
                if let Some(alt) = when_has_images.as_deref() {
                    candidates.push(alt);
                }
                if let Some(cloud) = when_cloud.as_deref() {
                    candidates.push(cloud);
                }
            },
        }
        for profile_name in candidates {
            if !config.profiles.contains_key(profile_name)
                && !config.adaptive_profiles.contains_key(profile_name)
            {
                return Err(LLMError::Configuration(format!(
                    "operation mapping references unknown profile `{profile_name}` (checked `profiles` and `adaptive_profiles`)"
                )));
            }
        }
    }

    // Adaptive profile self-validation — references must exist in
    // `profiles`, must not nest, and must differ from each other.
    if let Err(errors) = config.validate_adaptive_profiles() {
        return Err(LLMError::Configuration(format!(
            "adaptive profile configuration invalid: {}",
            errors.join("; ")
        )));
    }
    for (profile_name, profile) in &config.profiles {
        validate_context_contract(config, profile_name, profile)?;

        match profile.provider {
            LLMProviderKind::OpenAI
            | LLMProviderKind::Anthropic
            | LLMProviderKind::Minimax
            | LLMProviderKind::DeepSeek
            | LLMProviderKind::OpenRouter
            | LLMProviderKind::Gemini
            | LLMProviderKind::Yutori
            | LLMProviderKind::Xai
            | LLMProviderKind::Sarvam => {
                if profile
                    .api_key_env
                    .as_ref()
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(true)
                {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` must specify `api_key_env` for provider `{}`",
                        profile.provider
                    )));
                }
            },
            LLMProviderKind::Ollama | LLMProviderKind::Custom(_) => {},
        }

        if let Some(meta) = profile.metadata.as_ref() {
            let mut openai_api_mode: Option<String> = None;
            let mut gemini_api_mode: Option<String> = None;
            if let Some(fallback) = meta.get("fallback_profile").and_then(Value::as_str) {
                if fallback == profile_name {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` cannot fallback to itself"
                    )));
                }

                if !config.profiles.contains_key(fallback) {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` references unknown fallback `{fallback}`"
                    )));
                }
            }

            if let Some(mode) = meta.get("openai_api_mode") {
                let Some(mode) = mode.as_str() else {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` has non-string `openai_api_mode` metadata"
                    )));
                };

                if profile.provider != LLMProviderKind::OpenAI {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` sets `openai_api_mode` but provider `{}` is not OpenAI",
                        profile.provider
                    )));
                }

                if !matches!(
                    mode.trim().to_ascii_lowercase().as_str(),
                    "auto" | "chat" | "responses"
                ) {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` has invalid openai_api_mode `{mode}`"
                    )));
                }

                openai_api_mode = Some(mode.trim().to_ascii_lowercase());
            }

            if let Some(mode) = meta.get("gemini_api_mode") {
                let Some(mode) = mode.as_str() else {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` has non-string `gemini_api_mode` metadata"
                    )));
                };

                if profile.provider != LLMProviderKind::Gemini {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` sets `gemini_api_mode` but provider `{}` is not Gemini",
                        profile.provider
                    )));
                }

                if !matches!(
                    mode.trim().to_ascii_lowercase().as_str(),
                    "generate_content" | "interactions"
                ) {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` has invalid gemini_api_mode `{mode}`"
                    )));
                }

                gemini_api_mode = Some(mode.trim().to_ascii_lowercase());
            }

            // Server-side web search rides profile metadata into request
            // extras; validate it at load time so a typo cannot silently
            // enable nothing (or enable search on a transport that fails it
            // closed at runtime).
            if let Some(flag) = meta.get("server_web_search") {
                let shape_valid = matches!(flag, Value::Bool(_) | Value::Object(_));
                if !shape_valid {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` has non-bool/object `server_web_search` metadata"
                    )));
                }
                let enabled = flag.as_bool().unwrap_or(true);
                if enabled
                    && !matches!(
                        profile.provider,
                        LLMProviderKind::OpenAI
                            | LLMProviderKind::Anthropic
                            | LLMProviderKind::Gemini
                            | LLMProviderKind::OpenRouter
                    )
                {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` enables `server_web_search` but provider `{}` \
                         has no server-side web search transport",
                        profile.provider
                    )));
                }
                if enabled
                    && profile.provider == LLMProviderKind::OpenAI
                    && openai_api_mode.as_deref() == Some("chat")
                {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` enables `server_web_search` but pins \
                             `openai_api_mode: chat`; the chat transport fails the flag closed — \
                             use `responses` (or omit the mode)"
                    )));
                }
            }

            if openai_api_mode.as_deref() == Some("chat") {
                if profile.supports_vision == Some(true)
                    || matches!(profile.default_modality, Some(LLMModality::Vision))
                {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` cannot use openai_api_mode=chat with vision enabled"
                    )));
                }

                if profile.supports_computer_use == Some(true) {
                    return Err(LLMError::Configuration(format!(
                        "profile `{profile_name}` cannot use openai_api_mode=chat with computer use enabled"
                    )));
                }
            }

            if gemini_api_mode.as_deref() == Some("interactions")
                && meta
                    .get("streaming")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return Err(LLMError::Configuration(format!(
                    "profile `{profile_name}` cannot enable streaming with gemini_api_mode=interactions until the Interactions SSE adapter is wired"
                )));
            }
        }

        if profile.supports_vision == Some(true) {
            let vision_capable = match profile.provider {
                LLMProviderKind::OpenAI
                | LLMProviderKind::Anthropic
                | LLMProviderKind::OpenRouter
                | LLMProviderKind::Gemini
                | LLMProviderKind::Yutori
                // MiniMax M3 is natively multimodal (image + reasoning + tools
                // on the Anthropic-compat endpoint). The provider is allowed to
                // declare vision; per-profile `supports_vision` + the model still
                // gate which MiniMax profiles actually use it (M2.x stay text-only).
                | LLMProviderKind::Minimax => true,
                // DeepSeek is not vision-capable as a family: Flash and Pro are
                // text-only and only the documented multimodal model takes
                // images. Gate on the model through the provider's own
                // predicate, so validation and runtime cannot disagree about
                // which models see images.
                LLMProviderKind::DeepSeek => {
                    crate::providers::DeepSeekProvider::model_supports_vision(&profile.model)
                },
                LLMProviderKind::Xai => {
                    crate::providers::XaiProvider::model_supports_vision(&profile.model)
                },
                _ => false,
            };
            if !vision_capable {
                return Err(LLMError::Configuration(format!(
                    "profile `{profile_name}` declares vision support but provider `{}` does not advertise vision capability for model `{}`",
                    profile.provider, profile.model
                )));
            }
        }

        if profile.supports_reasoning == Some(true)
            && !matches!(
                profile.provider,
                LLMProviderKind::OpenAI
                    | LLMProviderKind::Minimax
                    | LLMProviderKind::DeepSeek
                    | LLMProviderKind::OpenRouter
                    | LLMProviderKind::Anthropic
                    | LLMProviderKind::Gemini
                    | LLMProviderKind::Yutori
                    | LLMProviderKind::Xai
                    | LLMProviderKind::Sarvam
            )
        {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` declares reasoning support but provider `{}` is not configured for reasoning",
                profile.provider
            )));
        }

        // Grok reasoning cannot be turned off: xAI refuses `effort: none`.
        // A profile that asks for it would fail every call, so refuse it here.
        if profile.provider == LLMProviderKind::Xai
            && profile.reasoning.as_ref().is_some_and(|reasoning| {
                ReasoningConfig::effort_disables_reasoning(&reasoning.effort)
            })
        {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` disables reasoning, which xAI Grok models do not support; \
                 use effort low / medium / high / xhigh"
            )));
        }

        if profile.supports_reasoning == Some(false)
            && profile.reasoning.as_ref().is_some_and(|reasoning| {
                !ReasoningConfig::effort_disables_reasoning(&reasoning.effort)
            })
        {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` disables reasoning support but configures an enabled reasoning default"
            )));
        }

        if profile.supports_tool_calling == Some(true)
            && matches!(profile.provider, LLMProviderKind::Ollama)
            && !ollama_profile_uses_chat_api(profile.api_base_url.as_deref())
        {
            // Not a hard error: magician's config policy requires EVERY routed
            // profile to declare `supports_tool_calling: true`, including
            // no-tools profiles pinned to Ollama (e.g. local transcript
            // summaries). The declaration can't cause silent degradation —
            // the Ollama provider rejects any tool-bearing `/api/generate`
            // request loudly at call time — so warn instead of refusing the
            // whole config. Profiles on the native `/api/chat` contract carry
            // tools and are not warned about.
            let explicitly_no_tools = profile
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("tool_choice"))
                .and_then(Value::as_object)
                .and_then(|tool_choice| tool_choice.get("type"))
                .and_then(Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("none"))
                .unwrap_or(false);
            if explicitly_no_tools {
                debug!(
                    profile = %profile_name,
                    "profile declares tool calling support for config compatibility, but metadata pins tool_choice=none"
                );
            } else {
                warn!(
                    profile = %profile_name,
                    "profile declares tool calling support but provider `ollama` rejects tool-bearing requests at call time"
                );
            }
        }

        if profile.supports_computer_use == Some(true)
            && !matches!(
                profile.provider,
                LLMProviderKind::OpenAI
                    | LLMProviderKind::Anthropic
                    | LLMProviderKind::Gemini
                    | LLMProviderKind::Yutori
            )
        {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` declares computer use support but provider `{}` is not configured for it",
                profile.provider
            )));
        }
    }

    Ok(())
}

fn enforce_capabilities(capabilities: &LLMCapability, request: &LLMRequest) -> LLMResult<()> {
    if !capabilities.supports_modality(request.modality) {
        debug!(
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            model = %request.model,
            modality = ?request.modality,
            supported_modalities = ?capabilities.modalities,
            "rejecting request due to unsupported modality"
        );
        return Err(LLMError::UnsupportedCapability(format!(
            "model does not support requested modality: {:?}",
            request.modality
        )));
    }

    if !capabilities.tool_calling && !request.tools.is_empty() {
        debug!(
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            model = %request.model,
            tool_count = request.tools.len(),
            "rejecting request due to missing tool calling capability"
        );
        return Err(LLMError::UnsupportedCapability(
            "model does not support tool calling".to_string(),
        ));
    }

    if matches!(
        request.response_format_value(),
        Some(LLMResponseFormat::JsonObject) | Some(LLMResponseFormat::JsonSchema { .. })
    ) && !capabilities.json_mode
    {
        debug!(
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            model = %request.model,
            "rejecting request due to JSON response format requirement"
        );
        return Err(LLMError::UnsupportedCapability(
            "model does not support JSON response format".to_string(),
        ));
    }

    let reasoning_requested = request
        .reasoning
        .as_ref()
        .map(|reasoning| !reasoning.is_disabled())
        .unwrap_or(false);
    if reasoning_requested && !capabilities.supports_reasoning() {
        debug!(
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            model = %request.model,
            "rejecting request due to missing reasoning capability"
        );
        return Err(LLMError::UnsupportedCapability(
            "model does not support reasoning configuration".to_string(),
        ));
    }

    Ok(())
}

fn apply_profile_defaults(profile: &LLMProfile, request: &mut LLMRequest, preserve_model: bool) {
    let existing_model = request.model.trim().to_string();
    if preserve_model && !existing_model.is_empty() {
        request.model = existing_model;
    } else {
        // Default behavior: use profile model so fallback profiles remain deterministic.
        request.model = profile.model.clone();
    }

    if request.temperature.is_none() {
        request.temperature = profile.temperature;
    }

    if request.max_output_tokens.is_none() {
        request.max_output_tokens = profile.max_output_tokens;
    }

    if profile.default_modality.is_some() && request.modality == LLMModality::Text {
        let profile_modality = profile.modality();
        if profile_modality != LLMModality::Text {
            request.modality = profile_modality;
        }
    }

    if request.metadata.timeout_secs.is_none() {
        request.metadata.timeout_secs = profile.timeout_secs;
    }

    if request.reasoning.is_none() {
        if let Some(defaults) = &profile.reasoning {
            request.reasoning = Some(ReasoningConfig {
                effort: Some(defaults.effort.clone()),
                max_reasoning_tokens: defaults.max_reasoning_tokens,
                strategy: defaults.strategy.clone(),
                summary: defaults.summary.clone(),
            });
        }
    }

    if let Some(metadata) = profile.metadata.as_ref() {
        let provider_metadata = metadata
            .iter()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "fallback_profile" | "context_window_tokens" | "chunking"
                )
            })
            .collect::<Vec<_>>();
        if !provider_metadata.is_empty() {
            let mut extras = match request.take_extra_value() {
                Some(Value::Object(extras)) => extras,
                Some(_) | None => serde_json::Map::new(),
            };

            for (key, value) in provider_metadata {
                extras.insert(key.clone(), clone_json_value_iteratively(value));
            }

            if !extras.is_empty() {
                request.set_extra(Value::Object(extras));
            }
        }
    }
}

/// Reconcile a caller's reuse intent with the physical profile selected by the
/// router. An opaque checkpoint is valid only for the exact cohort that issued
/// it; every fallback is a cold start because the current caller state stores
/// an id but not the successful fallback route.
fn normalize_context_reuse_for_profile(
    profile: &LLMProfile,
    request: &mut LLMRequest,
    is_fallback_hop: bool,
) {
    if request.context_reuse.is_none() {
        return;
    }
    let strategy = strategy_for_provider(&profile.provider, profile.metadata.as_ref());
    let physical_cohort = transport_cohort_fingerprint(
        &profile.provider,
        &request.model,
        profile.api_base_url.as_deref(),
        profile.metadata.as_ref(),
    );
    let stable_prefix =
        stable_prefix_fingerprint(&request.model, &request.messages, &request.tools);
    let disclosure_partition = request
        .metadata
        .disclosure_guard()
        .map(|guard| guard.continuation_partition().to_string());
    let reuse = request
        .context_reuse_mut()
        .expect("context reuse presence checked before computing fingerprints");
    let cohort_matches =
        reuse.transport_cohort_fingerprint.as_deref() == Some(physical_cohort.as_str());
    let disclosure_partition_matches = match disclosure_partition.as_deref() {
        Some(expected) => reuse.disclosure_partition_fingerprint.as_deref() == Some(expected),
        None => reuse.disclosure_partition_fingerprint.is_none(),
    };
    if is_fallback_hop
        || !strategy.is_stateful()
        || !cohort_matches
        || !disclosure_partition_matches
    {
        reuse.continuation_id = None;
    }
    reuse.strategy = strategy;
    reuse.transport_cohort_fingerprint = Some(physical_cohort);
    reuse.stable_prefix_fingerprint = Some(stable_prefix);
    reuse.disclosure_partition_fingerprint = disclosure_partition;
    reuse.rolling_prefix = matches!(strategy, ContextReuseStrategy::PrefixCache);
}

fn build_physical_attempt_plan(
    profile_name: &str,
    profile: &LLMProfile,
    provider: &LLMProviderKind,
    request: &LLMRequest,
    attempt_index: u32,
    started_at_unix_ms: i64,
) -> LLMResult<LlmPhysicalAttemptPlan> {
    let trace = request.metadata.trace_context.as_ref().ok_or_else(|| {
        LLMError::Validation(
            "app workflow LLM dispatch requires an admitted trace context".to_owned(),
        )
    })?;
    let task_id = trace.task_id.clone().ok_or_else(|| {
        LLMError::Validation("app workflow LLM dispatch requires a task identity".to_owned())
    })?;
    let root_execution_id = trace.root_execution_id.clone().ok_or_else(|| {
        LLMError::Validation(
            "app workflow LLM dispatch requires a canonical root identity".to_owned(),
        )
    })?;
    let execution_id = trace.execution_id.clone().ok_or_else(|| {
        LLMError::Validation("app workflow LLM dispatch requires an execution identity".to_owned())
    })?;
    let context_window_tokens = profile.context_window_tokens.ok_or_else(|| {
        LLMError::Configuration(
            "app workflow model profile requires an explicit context_window_tokens ceiling"
                .to_owned(),
        )
    })?;
    let output_token_upper = request.max_output_tokens.ok_or_else(|| {
        LLMError::Configuration(
            "app workflow model profile requires an explicit max_output_tokens ceiling".to_owned(),
        )
    })?;
    let timeout_secs = request.metadata.timeout_secs.ok_or_else(|| {
        LLMError::Configuration(
            "app workflow model profile requires an explicit physical timeout".to_owned(),
        )
    })?;
    let guard = request.metadata.disclosure_guard().ok_or_else(|| {
        LLMError::Validation("app workflow physical resource guard is unavailable".to_owned())
    })?;
    let pricing = active_table()
        .physical_attempt_quote_at(
            provider,
            &request.model,
            context_window_tokens,
            started_at_unix_ms,
        )
        .ok_or_else(|| {
            LLMError::Configuration(
                "app workflow physical provider pricing is unavailable".to_owned(),
            )
        })?;
    let input_token_upper = u64::from(context_window_tokens);
    let output_token_upper = u64::from(output_token_upper);
    let cost_upper_microusd = pricing
        .cost_upper_microusd(input_token_upper, output_token_upper)
        .ok_or_else(|| {
            LLMError::Validation("app workflow LLM price ceiling overflow".to_owned())
        })?;
    let effective_request_digest = effective_physical_request_digest(request)?;
    LlmPhysicalAttemptPlan::from_router(
        trace.llm_call_id.clone(),
        task_id,
        root_execution_id,
        execution_id,
        trace.iteration_id.clone(),
        profile_name.to_owned(),
        provider.clone(),
        request.model.clone(),
        guard.expected_transport_cohort().to_owned(),
        pricing.pricing_version().to_owned(),
        effective_request_digest,
        attempt_index,
        input_token_upper,
        input_token_upper,
        output_token_upper,
        cost_upper_microusd,
        started_at_unix_ms,
        timeout_secs,
    )
    .map_err(LLMError::Validation)
}

/// Hash the exact final provider-neutral request after profile defaults,
/// normalization and router-only field stripping. JSON lanes have already
/// crossed bounded admission; the capped hashing writer prevents a second
/// payload-sized allocation and never exposes request bytes to diagnostics.
fn effective_physical_request_digest(request: &LLMRequest) -> LLMResult<String> {
    const MAX_DIGESTED_REQUEST_BYTES: usize = 64 * 1024 * 1024;

    struct DigestWriter {
        hasher: blake3::Hasher,
        bytes: usize,
    }

    impl io::Write for DigestWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let next = self
                .bytes
                .checked_add(buffer.len())
                .ok_or_else(|| io::Error::other("physical request size overflow"))?;
            if next > MAX_DIGESTED_REQUEST_BYTES {
                return Err(io::Error::other("physical request digest ceiling exceeded"));
            }
            self.hasher.update(buffer);
            self.bytes = next;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = DigestWriter {
        hasher: blake3::Hasher::new(),
        bytes: 0,
    };
    writer.hasher.update(b"magician.llm.physical-request.v1\0");
    serde_json::to_writer(&mut writer, request).map_err(|_| {
        LLMError::Validation("app workflow physical request digest failed closed".to_owned())
    })?;
    Ok(writer.hasher.finalize().to_hex().to_string())
}

fn committed_physical_attempt_observation(
    plan: &LlmPhysicalAttemptPlan,
    response: &LLMResponse,
    provider_started_at_unix_ms: i64,
    completed_at_unix_ms: i64,
) -> LLMResult<LlmPhysicalAttemptObservation> {
    let usage = response.usage.as_ref().ok_or_else(|| {
        LLMError::Validation(
            "app workflow physical provider omitted authoritative usage".to_owned(),
        )
    })?;
    let prompt_tokens = usage.prompt_tokens.ok_or_else(|| {
        LLMError::Validation("app workflow physical provider omitted input-token usage".to_owned())
    })?;
    let completion_tokens = usage.completion_tokens.ok_or_else(|| {
        LLMError::Validation("app workflow physical provider omitted output-token usage".to_owned())
    })?;
    let cached_tokens = usage.cached_tokens.unwrap_or(0);
    let cache_creation_tokens = usage.cache_creation_tokens.unwrap_or(0);
    if cached_tokens
        .checked_add(cache_creation_tokens)
        .is_none_or(|cached| cached > prompt_tokens)
    {
        return Err(LLMError::Validation(
            "app workflow physical provider returned inconsistent cache usage".to_owned(),
        ));
    }
    let table = active_table();
    let pricing_version = table
        .pricing_version_at(plan.provider(), plan.model(), provider_started_at_unix_ms)
        .ok_or_else(|| {
            LLMError::Validation("app workflow physical provider pricing disappeared".to_owned())
        })?;
    if pricing_version != plan.pricing_version() {
        return Err(LLMError::Validation(
            "app workflow physical provider pricing changed during dispatch".to_owned(),
        ));
    }
    let cost_microusd = usd_to_microusd_ceil(compute_cost_with_server_web_search_at(
        plan.provider(),
        plan.model(),
        usage,
        // Server-side searches bill per call on top of tokens; the count
        // comes from the retained raw response so settlement can never
        // under-count a search-augmented call. ASSUMPTION: every supporting
        // transport populates raw_response on committed success (verified
        // for all four today); a transport that ever stops would settle
        // search costs as zero — silently under-counting.
        response
            .raw_response
            .as_deref()
            .map(|raw| crate::server_web_search::web_search_call_count(plan.provider(), raw))
            .unwrap_or(0),
        provider_started_at_unix_ms,
    ))
    .ok_or_else(|| LLMError::Validation("app workflow LLM actual cost overflow".to_owned()))?;
    LlmPhysicalAttemptObservation::committed(
        u64::from(prompt_tokens),
        u64::from(cached_tokens),
        u64::from(completion_tokens),
        cost_microusd,
        provider_started_at_unix_ms,
        completed_at_unix_ms,
    )
    .map_err(LLMError::Validation)
}

/// Enforce the non-serializable disclosure route fence against the router's
/// own immutable profile snapshot immediately before provider invocation.
/// This check runs on every fallback hop and on the streaming path.
async fn enforce_disclosure_route(
    profile_name: &str,
    profile: &LLMProfile,
    request: &LLMRequest,
) -> LLMResult<()> {
    let Some(guard) = request.metadata.disclosure_guard() else {
        return Ok(());
    };
    if guard.expected_profile() != profile_name {
        return Err(LLMError::Validation(
            "labeled-content disclosure profile changed before dispatch".to_string(),
        ));
    }
    let cohort = transport_cohort_fingerprint(
        &profile.provider,
        &request.model,
        profile.api_base_url.as_deref(),
        profile.metadata.as_ref(),
    );
    if guard.expected_transport_cohort() != cohort {
        return Err(LLMError::Validation(
            "labeled-content disclosure endpoint/model cohort changed before dispatch".to_string(),
        ));
    }
    guard
        .revalidate(
            profile_name,
            &profile.provider,
            &request.model,
            profile.api_base_url.as_deref(),
        )
        .await
        .map_err(|message| {
            LLMError::Validation(format!(
                "labeled-content disclosure authority changed before dispatch: {message}"
            ))
        })?;
    Ok(())
}

fn router_provider_override(extra: Option<&Value>) -> Option<String> {
    extra
        .and_then(Value::as_object)
        .and_then(|map| map.get("router_provider_override"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
}

fn router_profile_override(extra: Option<&Value>) -> Option<String> {
    extra
        .and_then(Value::as_object)
        .and_then(|map| map.get("router_profile_override"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn router_provider_override_kind(extra: Option<&Value>) -> Option<LLMProviderKind> {
    router_provider_override(extra).map(|provider| LLMProviderKind::from_str(provider.as_str()))
}

/// `router_required_provider_kind`: a hard dispatch-time provider LOCK, not a
/// selection hint. Unlike `router_provider_override` (which picks a profile
/// for a provider) this never influences profile selection — it only refuses
/// dispatch when the profile about to be invoked (initial or any fallback
/// hop) belongs to a different provider kind. Used by fail-closed callers
/// (e.g. the mail-assist distiller's locality guard) so a guard verdict
/// computed against an earlier config snapshot cannot be silently invalidated
/// by profile re-resolution or a config hot-reload.
fn router_required_provider_kind(extra: Option<&Value>) -> Option<LLMProviderKind> {
    extra
        .and_then(Value::as_object)
        .and_then(|map| map.get("router_required_provider_kind"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| LLMProviderKind::from_str(value.to_ascii_lowercase().as_str()))
}

fn router_preserve_request_model(extra: Option<&Value>) -> bool {
    extra
        .and_then(Value::as_object)
        .and_then(|map| map.get("router_preserve_model"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Map an adaptive composite name to its `fast_profile`. Returns `name`
/// unchanged if it's not in `adaptive_profiles`. Used at the router's
/// transport layer so adaptive-unaware code paths (autonomous loops,
/// capability probes, inner-loop dispatch) get the concrete fast
/// variant without needing to know that the configured default is a
/// composite. Adaptive-aware callers — currently only
/// `process_chat_inline_turn` — pre-resolve to a standard profile name
/// before dispatch, so the resolve here is a structural no-op for
/// them.
fn resolve_adaptive_to_fast(config: &LLMRouterConfig, name: &str) -> String {
    config
        .adaptive_profiles
        .get(name)
        .map(|adaptive| adaptive.fast_profile.clone())
        .unwrap_or_else(|| name.to_string())
}

fn select_profile_for_provider(
    config: &LLMRouterConfig,
    provider: &LLMProviderKind,
    preferred_model: Option<&str>,
    preferred_profile: Option<&str>,
) -> Option<String> {
    if let Some(profile_name) = preferred_profile {
        if let Some(profile) = config.profiles.get(profile_name) {
            if &profile.provider == provider
                && preferred_model
                    .map(|model| profile.model == model)
                    .unwrap_or(true)
            {
                return Some(profile_name.to_string());
            }
        }
    }

    if let Some(model) = preferred_model {
        if let Some((profile_name, _)) = config
            .profiles
            .iter()
            .find(|(_, profile)| &profile.provider == provider && profile.model == model)
        {
            return Some(profile_name.clone());
        }
    }

    let mut provider_profiles: Vec<&String> = config
        .profiles
        .iter()
        .filter(|(_, profile)| &profile.provider == provider)
        .map(|(name, _)| name)
        .collect();
    provider_profiles.sort();
    provider_profiles.first().map(|name| (*name).clone())
}

fn strip_router_only_extra(request: &mut LLMRequest) {
    const ROUTER_ONLY_KEYS: [&str; 6] = [
        "router_profile_override",
        "router_provider_override",
        "router_required_provider_kind",
        "router_preserve_model",
        "context_window_tokens",
        "chunking",
    ];
    let contains_router_state = request
        .extra_value()
        .and_then(Value::as_object)
        .is_some_and(|object| ROUTER_ONLY_KEYS.iter().any(|key| object.contains_key(*key)));
    if !contains_router_state {
        return;
    }
    let Some(mut value) = request.take_extra_value() else {
        return;
    };
    let Some(obj) = value.as_object_mut() else {
        request.set_extra(value);
        return;
    };

    for key in ROUTER_ONLY_KEYS {
        obj.remove(key);
    }

    if obj.is_empty() {
        request.extra = None;
    } else {
        request.set_extra(value);
    }
}

fn validate_context_contract(
    config: &LLMRouterConfig,
    profile_name: &str,
    profile: &LLMProfile,
) -> LLMResult<()> {
    let Some(context_window_tokens) = profile.context_window_tokens else {
        if profile
            .chunking
            .as_ref()
            .map(|chunking| chunking.enabled)
            .unwrap_or(false)
        {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` enables chunking without `context_window_tokens`"
            )));
        }
        return Ok(());
    };

    if context_window_tokens == 0 {
        return Err(LLMError::Configuration(format!(
            "profile `{profile_name}` has zero `context_window_tokens`"
        )));
    }

    if profile.provider == LLMProviderKind::Ollama {
        if let Some(num_ctx) = profile
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("options"))
            .and_then(Value::as_object)
            .and_then(|options| options.get("num_ctx"))
        {
            let Some(num_ctx) = num_ctx.as_u64().and_then(|value| u32::try_from(value).ok()) else {
                return Err(LLMError::Configuration(format!(
                    "profile `{profile_name}` has non-positive or non-integer Ollama `options.num_ctx`"
                )));
            };
            if num_ctx != context_window_tokens {
                return Err(LLMError::Configuration(format!(
                    "profile `{profile_name}` has `context_window_tokens={context_window_tokens}` but Ollama `options.num_ctx={num_ctx}`"
                )));
            }
        }
    }

    let safety_margin_tokens = profile
        .chunking
        .as_ref()
        .map(|chunking| chunking.safety_margin_tokens)
        .unwrap_or(crate::config::DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS);
    if safety_margin_tokens >= context_window_tokens {
        return Err(LLMError::Configuration(format!(
            "profile `{profile_name}` chunking safety margin must be smaller than its physical context window"
        )));
    }

    let Some(chunking) = profile.chunking.as_ref() else {
        return Ok(());
    };

    if let Some(target_payload_tokens) = chunking.target_payload_tokens {
        if target_payload_tokens == 0 || target_payload_tokens >= context_window_tokens {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` target payload must be positive and smaller than its physical context window"
            )));
        }
        if let Some(logical_window_tokens) = chunking.logical_window_tokens {
            if logical_window_tokens < target_payload_tokens {
                return Err(LLMError::Configuration(format!(
                    "profile `{profile_name}` logical window cannot be smaller than its target payload"
                )));
            }
        }
    }

    if let Some(logical_timeout_secs) = chunking.logical_timeout_secs {
        if logical_timeout_secs == 0 {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` logical chunk timeout must be positive"
            )));
        }
        if profile
            .timeout_secs
            .is_some_and(|physical_timeout| logical_timeout_secs < physical_timeout)
        {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` logical chunk timeout cannot be shorter than its physical timeout"
            )));
        }
    }

    if !chunking.enabled {
        return Ok(());
    }

    if profile.provider != LLMProviderKind::Ollama {
        return Err(LLMError::Configuration(format!(
            "profile `{profile_name}` enables Phase 1 chunking for non-Ollama provider `{}`",
            profile.provider
        )));
    }
    if chunking
        .adapter
        .as_deref()
        .map(str::trim)
        .filter(|adapter| !adapter.is_empty())
        .is_none()
    {
        return Err(LLMError::Configuration(format!(
            "profile `{profile_name}` enables chunking without an adapter"
        )));
    }
    if chunking.logical_window_tokens.unwrap_or(0) == 0
        || chunking.target_payload_tokens.unwrap_or(0) == 0
    {
        return Err(LLMError::Configuration(format!(
            "profile `{profile_name}` enables chunking without positive logical and target payload windows"
        )));
    }

    match chunking.fallback_policy {
        crate::config::ChunkFallbackPolicy::MappedProfile => {
            let fallback = chunking
                .fallback_profile
                .as_deref()
                .map(str::trim)
                .filter(|fallback| !fallback.is_empty())
                .ok_or_else(|| {
                    LLMError::Configuration(format!(
                        "profile `{profile_name}` uses mapped fallback without `fallback_profile`"
                    ))
                })?;
            if fallback == profile_name || !config.profiles.contains_key(fallback) {
                return Err(LLMError::Configuration(format!(
                    "profile `{profile_name}` has invalid chunk fallback profile `{fallback}`"
                )));
            }
        },
        _ if chunking.fallback_profile.is_some() => {
            return Err(LLMError::Configuration(format!(
                "profile `{profile_name}` sets `fallback_profile` without `fallback_policy: mapped_profile`"
            )));
        },
        _ => {},
    }

    Ok(())
}

fn preflight_profile_request(
    profile_name: &str,
    profile: &LLMProfile,
    request: &LLMRequest,
) -> LLMResult<Option<ContextPreflight>> {
    if profile.provider != LLMProviderKind::Ollama {
        return Ok(None);
    }

    let preflight = preflight_request(profile, request, &ConservativeOllamaEstimator)?;
    if let Some(preflight) = preflight.as_ref() {
        // Routine per-call sizing math (`would_overflow=false` in the normal
        // case). A real overflow is not silenced by this: `enforce()` below turns
        // it into a hard error that surfaces on its own path.
        debug!(
            target: "magicllm::context_preflight",
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            profile = %profile_name,
            provider = "ollama",
            model = %request.model,
            estimator = preflight.estimator,
            mode = preflight.mode.as_str(),
            source_bytes = preflight.source_bytes,
            estimated_input_tokens = preflight.estimated_input_tokens,
            reserved_output_tokens = preflight.reserved_output_tokens,
            safety_margin_tokens = preflight.safety_margin_tokens,
            required_tokens = preflight.required_tokens,
            context_window_tokens = preflight.context_window_tokens,
            would_overflow = preflight.would_overflow,
            "llm_context_preflight"
        );
        preflight.enforce()?;
    }
    Ok(preflight)
}

fn record_estimator_observation(
    operation: &str,
    trace_id: Option<&str>,
    profile_name: &str,
    model: &str,
    preflight: Option<&ContextPreflight>,
    actual_prompt_tokens: Option<u32>,
) {
    let (Some(preflight), Some(actual_prompt_tokens)) = (preflight, actual_prompt_tokens) else {
        return;
    };
    let estimator_error_tokens =
        i64::from(actual_prompt_tokens) - i64::from(preflight.estimated_input_tokens);
    // Estimator-calibration telemetry (how far the byte-heuristic drifted from
    // the provider's real prompt count). Useful when tuning the estimator, noise
    // on every call otherwise — DEBUG.
    debug!(
        target: "magicllm::context_preflight",
        operation,
        trace_id = trace_id.unwrap_or(""),
        profile = profile_name,
        provider = "ollama",
        model,
        estimator = preflight.estimator,
        estimated_input_tokens = preflight.estimated_input_tokens,
        actual_prompt_tokens,
        estimator_error_tokens,
        "llm_context_estimator_observation"
    );
}

/// The native chat contract carries tools; only the completion contract
/// refuses them at call time. The provider owns that distinction, so the
/// configuration warning asks it and fires only for profiles it will reject.
fn ollama_profile_uses_chat_api(api_base_url: Option<&str>) -> bool {
    api_base_url
        .map(crate::providers::ollama::base_url_uses_chat_api)
        .unwrap_or(false)
}

#[cfg(test)]
mod ollama_chat_contract_tests {
    use super::ollama_profile_uses_chat_api;

    #[test]
    fn chat_endpoint_is_the_tool_capable_contract() {
        assert!(ollama_profile_uses_chat_api(Some(
            "http://localhost:11434/api/chat"
        )));
        assert!(ollama_profile_uses_chat_api(Some(
            "http://localhost:11434/api/chat/"
        )));
    }

    #[test]
    fn generate_endpoint_and_absent_url_are_not() {
        assert!(!ollama_profile_uses_chat_api(Some(
            "http://localhost:11434/api/generate"
        )));
        assert!(!ollama_profile_uses_chat_api(Some(
            "http://localhost:11434"
        )));
        assert!(!ollama_profile_uses_chat_api(None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::LLMProvider;
    use crate::types::{
        ContentBlock, LLMMessage, LLMToolCall, LLMToolResult, LlmContentCaptureEvent,
        LlmContentCaptureObserver, LlmContentCaptureSink, LlmDisclosureAuthorizer,
        LlmDisclosureCapturePolicy, LlmDisclosureGuard, MessageRole, RequestMetadata,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct CapturingProvider {
        kind: LLMProviderKind,
        requests: Arc<Mutex<Vec<LLMRequest>>>,
        response_id: Option<String>,
    }

    impl CapturingProvider {
        fn new(kind: LLMProviderKind) -> Self {
            Self {
                kind,
                requests: Arc::new(Mutex::new(Vec::new())),
                response_id: None,
            }
        }

        fn with_response_id(mut self, response_id: &str) -> Self {
            self.response_id = Some(response_id.to_string());
            self
        }
    }

    struct FailingProvider {
        kind: LLMProviderKind,
    }

    struct DeepResponseProvider {
        kind: LLMProviderKind,
        hang_after_stream_done: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ObservedContentEvent {
        kind: &'static str,
        model: String,
        profile: Option<String>,
        provider_attempt_index: Option<u32>,
    }

    #[derive(Default)]
    struct RecordingContentObserver(Mutex<Vec<ObservedContentEvent>>);

    impl LlmContentCaptureObserver for RecordingContentObserver {
        fn observe(&self, event: LlmContentCaptureEvent<'_>) {
            let observed = match event {
                LlmContentCaptureEvent::LogicalRequest { request } => ObservedContentEvent {
                    kind: "logical_request",
                    model: request.model.clone(),
                    profile: None,
                    provider_attempt_index: None,
                },
                LlmContentCaptureEvent::EffectiveRequest {
                    request,
                    profile,
                    provider_attempt_index,
                    ..
                } => ObservedContentEvent {
                    kind: "effective_request",
                    model: request.model.clone(),
                    profile: Some(profile.to_string()),
                    provider_attempt_index: Some(provider_attempt_index),
                },
                LlmContentCaptureEvent::NormalizedResponse {
                    model,
                    profile,
                    provider_attempt_index,
                    ..
                } => ObservedContentEvent {
                    kind: "normalized_response",
                    model: model.to_string(),
                    profile: Some(profile.to_string()),
                    provider_attempt_index: Some(provider_attempt_index),
                },
            };
            self.0.lock().expect("content observer").push(observed);
        }
    }

    impl FailingProvider {
        fn new(kind: LLMProviderKind) -> Self {
            Self { kind }
        }
    }

    impl DeepResponseProvider {
        fn new(kind: LLMProviderKind, hang_after_stream_done: bool) -> Self {
            Self {
                kind,
                hang_after_stream_done,
            }
        }

        fn deep_value() -> Value {
            let mut value = Value::Null;
            for _ in 0..10_000 {
                value = Value::Array(vec![value]);
            }
            value
        }

        fn response() -> LLMResponse {
            LLMResponse {
                messages: Arc::new(vec![LLMMessage {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Json {
                        value: Self::deep_value(),
                    }],
                }]),
                tool_calls: Arc::new(vec![LLMToolCall {
                    id: "deep-call".to_string(),
                    name: "deep_tool".to_string(),
                    arguments: Self::deep_value(),
                }]),
                tool_results: Arc::new(vec![LLMToolResult {
                    tool_call_id: "deep-call".to_string(),
                    output: Self::deep_value(),
                }]),
                raw_response: Some(Arc::new(Self::deep_value())),
                ..LLMResponse::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl LLMProvider for CapturingProvider {
        fn provider_kind(&self) -> LLMProviderKind {
            self.kind.clone()
        }

        fn capabilities(&self, _model: &str) -> LLMCapability {
            LLMCapability {
                modalities: vec![LLMModality::Text, LLMModality::Vision],
                tool_calling: true,
                ..LLMCapability::default()
            }
        }

        async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
            self.requests.lock().unwrap().push(request);
            Ok(LLMResponse {
                text: Some(Arc::<str>::from("ok")),
                reasoning_text: None,
                response_id: self.response_id.clone(),
                messages: Arc::new(Vec::new()),
                tool_calls: Arc::new(Vec::new()),
                tool_results: Arc::new(Vec::new()),
                usage: None,
                finish_reason: Some("stop".to_string()),
                raw_response: None,
                provider_latency_ms: None,
                trace_receipt: None,
                route_identity: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl LLMProvider for FailingProvider {
        fn provider_kind(&self) -> LLMProviderKind {
            self.kind.clone()
        }

        fn capabilities(&self, _model: &str) -> LLMCapability {
            LLMCapability {
                modalities: vec![LLMModality::Text, LLMModality::Vision],
                tool_calling: true,
                ..LLMCapability::default()
            }
        }

        async fn invoke(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
            Err(LLMError::Provider {
                provider: self.kind.to_string(),
                message: "forced failure".to_string(),
            })
        }
    }

    #[async_trait::async_trait]
    impl LLMProvider for DeepResponseProvider {
        fn provider_kind(&self) -> LLMProviderKind {
            self.kind.clone()
        }

        fn capabilities(&self, _model: &str) -> LLMCapability {
            LLMCapability {
                modalities: vec![LLMModality::Text],
                tool_calling: true,
                streaming: true,
                ..LLMCapability::default()
            }
        }

        async fn invoke(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
            Ok(Self::response())
        }

        async fn invoke_stream(
            &self,
            _request: LLMRequest,
            tx: mpsc::Sender<StreamDelta>,
        ) -> LLMResult<()> {
            let _ = tx.send(StreamDelta::Done(Self::response())).await;
            if self.hang_after_stream_done {
                std::future::pending::<()>().await;
            }
            Ok(())
        }
    }

    fn sample_profile(provider: LLMProviderKind, model: &str) -> LLMProfile {
        LLMProfile {
            provider,
            model: model.to_string(),
            api_key_env: Some("__TEST_KEY__".to_string()),
            api_base_url: None,
            temperature: Some(0.2),
            max_output_tokens: Some(256),
            default_modality: None,
            reasoning: None,
            metadata: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            timeout_secs: None,
            context_window_tokens: None,
            chunking: None,
        }
    }

    fn sample_request(model: &str) -> LLMRequest {
        LLMRequest {
            model: model.to_string(),
            messages: vec![LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::text("hello")],
            }]
            .into(),
            metadata: RequestMetadata {
                operation: "test".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn disclosure_guard(profile_name: &str, profile: &LLMProfile) -> LlmDisclosureGuard {
        let cohort = transport_cohort_fingerprint(
            &profile.provider,
            &profile.model,
            profile.api_base_url.as_deref(),
            profile.metadata.as_ref(),
        );
        LlmDisclosureGuard::new(
            profile_name,
            cohort,
            "app-continuation-partition-v1",
            "app-policy-digest-v1",
            LlmDisclosureCapturePolicy::MetadataOnly,
            Arc::new(CurrentDisclosureAuthority),
        )
        .expect("valid disclosure guard")
    }

    #[derive(Debug)]
    struct CurrentDisclosureAuthority;

    #[async_trait::async_trait]
    impl LlmDisclosureAuthorizer for CurrentDisclosureAuthority {
        async fn revalidate(
            &self,
            _profile: &str,
            _provider: &LLMProviderKind,
            _model: &str,
            _api_base_url: Option<&str>,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RevokedDisclosureAuthority;

    #[async_trait::async_trait]
    impl LlmDisclosureAuthorizer for RevokedDisclosureAuthority {
        async fn revalidate(
            &self,
            _profile: &str,
            _provider: &LLMProviderKind,
            _model: &str,
            _api_base_url: Option<&str>,
        ) -> Result<(), String> {
            Err("grant revoked".to_owned())
        }
    }

    fn deep_response_router(streaming: bool) -> MultiLLMRouter {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "deep-response".to_string();
        config
            .operation_mapping
            .insert("test".to_string(), "deep-response".into());
        let mut profile = sample_profile(LLMProviderKind::OpenAI, "gpt-5");
        profile.metadata = Some(HashMap::from([(
            "streaming".to_string(),
            Value::Bool(streaming),
        )]));
        config.profiles.insert("deep-response".to_string(), profile);
        let mut router = MultiLLMRouter::new(config).expect("deep response router");
        router.register_provider(Arc::new(DeepResponseProvider::new(
            LLMProviderKind::OpenAI,
            streaming,
        )));
        router
    }

    fn assert_response_boundary_validation<T>(result: LLMResult<T>) {
        let error = match result {
            Ok(_) => panic!("over-depth provider response must be rejected"),
            Err(error) => error,
        };
        assert!(matches!(error.root_cause(), LLMError::Validation(_)));
    }

    #[test]
    fn sync_custom_provider_response_is_rejected_and_disposed_on_a_small_stack() {
        std::thread::Builder::new()
            .name("sync-deep-response-boundary".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime");
                runtime.block_on(async {
                    let router = deep_response_router(false);
                    assert_response_boundary_validation(
                        router.route(sample_request("gpt-5")).await,
                    );
                });
            })
            .expect("small-stack sync response worker")
            .join()
            .expect("sync rejection must not overflow");
    }

    #[test]
    fn sync_fallback_stream_response_is_rejected_and_disposed_on_a_small_stack() {
        std::thread::Builder::new()
            .name("sync-stream-deep-response-boundary".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime");
                runtime.block_on(async {
                    let router = deep_response_router(false);
                    let (tx, mut rx) = mpsc::channel(2);
                    assert_response_boundary_validation(
                        router.route_stream(sample_request("gpt-5"), tx).await,
                    );
                    assert!(
                        rx.recv().await.is_none(),
                        "rejected response must not emit Done"
                    );
                });
            })
            .expect("small-stack sync-stream response worker")
            .join()
            .expect("sync stream fallback rejection must not overflow");
    }

    #[test]
    fn streaming_custom_provider_done_is_rejected_and_disposed_on_a_small_stack() {
        std::thread::Builder::new()
            .name("streaming-deep-response-boundary".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("current-thread runtime");
                runtime.block_on(async {
                    let router = deep_response_router(true);
                    let (tx, mut rx) = mpsc::channel(2);
                    assert_response_boundary_validation(
                        tokio::time::timeout(
                            std::time::Duration::from_secs(1),
                            router.route_stream(sample_request("gpt-5"), tx),
                        )
                        .await
                        .expect("invalid Done must cancel a provider that remains pending"),
                    );
                    assert!(
                        rx.recv().await.is_none(),
                        "rejected response must not emit Done"
                    );
                });
            })
            .expect("small-stack streaming response worker")
            .join()
            .expect("streaming rejection must not overflow");
    }

    #[test]
    fn enforce_capabilities_allows_disabled_reasoning_without_reasoning_support() {
        let mut request = sample_request("non-reasoning-model");
        request.reasoning = Some(ReasoningConfig {
            effort: Some("none".to_string()),
            ..Default::default()
        });

        let capability = LLMCapability {
            modalities: vec![LLMModality::Text],
            reasoning: LLMReasoning::None,
            tool_calling: true,
            ..Default::default()
        };

        enforce_capabilities(&capability, &request).expect("disabled reasoning is not reasoning");
    }

    #[test]
    fn enforce_capabilities_rejects_enabled_reasoning_without_reasoning_support() {
        let mut request = sample_request("non-reasoning-model");
        request.reasoning = Some(ReasoningConfig {
            effort: Some("high".to_string()),
            ..Default::default()
        });

        let capability = LLMCapability {
            modalities: vec![LLMModality::Text],
            reasoning: LLMReasoning::None,
            tool_calling: true,
            ..Default::default()
        };

        assert!(enforce_capabilities(&capability, &request).is_err());
    }

    #[test]
    fn apply_profile_defaults_respects_preserve_model_flag() {
        let profile = sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra");

        let mut request = sample_request("custom-model");
        apply_profile_defaults(&profile, &mut request, true);
        assert_eq!(request.model, "custom-model");

        let mut request = sample_request("custom-model");
        apply_profile_defaults(&profile, &mut request, false);
        assert_eq!(request.model, "gpt-5.6-terra");
    }

    #[test]
    fn effective_capabilities_use_chat_capabilities_for_auto_text_only_requests() {
        let mut profile = sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra");
        profile.metadata = Some(HashMap::from([(
            "openai_api_mode".to_string(),
            Value::String("auto".to_string()),
        )]));

        let mut request = sample_request("gpt-5.6-terra");
        apply_profile_defaults(&profile, &mut request, false);
        let caps = effective_capabilities_for_request(
            &profile,
            &request,
            OpenAIResponsesProvider::capabilities_for_model("gpt-5.6-terra"),
        );

        assert!(!caps.modalities.contains(&LLMModality::Vision));
        assert!(caps.streaming);
    }

    #[test]
    fn effective_capabilities_use_responses_capabilities_for_auto_vision_requests() {
        let mut profile = sample_profile(LLMProviderKind::OpenAI, "gpt-5");
        profile.metadata = Some(HashMap::from([(
            "openai_api_mode".to_string(),
            Value::String("auto".to_string()),
        )]));

        let mut request = sample_request("gpt-5");
        apply_profile_defaults(&profile, &mut request, false);
        request.modality = LLMModality::Vision;
        let caps = effective_capabilities_for_request(
            &profile,
            &request,
            OpenAIChatProvider::capabilities_for_model("gpt-5"),
        );

        assert!(caps.modalities.contains(&LLMModality::Vision));
    }

    #[test]
    fn effective_capabilities_apply_profile_overrides() {
        let mut profile = sample_profile(LLMProviderKind::Anthropic, "claude-sonnet-4-6");
        profile.supports_vision = Some(true);
        profile.supports_reasoning = Some(true);
        profile.supports_tool_calling = Some(false);
        profile.supports_computer_use = Some(true);

        let caps = effective_capabilities_for_request(
            &profile,
            &sample_request("claude-sonnet-4-6"),
            LLMCapability::default(),
        );

        assert!(caps.modalities.contains(&LLMModality::Vision));
        assert!(caps.supports_reasoning());
        assert!(!caps.tool_calling);
        assert!(caps.computer_use);
    }

    #[test]
    fn select_profile_for_provider_prefers_matching_model() {
        let mut config = LLMRouterConfig::default();
        config.profiles.insert(
            "openai_a".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra"),
        );
        config.profiles.insert(
            "openai_b".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "gpt-5"),
        );
        config.profiles.insert(
            "anthropic_a".to_string(),
            sample_profile(LLMProviderKind::Anthropic, "claude-3-5-sonnet"),
        );

        let selected = select_profile_for_provider(
            &config,
            &LLMProviderKind::OpenAI,
            Some("gpt-5"),
            Some("openai_a"),
        );
        assert_eq!(selected.as_deref(), Some("openai_b"));

        let selected = select_profile_for_provider(
            &config,
            &LLMProviderKind::Anthropic,
            None,
            Some("openai_a"),
        );
        assert_eq!(selected.as_deref(), Some("anthropic_a"));
    }

    #[tokio::test]
    async fn route_honors_exact_profile_override_and_applies_full_profile_defaults() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "vision-canvas-sonnet".to_string();
        config
            .operation_mapping
            .insert("visual_review".to_string(), "vision-canvas-sonnet".into());
        config.profiles.insert(
            "vision-canvas-sonnet".to_string(),
            LLMProfile {
                temperature: Some(0.2),
                max_output_tokens: Some(8192),
                reasoning: None,
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Anthropic, "claude-sonnet-4-6")
            },
        );
        config.profiles.insert(
            "vision-gpt5".to_string(),
            LLMProfile {
                temperature: Some(0.1),
                max_output_tokens: Some(4096),
                reasoning: Some(crate::config::ReasoningDefaults {
                    effort: "medium".to_string(),
                    max_reasoning_tokens: Some(2048),
                    strategy: None,
                    summary: None,
                }),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5")
            },
        );

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let captured = provider.requests.clone();

        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "visual_review".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "vision-gpt5",
            })
            .into(),
        );

        let response = router.route(request).await.expect("route should succeed");

        let identity = response
            .route_identity
            .as_ref()
            .expect("successful routing must expose its effective route");
        assert_eq!(identity.profile, "vision-gpt5");
        assert_eq!(identity.provider, LLMProviderKind::OpenAI);
        assert_eq!(identity.model, "gpt-5");

        let requests = captured.lock().unwrap();
        let applied = requests.last().expect("captured request");
        assert_eq!(applied.model, "gpt-5");
        assert_eq!(applied.temperature, Some(0.1));
        assert_eq!(applied.max_output_tokens, Some(4096));
        assert_eq!(
            applied
                .reasoning
                .as_ref()
                .and_then(|value| value.effort.as_deref()),
            Some("medium")
        );
        assert!(
            applied
                .extra
                .as_ref()
                .and_then(|value| value.get("router_profile_override"))
                .is_none(),
            "router-only override metadata should not leak to providers"
        );
    }

    #[tokio::test]
    async fn content_observer_receives_one_logical_effective_and_normalized_event() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        config.profiles.insert(
            "selected".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "effective-model"),
        );
        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let observer = Arc::new(RecordingContentObserver::default());
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("logical-model");
        request.metadata.content_capture_sink = LlmContentCaptureSink::new(observer.clone());
        router.route(request).await.expect("route");

        assert_eq!(
            observer.0.lock().expect("events").as_slice(),
            [
                ObservedContentEvent {
                    kind: "logical_request",
                    model: "logical-model".to_string(),
                    profile: None,
                    provider_attempt_index: None,
                },
                ObservedContentEvent {
                    kind: "effective_request",
                    model: "effective-model".to_string(),
                    profile: Some("selected".to_string()),
                    provider_attempt_index: Some(1),
                },
                ObservedContentEvent {
                    kind: "normalized_response",
                    model: "effective-model".to_string(),
                    profile: Some("selected".to_string()),
                    provider_attempt_index: Some(1),
                },
            ]
        );
    }

    #[tokio::test]
    async fn disclosure_guard_binds_exact_route_and_suppresses_payload_capture() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        let mut profile = sample_profile(LLMProviderKind::OpenAI, "effective-model");
        profile.api_base_url = Some("https://trusted.example/v1".to_string());
        let guard = disclosure_guard("selected", &profile);
        config.profiles.insert("selected".to_string(), profile);

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let captured = provider.requests.clone();
        let observer = Arc::new(RecordingContentObserver::default());
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("logical-model");
        request.metadata.content_capture_sink = LlmContentCaptureSink::new(observer.clone());
        request.metadata.set_disclosure_guard(guard);
        router.route(request).await.expect("exact guarded route");

        assert!(observer.0.lock().expect("events").is_empty());
        let requests = captured.lock().expect("captured requests");
        let routed = requests.last().expect("provider request");
        assert!(routed.metadata.disclosure_guard().is_some());
        assert!(!routed.metadata.content_capture_sink.is_attached());
    }

    #[test]
    fn disclosure_checkpoint_is_content_free_and_cannot_replace_runtime_guard() {
        let profile = sample_profile(LLMProviderKind::OpenAI, "effective-model");
        let guard = disclosure_guard("selected", &profile);
        let checkpoint = guard.checkpoint();
        let encoded = serde_json::to_value(&checkpoint).expect("checkpoint JSON");
        let decoded: crate::types::LlmDisclosureCheckpoint =
            serde_json::from_value(encoded.clone()).expect("checkpoint round trip");
        assert!(decoded.matches_guard(&guard));
        assert!(!encoded.to_string().contains("authorizer"));
        assert!(!encoded.to_string().contains("protected app content"));

        let request = sample_request("logical-model");
        assert!(request.metadata.disclosure_guard().is_none());
    }

    #[tokio::test]
    async fn disclosure_guard_rejects_endpoint_or_model_cohort_drift_before_provider() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        let original = sample_profile(LLMProviderKind::OpenAI, "effective-model");
        let guard = disclosure_guard("selected", &original);
        let mut drifted = original;
        drifted.api_base_url = Some("https://different.example/v1".to_string());
        config.profiles.insert("selected".to_string(), drifted);

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("logical-model");
        request.metadata.set_disclosure_guard(guard);
        let error = router
            .route(request)
            .await
            .expect_err("cohort drift must fail closed");
        assert!(matches!(error.root_cause(), LLMError::Validation(_)));
        assert!(captured.lock().expect("captured requests").is_empty());
    }

    #[tokio::test]
    async fn disclosure_guard_revalidates_mutable_authority_before_provider() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        let profile = sample_profile(LLMProviderKind::OpenAI, "effective-model");
        let cohort = transport_cohort_fingerprint(
            &profile.provider,
            &profile.model,
            profile.api_base_url.as_deref(),
            profile.metadata.as_ref(),
        );
        config.profiles.insert("selected".to_string(), profile);
        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("logical-model");
        request.metadata.set_disclosure_guard(
            LlmDisclosureGuard::new(
                "selected",
                cohort,
                "app-continuation-partition-v1",
                "app-policy-digest-v1",
                LlmDisclosureCapturePolicy::MetadataOnly,
                Arc::new(RevokedDisclosureAuthority),
            )
            .expect("guard"),
        );
        let error = router
            .route(request)
            .await
            .expect_err("revoked authority must fail closed");
        assert!(matches!(error.root_cause(), LLMError::Validation(_)));
        assert!(captured.lock().expect("captured requests").is_empty());
    }

    #[tokio::test]
    async fn disclosure_partition_change_cold_starts_provider_continuation() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        let mut profile = sample_profile(LLMProviderKind::OpenAI, "effective-model");
        profile.metadata = Some(HashMap::from([(
            "openai_api_mode".to_string(),
            Value::String("responses".to_string()),
        )]));
        let guard = disclosure_guard("selected", &profile);
        let cohort = transport_cohort_fingerprint(
            &profile.provider,
            &profile.model,
            profile.api_base_url.as_deref(),
            profile.metadata.as_ref(),
        );
        config.profiles.insert("selected".to_string(), profile);

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("logical-model");
        request.metadata.set_disclosure_guard(guard);
        let mut reuse =
            crate::context_reuse::ContextReuseConfig::new(ContextReuseStrategy::ServerContinuation);
        reuse.continuation_id = Some("provider-history".to_string());
        reuse.transport_cohort_fingerprint = Some(cohort);
        reuse.disclosure_partition_fingerprint = Some("stale-partition".to_string());
        request.set_context_reuse(reuse);

        router.route(request).await.expect("guarded cold start");
        let requests = captured.lock().expect("captured requests");
        let reuse = requests
            .last()
            .and_then(|request| request.context_reuse.as_deref())
            .expect("context plan");
        assert_eq!(reuse.continuation_id, None);
        assert_eq!(
            reuse.disclosure_partition_fingerprint.as_deref(),
            Some("app-continuation-partition-v1")
        );
    }

    #[test]
    fn protected_provider_state_is_removed_before_physical_digest() {
        let mut request = sample_request("model");
        request.set_extra(json!({
            "cachedContent": "cachedContents/private",
            "cache_control": { "type": "ephemeral" },
            "conversation": "conversation-private",
            "session_id": "session-private",
            "store": true,
            "temperature_hint": 7
        }));

        strip_protected_provider_state(&mut request);

        let extra = request
            .extra_value()
            .and_then(Value::as_object)
            .expect("extra object");
        for key in [
            "cachedContent",
            "cache_control",
            "conversation",
            "session_id",
            "store",
        ] {
            assert!(!extra.contains_key(key), "state key {key} must be absent");
        }
        assert_eq!(extra.get("temperature_hint"), Some(&Value::from(7)));
    }

    #[test]
    fn content_observer_and_capture_state_are_never_serialized() {
        let observer = Arc::new(RecordingContentObserver::default());
        let mut request = sample_request("model");
        request.metadata.content_capture_sink = LlmContentCaptureSink::new(observer);
        request.metadata.logical_content_capture_emitted = true;
        let profile = sample_profile(LLMProviderKind::OpenAI, "model");
        request
            .metadata
            .set_disclosure_guard(disclosure_guard("selected", &profile));
        let mut reuse =
            crate::context_reuse::ContextReuseConfig::new(ContextReuseStrategy::ServerContinuation);
        reuse.continuation_id = Some("provider-state".to_string());
        reuse.transport_cohort_fingerprint = Some("transport-fingerprint".to_string());
        request.set_context_reuse(reuse);
        let encoded = serde_json::to_value(&request).expect("request JSON");
        let metadata = encoded["metadata"].as_object().expect("metadata object");
        assert!(!metadata.contains_key("content_capture_sink"));
        assert!(!metadata.contains_key("logical_content_capture_emitted"));
        assert!(!metadata.contains_key("trace_context"));
        assert!(!metadata.contains_key("provider_attempt_counter"));
        assert!(!metadata.contains_key("disclosure_guard"));
        assert!(!encoded.as_object().unwrap().contains_key("context_reuse"));
        assert!(!encoded.to_string().contains("provider-state"));
    }

    #[tokio::test]
    async fn profile_bound_providers_do_not_cross_endpoints_with_the_same_kind() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "endpoint-a".to_string();
        config.profiles.insert(
            "endpoint-a".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "model-a"),
        );
        config.profiles.insert(
            "endpoint-b".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "model-b"),
        );

        let endpoint_a = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let calls_a = Arc::clone(&endpoint_a.requests);
        let endpoint_b = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let calls_b = Arc::clone(&endpoint_b.requests);
        let mut router = MultiLLMRouter::new(config).expect("router");
        router
            .register_profile_provider("endpoint-a", endpoint_a)
            .expect("endpoint a");
        router
            .register_profile_provider("endpoint-b", endpoint_b)
            .expect("endpoint b");

        let mut request = sample_request("ignored");
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "endpoint-b",
            })
            .into(),
        );
        let response = router.route(request).await.expect("route through b");

        assert!(calls_a.lock().unwrap().is_empty());
        assert_eq!(calls_b.lock().unwrap().len(), 1);
        let identity = response.route_identity.expect("effective route");
        assert_eq!(identity.profile, "endpoint-b");
        assert_eq!(identity.model, "model-b");
    }

    #[test]
    fn profile_bound_provider_kind_mismatch_fails_closed() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "openai".to_string();
        config.profiles.insert(
            "openai".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "gpt-test"),
        );
        let mut router = MultiLLMRouter::new(config).expect("router");
        let error = router
            .register_profile_provider(
                "openai",
                Arc::new(CapturingProvider::new(LLMProviderKind::Anthropic)),
            )
            .expect_err("provider/profile mismatch must fail closed");
        assert!(matches!(error, LLMError::Configuration(_)));
    }

    #[test]
    fn dispatch_gates_resolve_provider_and_timeout_from_exact_profile_override() {
        use crate::dispatch::DispatchRouter;

        let mut config = LLMRouterConfig::default();
        config.default_profile = "remote-default".to_string();
        config
            .operation_mapping
            .insert("phase3_test".to_string(), "remote-default".into());
        config.profiles.insert(
            "remote-default".to_string(),
            LLMProfile {
                timeout_secs: Some(45),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra")
            },
        );
        config.profiles.insert(
            "local-chunk".to_string(),
            LLMProfile {
                timeout_secs: Some(180),
                ..sample_profile(LLMProviderKind::Ollama, "gemma4:12b")
            },
        );
        let router = MultiLLMRouter::new(config).expect("router");
        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "phase3_test".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "local-chunk"
            })
            .into(),
        );

        assert_eq!(
            DispatchRouter::provider_for_request(&router, &request),
            Some(LLMProviderKind::Ollama)
        );
        assert_eq!(
            DispatchRouter::timeout_for_request(&router, &request),
            Some(180)
        );
        assert_eq!(
            DispatchRouter::provider_for_operation(&router, "phase3_test"),
            Some(LLMProviderKind::OpenAI)
        );
    }

    #[tokio::test]
    async fn route_rejects_unknown_exact_profile_override() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "vision-canvas-sonnet".to_string();
        config.profiles.insert(
            "vision-canvas-sonnet".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Anthropic, "claude-sonnet-4-6")
            },
        );

        let router = MultiLLMRouter::new(config).expect("router");
        let mut request = sample_request("ignored");
        request.metadata.operation = "visual_review".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "missing-profile",
            })
            .into(),
        );

        let err = router
            .route(request)
            .await
            .expect_err("expected config error");
        assert!(matches!(err, LLMError::Configuration(_)));
        assert!(err.to_string().contains("missing-profile"));
    }

    #[tokio::test]
    async fn terminal_provider_failure_carries_the_effective_route() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        config.profiles.insert(
            "selected".to_string(),
            sample_profile(LLMProviderKind::OpenAI, "actual-model"),
        );
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));

        let error = router
            .route(sample_request("requested-placeholder"))
            .await
            .expect_err("provider must fail");
        let (profile, provider, model) = error
            .effective_route()
            .expect("provider failure must expose the concrete route");
        assert_eq!(profile, "selected");
        assert_eq!(provider, &LLMProviderKind::OpenAI);
        assert_eq!(model, "actual-model");
        assert!(matches!(error.root_cause(), LLMError::Provider { .. }));
    }

    #[tokio::test]
    async fn disclosure_bound_provider_failure_keeps_route_but_redacts_provider_detail() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected".to_string();
        let profile = sample_profile(LLMProviderKind::OpenAI, "actual-model");
        let guard = disclosure_guard("selected", &profile);
        config.profiles.insert("selected".to_string(), profile);
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));

        let mut request = sample_request("requested-placeholder");
        request.metadata.set_disclosure_guard(guard);
        let error = router.route(request).await.expect_err("provider must fail");

        assert_eq!(
            error.effective_route(),
            Some(("selected", &LLMProviderKind::OpenAI, "actual-model"))
        );
        assert!(matches!(error.root_cause(), LLMError::Provider { .. }));
        assert!(!error.to_string().contains("forced failure"));
        assert!(error
            .to_string()
            .contains("guarded provider request failed"));
    }

    #[tokio::test]
    async fn streaming_provider_failure_carries_the_effective_route() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected-stream".to_string();
        config.profiles.insert(
            "selected-stream".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "streaming".to_string(),
                    Value::Bool(true),
                )])),
                ..sample_profile(LLMProviderKind::OpenAI, "actual-stream-model")
            },
        );
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));
        let (tx, mut rx) = mpsc::channel(4);

        let error = router
            .route_stream(sample_request("requested-placeholder"), tx)
            .await
            .expect_err("streaming provider must fail");

        let (profile, provider, model) = error
            .effective_route()
            .expect("streaming failure must expose the concrete route");
        assert_eq!(profile, "selected-stream");
        assert_eq!(provider, &LLMProviderKind::OpenAI);
        assert_eq!(model, "actual-stream-model");
        assert!(matches!(error.root_cause(), LLMError::Provider { .. }));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn disclosure_bound_stream_failure_redacts_provider_detail() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "selected-stream".to_string();
        let profile = LLMProfile {
            metadata: Some(HashMap::from([(
                "streaming".to_string(),
                Value::Bool(true),
            )])),
            ..sample_profile(LLMProviderKind::OpenAI, "actual-stream-model")
        };
        let guard = disclosure_guard("selected-stream", &profile);
        config
            .profiles
            .insert("selected-stream".to_string(), profile);
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));
        let (tx, mut rx) = mpsc::channel(4);
        let mut request = sample_request("requested-placeholder");
        request.metadata.set_disclosure_guard(guard);

        let error = router
            .route_stream(request, tx)
            .await
            .expect_err("streaming provider must fail");

        assert_eq!(
            error.effective_route(),
            Some((
                "selected-stream",
                &LLMProviderKind::OpenAI,
                "actual-stream-model"
            ))
        );
        assert!(!error.to_string().contains("forced failure"));
        assert!(error
            .to_string()
            .contains("guarded provider request failed"));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn route_blocks_cross_provider_fallback_for_exact_profile_override() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "vision-gpt5".to_string();
        config
            .operation_mapping
            .insert("visual_review".to_string(), "vision-gpt5".into());
        config.profiles.insert(
            "vision-gpt5".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "fallback_profile".to_string(),
                    Value::String("sonnet46-messages-vision-toolsany-rnone".to_string()),
                )])),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5")
            },
        );
        config.profiles.insert(
            "sonnet46-messages-vision-toolsany-rnone".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Anthropic, "claude-sonnet-4-6")
            },
        );

        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));

        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "visual_review".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "vision-gpt5",
            })
            .into(),
        );

        let err = router
            .route(request)
            .await
            .expect_err("cross-provider fallback should be blocked");
        assert!(matches!(err.root_cause(), LLMError::Provider { .. }));
        let (profile, provider, model) = err
            .effective_route()
            .expect("the rejected fallback follows a real primary attempt");
        assert_eq!(profile, "vision-gpt5");
        assert_eq!(provider, &LLMProviderKind::OpenAI);
        assert_eq!(model, "gpt-5");
        assert!(err.to_string().contains("disallows fallback"));
        // The locked-profile check fires (not locked-provider), so the error
        // mentions the fallback profile name, not the fallback provider name.
        assert!(err
            .to_string()
            .contains("sonnet46-messages-vision-toolsany-rnone"));
    }

    #[tokio::test]
    async fn route_blocks_same_provider_fallback_for_exact_profile_override() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "vision-gpt5".to_string();
        config
            .operation_mapping
            .insert("visual_review".to_string(), "vision-gpt5".into());
        config.profiles.insert(
            "vision-gpt5".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "fallback_profile".to_string(),
                    Value::String("vision-gpt5-mini".to_string()),
                )])),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5")
            },
        );
        config.profiles.insert(
            "vision-gpt5-mini".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra")
            },
        );

        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));

        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "visual_review".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "vision-gpt5",
            })
            .into(),
        );

        let err = router
            .route(request)
            .await
            .expect_err("same-provider fallback should be blocked");
        assert!(matches!(err.root_cause(), LLMError::Provider { .. }));
        assert_eq!(
            err.effective_route()
                .map(|(profile, _, model)| (profile, model)),
            Some(("vision-gpt5", "gpt-5"))
        );
        assert!(err
            .to_string()
            .contains("requested profile disallows fallback"));
        assert!(err.to_string().contains("vision-gpt5-mini"));
    }

    #[tokio::test]
    async fn route_honors_required_provider_kind_lock_on_match() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "remote-openai".to_string();
        config
            .operation_mapping
            .insert("distill_test".to_string(), "remote-openai".into());
        config.profiles.insert(
            "remote-openai".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5")
            },
        );
        config.profiles.insert(
            "local-ollama".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Ollama, "gemma3")
            },
        );

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::Ollama));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "distill_test".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "local-ollama",
                "router_required_provider_kind": "ollama",
            })
            .into(),
        );

        router.route(request).await.expect("route should succeed");

        let requests = captured.lock().unwrap();
        let applied = requests.last().expect("captured request");
        // The pinned profile dispatched (not the operation's remote default)
        // and the router-only lock keys did not leak to the provider.
        assert_eq!(applied.model, "gemma3");
        assert!(applied
            .extra
            .as_ref()
            .and_then(|value| value.get("router_required_provider_kind"))
            .is_none());
    }

    #[tokio::test]
    async fn route_rejects_pinned_profile_that_violates_required_provider_kind() {
        // The reload-race shape: the guard verified a profile name while it
        // was ollama-kind, but by dispatch time the config defines that name
        // as a REMOTE profile. The dispatch-time lock must refuse.
        let mut config = LLMRouterConfig::default();
        config.default_profile = "local-ollama".to_string();
        config.profiles.insert(
            "local-ollama".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                // Same profile NAME, redefined to a remote provider.
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5")
            },
        );

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "distill_test".to_string();
        request.set_extra(
            serde_json::json!({
                "router_profile_override": "local-ollama",
                "router_required_provider_kind": "ollama",
            })
            .into(),
        );

        let err = router
            .route(request)
            .await
            .expect_err("provider-kind lock should refuse dispatch");
        assert!(matches!(err, LLMError::Configuration(_)));
        assert!(err.effective_route().is_none());
        assert!(err.to_string().contains("provider lock"));
        assert!(
            captured.lock().unwrap().is_empty(),
            "provider must not be invoked"
        );
    }

    #[tokio::test]
    async fn route_required_provider_kind_blocks_cross_provider_fallback_hop() {
        // No profile pin here — this proves the lock is enforced on EVERY
        // hop of the routing loop independently of `locked_profile`.
        let mut config = LLMRouterConfig::default();
        config.default_profile = "local-ollama".to_string();
        config
            .operation_mapping
            .insert("distill_test".to_string(), "local-ollama".into());
        config.profiles.insert(
            "local-ollama".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "fallback_profile".to_string(),
                    Value::String("remote-openai".to_string()),
                )])),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Ollama, "gemma3")
            },
        );
        config.profiles.insert(
            "remote-openai".to_string(),
            LLMProfile {
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5")
            },
        );

        let remote = Arc::new(CapturingProvider::new(LLMProviderKind::OpenAI));
        let remote_captured = remote.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::Ollama)));
        router.register_provider(remote);

        let mut request = sample_request("placeholder-model");
        request.metadata.operation = "distill_test".to_string();
        request.set_extra(
            serde_json::json!({
                "router_required_provider_kind": "ollama",
            })
            .into(),
        );

        let err = router
            .route(request)
            .await
            .expect_err("fallback hop to a remote provider must be refused");
        assert!(matches!(err.root_cause(), LLMError::Configuration(_)));
        assert_eq!(
            err.effective_route()
                .map(|(profile, provider, model)| { (profile, provider.clone(), model) }),
            Some(("local-ollama", LLMProviderKind::Ollama, "gemma3"))
        );
        assert!(err.to_string().contains("provider lock"));
        assert!(
            remote_captured.lock().unwrap().is_empty(),
            "the remote fallback provider must never be invoked"
        );
    }

    #[tokio::test]
    async fn physical_fallback_cold_starts_and_does_not_return_foreign_continuation() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "primary-openai".to_string();
        config.profiles.insert(
            "primary-openai".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([
                    (
                        "fallback_profile".to_string(),
                        Value::String("fallback-gemini".to_string()),
                    ),
                    (
                        "openai_api_mode".to_string(),
                        Value::String("responses".to_string()),
                    ),
                ])),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-stateful")
            },
        );
        config.profiles.insert(
            "fallback-gemini".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "gemini_api_mode".to_string(),
                    Value::String("interactions".to_string()),
                )])),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Gemini, "gemini-stateful")
            },
        );

        let fallback = Arc::new(
            CapturingProvider::new(LLMProviderKind::Gemini)
                .with_response_id("gemini-response-must-not-escape"),
        );
        let fallback_requests = fallback.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));
        router.register_provider(fallback);

        let mut request = sample_request("gpt-stateful");
        let openai_metadata = HashMap::from([
            (
                "fallback_profile".to_string(),
                Value::String("fallback-gemini".to_string()),
            ),
            (
                "openai_api_mode".to_string(),
                Value::String("responses".to_string()),
            ),
        ]);
        let mut reuse =
            crate::context_reuse::ContextReuseConfig::new(ContextReuseStrategy::ServerContinuation);
        reuse.continuation_id = Some("openai-response".to_string());
        let openai_cohort = transport_cohort_fingerprint(
            &LLMProviderKind::OpenAI,
            "gpt-stateful",
            None,
            Some(&openai_metadata),
        );
        reuse.transport_cohort_fingerprint = Some(openai_cohort.clone());
        request.set_context_reuse(reuse);

        let response = router
            .route(request)
            .await
            .expect("fallback should succeed");
        assert_eq!(response.response_id, None);
        assert_eq!(
            response
                .route_identity
                .as_ref()
                .map(|route| route.provider.clone()),
            Some(LLMProviderKind::Gemini)
        );

        let captured = fallback_requests.lock().unwrap();
        let fallback_request = captured.last().expect("fallback request");
        let fallback_reuse = fallback_request
            .context_reuse
            .as_ref()
            .expect("typed context plan");
        assert_eq!(
            fallback_reuse.strategy,
            ContextReuseStrategy::ServerContinuation
        );
        assert_eq!(fallback_reuse.continuation_id, None);
        assert!(fallback_reuse.transport_cohort_fingerprint.is_some());
        assert_ne!(
            fallback_reuse.transport_cohort_fingerprint.as_deref(),
            Some(openai_cohort.as_str())
        );
    }

    #[tokio::test]
    async fn fallback_preflight_failure_retains_the_last_physical_attempt_route() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "primary".to_string();
        config.profiles.insert(
            "primary".to_string(),
            LLMProfile {
                supports_reasoning: Some(true),
                metadata: Some(HashMap::from([(
                    "fallback_profile".to_string(),
                    Value::String("fallback-no-reasoning".to_string()),
                )])),
                ..sample_profile(LLMProviderKind::OpenAI, "primary-model")
            },
        );
        config.profiles.insert(
            "fallback-no-reasoning".to_string(),
            LLMProfile {
                supports_reasoning: Some(false),
                ..sample_profile(LLMProviderKind::OpenAI, "fallback-model")
            },
        );
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(Arc::new(FailingProvider::new(LLMProviderKind::OpenAI)));
        let mut request = sample_request("placeholder-model");
        request.reasoning = Some(ReasoningConfig {
            effort: Some("high".to_string()),
            ..ReasoningConfig::default()
        });
        let counter = request.metadata.ensure_provider_attempt_counter();

        let error = router
            .route(request)
            .await
            .expect_err("fallback capability check must fail");

        assert!(matches!(
            error.root_cause(),
            LLMError::UnsupportedCapability(_)
        ));
        assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            error.effective_route().map(|(profile, provider, model)| (
                profile,
                provider.clone(),
                model
            )),
            Some(("primary", LLMProviderKind::OpenAI, "primary-model"))
        );
    }

    #[test]
    fn validate_config_rejects_invalid_openai_api_mode() {
        let mut config = LLMRouterConfig::default();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "openai_api_mode".to_string(),
                    Value::String("invalid".to_string()),
                )])),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra")
            },
        );

        let err = validate_config(&config).unwrap_err();
        assert!(matches!(err, LLMError::Configuration(_)));
    }

    #[test]
    fn validate_config_rejects_invalid_operation_catalog_metadata() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            sample_profile(LLMProviderKind::Ollama, "local-model"),
        );
        config.operation_mapping.insert(
            "documented_operation".to_string(),
            crate::config::OperationProfileSelector::Conditional {
                default: "default".to_string(),
                when_has_images: None,
                when_cloud: None,
                description: Some("\n".to_string()),
                group: Some("Planning".to_string()),
                engine: None,
            },
        );

        let error = validate_config(&config).expect_err("empty metadata must fail closed");
        assert!(
            matches!(error, LLMError::Configuration(message) if message.contains("documented_operation") && message.contains("description"))
        );
    }

    #[test]
    fn validate_config_accepts_explicit_gemini_api_modes() {
        for mode in ["generate_content", "interactions"] {
            let mut config = LLMRouterConfig::default();
            config.default_profile = "default".to_string();
            config.profiles.insert(
                "default".to_string(),
                LLMProfile {
                    metadata: Some(HashMap::from([(
                        "gemini_api_mode".to_string(),
                        Value::String(mode.to_string()),
                    )])),
                    ..sample_profile(LLMProviderKind::Gemini, "gemini-3-flash-preview")
                },
            );

            validate_config(&config).expect("documented Gemini API modes must validate");
        }
    }

    #[test]
    fn validate_config_accepts_server_web_search_on_supporting_transports() {
        for (provider, model, extra_metadata) in [
            (
                LLMProviderKind::OpenAI,
                "gpt-5.6-luna",
                vec![("openai_api_mode", "responses")],
            ),
            (LLMProviderKind::Anthropic, "claude-sonnet-4-6", vec![]),
            (LLMProviderKind::Gemini, "gemini-3.1-pro-preview", vec![]),
            (LLMProviderKind::OpenRouter, "openai/gpt-4o-mini", vec![]),
        ] {
            let label = format!("{provider}");
            let mut metadata: HashMap<String, Value> = HashMap::from([(
                "server_web_search".to_string(),
                serde_json::json!({"max_uses": 3}),
            )]);
            for (key, value) in extra_metadata {
                metadata.insert(key.to_string(), Value::String(value.to_string()));
            }
            let mut config = LLMRouterConfig::default();
            config.default_profile = "default".to_string();
            config.profiles.insert(
                "default".to_string(),
                LLMProfile {
                    metadata: Some(metadata),
                    ..sample_profile(provider, model)
                },
            );
            validate_config(&config)
                .unwrap_or_else(|err| panic!("{label} must accept server_web_search: {err}"));
        }
    }

    #[test]
    fn validate_config_rejects_server_web_search_on_unsupported_providers() {
        for provider in [
            LLMProviderKind::DeepSeek,
            LLMProviderKind::Minimax,
            LLMProviderKind::Ollama,
            LLMProviderKind::Yutori,
        ] {
            let label = format!("{provider}");
            let mut config = LLMRouterConfig::default();
            config.default_profile = "default".to_string();
            config.profiles.insert(
                "default".to_string(),
                LLMProfile {
                    metadata: Some(HashMap::from([(
                        "server_web_search".to_string(),
                        Value::Bool(true),
                    )])),
                    ..sample_profile(provider, "some-model")
                },
            );
            let err = validate_config(&config).unwrap_err();
            assert!(
                err.to_string()
                    .contains("no server-side web search transport"),
                "{label}: {err}"
            );
        }
    }

    #[test]
    fn validate_config_rejects_server_web_search_malformed_or_chat_pinned() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([(
                    "server_web_search".to_string(),
                    Value::String("yes".to_string()),
                )])),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-luna")
            },
        );
        let err = validate_config(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("non-bool/object `server_web_search`"));

        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([
                    (
                        "openai_api_mode".to_string(),
                        Value::String("chat".to_string()),
                    ),
                    ("server_web_search".to_string(), Value::Bool(true)),
                ])),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-luna")
            },
        );
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("openai_api_mode: chat"));
    }

    #[test]
    fn validate_config_rejects_invalid_or_misplaced_gemini_api_mode() {
        for (provider, mode) in [
            (LLMProviderKind::Gemini, "invalid"),
            (LLMProviderKind::Anthropic, "interactions"),
        ] {
            let mut config = LLMRouterConfig::default();
            config.default_profile = "default".to_string();
            config.profiles.insert(
                "default".to_string(),
                LLMProfile {
                    metadata: Some(HashMap::from([(
                        "gemini_api_mode".to_string(),
                        Value::String(mode.to_string()),
                    )])),
                    ..sample_profile(provider, "test-model")
                },
            );

            let err = validate_config(&config).unwrap_err();
            assert!(matches!(err, LLMError::Configuration(_)));
        }
    }

    #[test]
    fn validate_config_rejects_streaming_gemini_interactions_profile() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                metadata: Some(HashMap::from([
                    (
                        "gemini_api_mode".to_string(),
                        Value::String("interactions".to_string()),
                    ),
                    ("streaming".to_string(), Value::Bool(true)),
                ])),
                ..sample_profile(LLMProviderKind::Gemini, "gemini-3.6-flash")
            },
        );

        let error = validate_config(&config).expect_err("unsupported wire mode must fail closed");
        assert!(matches!(error, LLMError::Configuration(_)));
        assert!(error.to_string().contains("Interactions SSE adapter"));
    }

    #[test]
    fn validate_config_rejects_computer_use_on_unsupported_provider() {
        let mut config = LLMRouterConfig::default();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                supports_computer_use: Some(true),
                ..sample_profile(LLMProviderKind::Ollama, "llama3")
            },
        );

        let err = validate_config(&config).unwrap_err();
        assert!(matches!(err, LLMError::Configuration(_)));
    }

    #[test]
    fn validate_config_allows_minimax_reasoning_profiles() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                supports_reasoning: Some(true),
                supports_tool_calling: Some(true),
                ..sample_profile(LLMProviderKind::Minimax, "MiniMax-M2.7")
            },
        );

        validate_config(&config).expect("minimax reasoning profile should validate");
    }

    #[test]
    fn validate_config_rejects_enabled_reasoning_default_on_disabled_profile() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                supports_reasoning: Some(false),
                reasoning: Some(crate::config::ReasoningDefaults {
                    effort: "medium".to_string(),
                    max_reasoning_tokens: None,
                    strategy: None,
                    summary: None,
                }),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra")
            },
        );

        let error = validate_config(&config).expect_err("contradictory profile must fail closed");
        assert!(matches!(error, LLMError::Configuration(_)));
        assert!(error.to_string().contains("disables reasoning support"));
    }

    #[test]
    fn validate_config_allows_explicit_disabled_reasoning_default() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                supports_reasoning: Some(false),
                reasoning: Some(crate::config::ReasoningDefaults {
                    effort: "none".to_string(),
                    max_reasoning_tokens: None,
                    strategy: None,
                    summary: None,
                }),
                ..sample_profile(LLMProviderKind::OpenAI, "gpt-5.6-terra")
            },
        );

        validate_config(&config).expect("explicit disabled default is coherent");
    }

    /// DeepSeek vision is a property of the MODEL, not the provider.
    ///
    /// V4.1 Flash (`deepseek-flash`) and retired Flash aliases are multimodal.
    /// V4 Pro stays text-only until DeepSeek retires that name.
    #[test]
    fn deepseek_vision_is_allowed_only_on_the_multimodal_model() {
        let vision_profile = |model: &str| {
            let mut config = LLMRouterConfig::default();
            config.default_profile = "default".to_string();
            config.profiles.insert(
                "default".to_string(),
                LLMProfile {
                    supports_vision: Some(true),
                    ..sample_profile(LLMProviderKind::DeepSeek, model)
                },
            );
            config
        };

        validate_config(&vision_profile("deepseek-flash")).expect("V4.1 Flash may declare vision");
        validate_config(&vision_profile("deepseek-v4-flash-vision-exp"))
            .expect("retired Flash Vision alias is served by V4.1 Flash");
        validate_config(&vision_profile("deepseek-v4-flash"))
            .expect("retired V4 Flash alias is served by V4.1 Flash");

        let error = validate_config(&vision_profile("deepseek-v4-pro"))
            .expect_err("V4 Pro must not claim vision while the name still exists");
        assert!(matches!(error, LLMError::Configuration(_)));
        assert!(
            error.to_string().contains("deepseek-v4-pro"),
            "the failure must name the model: {error}"
        );
    }

    #[test]
    fn validate_config_rejects_mismatched_typed_and_ollama_context() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "default".to_string();
        config.profiles.insert(
            "default".to_string(),
            LLMProfile {
                context_window_tokens: Some(32_768),
                metadata: Some(HashMap::from([(
                    "options".to_string(),
                    serde_json::json!({"num_ctx": 24_576}),
                )])),
                ..sample_profile(LLMProviderKind::Ollama, "gemma4:12b")
            },
        );

        let error = validate_config(&config).expect_err("mismatch must fail");
        assert!(matches!(error, LLMError::Configuration(_)));
        assert!(error.to_string().contains("options.num_ctx=24576"));
    }

    #[test]
    fn logical_chunk_deadline_cannot_undercut_a_physical_provider_call() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "local".to_string();
        config.profiles.insert(
            "local".to_string(),
            LLMProfile {
                timeout_secs: Some(300),
                context_window_tokens: Some(32_768),
                chunking: Some(crate::config::ChunkingConfig {
                    enabled: true,
                    adapter: Some("memory_archive_v1".to_string()),
                    logical_window_tokens: Some(262_144),
                    target_payload_tokens: Some(24_576),
                    logical_timeout_secs: Some(299),
                    ..Default::default()
                }),
                ..sample_profile(LLMProviderKind::Ollama, "gemma4:12b")
            },
        );

        let error = validate_config(&config).expect_err("inverted timeouts must fail closed");
        assert!(error
            .to_string()
            .contains("cannot be shorter than its physical timeout"));
        config
            .profiles
            .get_mut("local")
            .unwrap()
            .chunking
            .as_mut()
            .unwrap()
            .logical_timeout_secs = Some(1_200);
        validate_config(&config).expect("a larger logical deadline is valid");
    }

    #[tokio::test]
    async fn disabled_chunking_observes_overflow_and_preserves_provider_request() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "local".to_string();
        config.profiles.insert(
            "local".to_string(),
            LLMProfile {
                context_window_tokens: Some(64),
                chunking: Some(crate::config::ChunkingConfig {
                    enabled: false,
                    safety_margin_tokens: 8,
                    ..Default::default()
                }),
                max_output_tokens: Some(32),
                metadata: Some(HashMap::from([
                    ("context_window_tokens".to_string(), Value::from(64)),
                    (
                        "chunking".to_string(),
                        serde_json::json!({"enabled": false}),
                    ),
                ])),
                ..sample_profile(LLMProviderKind::Ollama, "gemma4:12b")
            },
        );

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::Ollama));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("ignored");
        request.messages = vec![LLMMessage::user("x".repeat(256))].into();
        router
            .route(request)
            .await
            .expect("observe mode must preserve existing dispatch");

        let requests = captured.lock().unwrap();
        let delivered = requests.last().expect("provider request");
        assert_eq!(delivered.model, "gemma4:12b");
        assert!(matches!(
            delivered.messages.as_slice(),
            [LLMMessage {
                role: MessageRole::User,
                content,
            }] if matches!(content.as_slice(), [ContentBlock::Text { text }] if text == &"x".repeat(256))
        ));
        assert!(delivered
            .extra
            .as_ref()
            .and_then(|extra| extra.get("context_window_tokens"))
            .is_none());
        assert!(delivered
            .extra
            .as_ref()
            .and_then(|extra| extra.get("chunking"))
            .is_none());
    }

    #[tokio::test]
    async fn enabled_chunking_enforces_physical_overflow_before_provider() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "local".to_string();
        config.profiles.insert(
            "local".to_string(),
            LLMProfile {
                context_window_tokens: Some(64),
                chunking: Some(crate::config::ChunkingConfig {
                    enabled: true,
                    adapter: Some("fixture_adapter_v1".to_string()),
                    logical_window_tokens: Some(256),
                    target_payload_tokens: Some(32),
                    safety_margin_tokens: 8,
                    ..Default::default()
                }),
                max_output_tokens: Some(16),
                ..sample_profile(LLMProviderKind::Ollama, "gemma4:12b")
            },
        );

        let provider = Arc::new(CapturingProvider::new(LLMProviderKind::Ollama));
        let captured = provider.requests.clone();
        let mut router = MultiLLMRouter::new(config).expect("router");
        router.register_provider(provider);

        let mut request = sample_request("ignored");
        request.messages = vec![LLMMessage::user("x".repeat(256))].into();
        let error = router
            .route(request)
            .await
            .expect_err("enforce mode must reject overflow");

        assert!(matches!(
            error,
            LLMError::Context(crate::chunking::ContextError::PhysicalContextWindowExceeded { .. })
        ));
        assert!(captured.lock().unwrap().is_empty());
    }
}
