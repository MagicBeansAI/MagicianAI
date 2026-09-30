//! The pinned local-LLM dispatch seam, shared by channel-assist distillation
//! and lib-side memory applicability judging. One place resolves an
//! operation's verified local binding and dispatches pinned to it; extracted
//! from `channel_assist::assist::distill` (comms-crate extraction
//! prerequisite).

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};
use crate::magician_v2::realtime_events::{
    LlmEventCorrelation, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};
use magicllm::LLMProviderKind;

/// Why distillation cannot run. Content is never fetched and no LLM is
/// called under ANY variant. EVERY variant now preserves the backlog: an
/// unavailable or misrouted distiller means idle, and a durable queue is
/// never destroyed to signal a configuration state. (`NonLocalProvider`
/// used to drain rows to terminal `skipped`; under a locality *policy* the
/// same state means someone hand-edited config against the selected mode,
/// and the loud warning carries that signal without data loss.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistillUnavailable {
    /// No operation router is wired at all (boot without LLM config).
    /// Idle: backlog preserved.
    RouterUnavailable,
    /// `channel_ingest_distill` has no explicit `operation_mapping`
    /// binding (or the bound profile name is absent from `profiles`).
    /// The router's default-profile fallback deliberately does not
    /// count: unbound is OFF. Idle: backlog preserved.
    OperationUnbound,
    /// The bound profile's provider kind is not the local (ollama)
    /// family. Carries the offending kind's config identifier.
    /// Degraded: rows drain to `skipped`.
    NonLocalProvider(String),
}

/// The guard's positive verdict: the exact profile it verified as local.
/// Dispatch is PINNED to this — [`RouterDistillLlm`] forwards `profile`
/// as the magicllm `router_profile_override` (locked profile, no
/// re-resolution, no fallback traversal) and `kind` as the
/// `router_required_provider_kind` dispatch-time provider lock. The
/// guard's config read is thereby the only one that matters: dispatch
/// either uses this binding on an Ollama provider or refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedLocalBinding {
    /// Profile name explicitly bound to `channel_ingest_distill`.
    pub profile: String,
    /// Always [`LLMProviderKind::Ollama`] by construction.
    pub kind: LLMProviderKind,
}

/// Require an explicit binding, without requiring it to be local.
///
/// **Unbound is still OFF** — the router's `default_profile` is never
/// consulted, so enabling a consumer of this remains a deliberate second act
/// rather than something that inherits whatever the default happens to be.
/// What this drops is the *locality* requirement.
///
/// That distinction matters because locality is only a real boundary when the
/// input has not already been sent to a model. It is, for channel ingest:
/// email bodies arrive from the provider and reach no LLM unless this one
/// reads them, so keeping it on-host is a genuine privacy property. It is not,
/// for a chat transcript — that content went to whichever model conducted the
/// session, turn by turn, before any distillation existed. Refusing a remote
/// distiller there protects nothing and only costs quality.
///
/// The returned type is still `VerifiedLocalBinding` for compatibility with
/// the pinning path; when reached through this function the `kind` may be any
/// provider, and the name is historical.
pub fn resolve_bound_provider_for_operation(
    router: Option<&OperationLlmRouter>,
    operation: &str,
) -> Result<VerifiedLocalBinding, DistillUnavailable> {
    let Some(router) = router else {
        return Err(DistillUnavailable::RouterUnavailable);
    };
    match router.explicit_binding_for_operation(operation) {
        None => Err(DistillUnavailable::OperationUnbound),
        Some((profile, kind)) => Ok(VerifiedLocalBinding { profile, kind }),
    }
}

/// The single seam through which distill prompts reach a model. Tests
/// inject canned implementations; production has exactly ONE
/// implementation ([`RouterDistillLlm`]), which is only constructed by
/// the worker and only invoked by [`run_distill_pass`] after the guard
/// passed — and which re-runs the guard itself before every dispatch.
#[async_trait]
pub trait DistillLlm: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String>;

    /// Complete while constraining the provider to the managed distillation
    /// JSON schema. Test doubles and legacy implementations retain a safe
    /// default; the production Ollama path overrides this and forwards the
    /// schema natively.
    async fn complete_with_response_schema(
        &self,
        system: &str,
        user: &str,
        response_schema: &Value,
    ) -> Result<String> {
        let _ = response_schema;
        self.complete(system, user).await
    }
}

/// The seam under [`RouterDistillLlm`]: verify the locality guard, then
/// dispatch PINNED to the verified binding. Splitting verification from
/// dispatch lets tests assert (with a spy) that `complete()` forwards the
/// exact binding the guard returned — the pin is the privacy property, so
/// it gets its own regression seam. Production has exactly one
/// implementation ([`RouterPinnedDispatch`]).
#[async_trait]
pub trait PinnedLocalDispatch: Send + Sync {
    /// Re-run the fail-closed guard against the live config.
    fn verify_binding(&self) -> Result<VerifiedLocalBinding, DistillUnavailable>;

    /// Dispatch one completion pinned to `binding` (profile pin + provider
    /// lock — see the module header's layer 2 and 3).
    async fn dispatch_pinned(
        &self,
        binding: &VerifiedLocalBinding,
        system: &str,
        user: &str,
    ) -> Result<String>;

    async fn dispatch_pinned_with_response_schema(
        &self,
        binding: &VerifiedLocalBinding,
        system: &str,
        user: &str,
        response_schema: &Value,
    ) -> Result<String> {
        let _ = response_schema;
        self.dispatch_pinned(binding, system, user).await
    }
}

#[async_trait]
impl PinnedLocalDispatch for RouterPinnedDispatch {
    fn verify_binding(&self) -> Result<VerifiedLocalBinding, DistillUnavailable> {
        if self.require_local {
            resolve_local_provider_for_operation(Some(self.router.as_ref()), &self.operation)
        } else {
            resolve_bound_provider_for_operation(Some(self.router.as_ref()), &self.operation)
        }
    }

    async fn dispatch_pinned(
        &self,
        binding: &VerifiedLocalBinding,
        system: &str,
        user: &str,
    ) -> Result<String> {
        self.dispatch(binding, system, user, None).await
    }

    async fn dispatch_pinned_with_response_schema(
        &self,
        binding: &VerifiedLocalBinding,
        system: &str,
        user: &str,
        response_schema: &Value,
    ) -> Result<String> {
        self.dispatch(binding, system, user, Some(response_schema))
            .await
    }
}

/// Production [`DistillLlm`]: re-runs the guard immediately before every
/// dispatch and pins the dispatch to the binding THAT guard run verified
/// (never a cached one, never the operation's re-resolved default). The
/// pin travels as magicllm's `router_profile_override` (locked profile:
/// no re-resolution, no `fallback_profile` traversal) plus
/// `router_required_provider_kind: ollama` (dispatch-time provider lock,
/// enforced inside magicllm against the dispatching config snapshot on
/// every hop) — see the module header's three layers.
pub struct RouterDistillLlm {
    pub dispatch: Arc<dyn PinnedLocalDispatch>,
}

impl RouterDistillLlm {
    pub fn new(
        router: Arc<OperationLlmRouter>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self::new_for_operation(
            router,
            broadcaster,
            principal,
            workspace,
            "channel_ingest_distill",
            true,
        )
    }

    /// Same dispatcher, pinned to a caller-chosen operation.
    ///
    /// `require_local` must stay true for any consumer whose input has not
    /// already been sent to a model; see
    /// [`resolve_bound_provider_for_operation`] for why that is the deciding
    /// question rather than sensitivity alone.
    pub fn new_for_operation(
        router: Arc<OperationLlmRouter>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        operation: impl Into<String>,
        require_local: bool,
    ) -> Self {
        let operation = operation.into();
        let principal = principal.into();
        let workspace = workspace.into();
        Self {
            dispatch: Arc::new(RouterPinnedDispatch {
                operation,
                require_local,
                router: Arc::new(router.with_scope_context(Some(magicllm::LlmScope::new(
                    principal.clone(),
                    workspace.clone(),
                )))),
                broadcaster,
                principal,
                workspace,
            }),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_dispatch(dispatch: Arc<dyn PinnedLocalDispatch>) -> Self {
        Self { dispatch }
    }
}

/// Production seam: guard + pinned dispatch through the operation router.
/// Holds the broadcaster + scope so each call emits `LLMResponseReceived`
/// telemetry (the mail ops bypass the executor layer that normally emits — see
/// `super::telemetry`).
pub struct RouterPinnedDispatch {
    pub router: Arc<OperationLlmRouter>,
    pub broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    pub principal: String,
    pub workspace: String,
    /// Which operation this dispatcher pins to. Carried rather than hardcoded
    /// so a second consumer reuses the guard instead of copying it.
    pub operation: String,
    /// Whether the binding must be a local (Ollama) provider. True for channel
    /// ingest, whose input never reached an LLM before this. False for
    /// consumers whose input already went to a model, where refusing a remote
    /// binding buys nothing.
    pub require_local: bool,
}

impl RouterPinnedDispatch {
    async fn dispatch(
        &self,
        binding: &VerifiedLocalBinding,
        system: &str,
        user: &str,
        response_schema: Option<&Value>,
    ) -> Result<String> {
        let operation = LLMOperation::Other(self.operation.clone());
        let started = std::time::Instant::now();
        let response = match response_schema {
            Some(schema) => {
                self.router
                    .generate_for_operation_with_system_pinned_and_response_format(
                        &operation,
                        Some(system),
                        user,
                        &binding.profile,
                        Some(binding.kind.clone()),
                        magicllm::LLMResponseFormat::JsonSchema {
                            schema: schema.clone(),
                        },
                    )
                    .await
            },
            None => {
                // JSON enforcement parity across locality arms: the local
                // Ollama profiles get provider-enforced JSON from
                // `metadata.format: json`; a remote `when_cloud` arm has no
                // such metadata, so the format must ride the request. Fence-
                // tolerant parsers make a missing format degrade-not-break,
                // which is exactly why it is set explicitly here.
                let response_format = (binding.kind != magicllm::LLMProviderKind::Ollama)
                    .then(|| magicllm::LLMResponseFormat::JsonObject);
                match response_format {
                    Some(format) => {
                        self.router
                            .generate_for_operation_with_system_pinned_and_response_format(
                                &operation,
                                Some(system),
                                user,
                                &binding.profile,
                                Some(binding.kind.clone()),
                                format,
                            )
                            .await
                    },
                    None => {
                        self.router
                            .generate_for_operation_with_system_pinned(
                                &operation,
                                Some(system),
                                user,
                                &binding.profile,
                                Some(binding.kind.clone()),
                            )
                            .await
                    },
                }
            },
        }
        .context("channel ingest distill LLM call failed")?;
        // Emit AFTER the guarded dispatch (post-response) — the pin/guard is
        // untouched; this only records what already happened.
        emit_mail_llm_call(
            self.broadcaster.as_ref(),
            &self.operation,
            &self.principal,
            &self.workspace,
            &response,
            true,
            started.elapsed().as_millis() as u64,
        );
        Ok(response.content)
    }
}

/// Emit an `LLMResponseReceived` for one channel-op router call. No-op when the
/// broadcaster is absent (tests / no runtime) or the response carried no
/// telemetry. `fallback_operation` is used only if the router response didn't
/// Emit an `LLMResponseReceived` for one channel-op router call. No-op when the
/// broadcaster is absent (tests / no runtime) or the response carried no
/// telemetry. `fallback_operation` is used only if the router response didn't
/// Emit an `LLMResponseReceived` for one channel-op router call. No-op when the
/// broadcaster is absent (tests / no runtime) or the response carried no
/// telemetry. `fallback_operation` is used only if the router response didn't
/// Emit an `LLMResponseReceived` for one channel-op router call. No-op when the
/// broadcaster is absent (tests / no runtime) or the response carried no
/// telemetry. `fallback_operation` is used only if the router response didn't
/// stamp its own operation.
pub fn emit_mail_llm_call(
    broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    fallback_operation: &str,
    principal: &str,
    workspace: &str,
    response: &SimplifiedLLMResponse,
    success: bool,
    latency_ms: u64,
) {
    let Some(broadcaster) = broadcaster else {
        return;
    };
    let Some(tel) = response.telemetry.as_ref() else {
        return;
    };
    broadcaster.emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
        execution_id: format!("channel:{fallback_operation}"),
        principal: Some(principal.to_string()),
        workspace: Some(workspace.to_string()),
        correlation: tel.trace_receipt.as_ref().and_then(|receipt| {
            LlmEventCorrelation::scoped(
                receipt,
                principal,
                workspace,
                magicllm::LlmWorkloadClass::CommsAssist,
            )
        }),
        plan_id: String::new(),
        step_id: None,
        step_index: None,
        capability: "channel_assist".to_string(),
        success,
        decision_summary: String::new(),
        cost: tel.cost_usd,
        latency_ms,
        error: None,
        provider: tel.provider.clone(),
        model: tel.model.clone(),
        usage_reported: tel.usage_reported,
        input_tokens: tel.input_tokens,
        output_tokens: tel.output_tokens,
        reasoning_tokens: tel.reasoning_tokens,
        reasoning_summary: tel.reasoning_summary.clone(),
        cache_read_tokens: tel.cache_read_tokens,
        cache_creation_tokens: tel.cache_creation_tokens,
        audio_input_tokens: None,
        audio_output_tokens: None,
        audio_cached_tokens: None,
        search_calls: 0,
        ttft_ms: None,
        task_id: None,
        agent_id: None,
        delegated_agent_id: None,
        chat_session_id: None,
        operation: tel
            .operation
            .clone()
            .unwrap_or_else(|| fallback_operation.to_string()),
        profile: tel.profile.clone(),
        attempt: 1,
        response_kind: "text".to_string(),
        started_at_ms: tel.started_at_ms,
        timestamp: Utc::now().timestamp_millis(),
    });
}

#[async_trait]
impl DistillLlm for RouterDistillLlm {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let binding = self
            .dispatch
            .verify_binding()
            .map_err(|reason| anyhow!("distill guard refused dispatch: {reason}"))?;
        self.dispatch.dispatch_pinned(&binding, system, user).await
    }

    async fn complete_with_response_schema(
        &self,
        system: &str,
        user: &str,
        response_schema: &Value,
    ) -> Result<String> {
        let binding = self
            .dispatch
            .verify_binding()
            .map_err(|reason| anyhow!("distill guard refused dispatch: {reason}"))?;
        self.dispatch
            .dispatch_pinned_with_response_schema(&binding, system, user, response_schema)
            .await
    }
}

impl std::fmt::Display for DistillUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RouterUnavailable => f.write_str("no operation LLM router available"),
            Self::OperationUnbound => write!(
                f,
                "operation is not bound to a profile in \
                 operation_mapping (unbound = distillation off)"
            ),
            Self::NonLocalProvider(kind) => write!(
                f,
                "operation is bound to provider kind \
                 '{kind}', which is not the local (ollama) family — refusing to send message \
                 content to a remote provider"
            ),
        }
    }
}

/// The same guard, for any operation that must stay local.
///
/// Extracted so a second local-only consumer pins its own operation through
/// *this* check rather than growing a parallel copy. Two implementations of a
/// locality guard drift, and the one that drifts is the one that stops
/// refusing — so the operation name is a parameter and the enforcement is not.
pub fn resolve_local_provider_for_operation(
    router: Option<&OperationLlmRouter>,
    operation: &str,
) -> Result<VerifiedLocalBinding, DistillUnavailable> {
    let Some(router) = router else {
        return Err(DistillUnavailable::RouterUnavailable);
    };
    let mode = router.processing_locality();
    let binding = router.explicit_binding_for_operation(operation);
    require_permitted_provider(
        mode,
        binding
            .as_ref()
            .map(|(profile, kind)| (profile.as_str(), kind)),
    )
}

/// PURE guard core, split out so tests can drive it with mocked bindings:
/// `None` = the operation is unbound; only [`LLMProviderKind::Ollama`]
/// (magicllm's local-inference family) passes, yielding the verified
/// binding the dispatch must pin to. This is the `local` half of
/// [`require_permitted_provider`] and stays exactly the local-only guard the
/// existing tests pin.
pub fn require_local_provider(
    binding: Option<(&str, &LLMProviderKind)>,
) -> Result<VerifiedLocalBinding, DistillUnavailable> {
    match binding {
        None => Err(DistillUnavailable::OperationUnbound),
        Some((profile, LLMProviderKind::Ollama)) => Ok(VerifiedLocalBinding {
            profile: profile.to_string(),
            kind: LLMProviderKind::Ollama,
        }),
        Some((_, other)) => Err(DistillUnavailable::NonLocalProvider(
            other.as_str().to_string(),
        )),
    }
}

/// Policy-aware guard: the same explicit-binding requirement as
/// [`require_local_provider`], but the locality decision is the operator's
/// `privacy.processing.mode` rather than a Rust constant. `local` behaves
/// exactly like [`require_local_provider`] (non-Ollama refuses);
/// `cloud` accepts the bound provider of any kind — the `when_cloud` arm
/// the binding was resolved from. Unbound is still OFF in both modes: the
/// router default is never consulted.
pub fn require_permitted_provider(
    mode: magicllm::ProcessingLocality,
    binding: Option<(&str, &LLMProviderKind)>,
) -> Result<VerifiedLocalBinding, DistillUnavailable> {
    match mode {
        magicllm::ProcessingLocality::Local => require_local_provider(binding),
        magicllm::ProcessingLocality::Cloud => match binding {
            None => Err(DistillUnavailable::OperationUnbound),
            Some((profile, kind)) => Ok(VerifiedLocalBinding {
                profile: profile.to_string(),
                kind: kind.clone(),
            }),
        },
    }
}

impl DistillUnavailable {
    /// Whether queued rows should be preserved as `pending` (idle) rather
    /// than drained to `skipped` (degraded). Every variant preserves: see
    /// the enum docs for why a policy-era misrouting no longer destroys a
    /// durable queue.
    pub fn preserves_backlog(&self) -> bool {
        true
    }
}

/// Locality-policy guard matrix: {local, cloud} × {unbound, ollama, openai}.
/// The `local` column is `require_local_provider` unchanged; the `cloud`
/// column accepts the provider the policy selected. Every refusal preserves
/// the backlog.
#[cfg(any(test, feature = "test-fixtures"))]
mod locality_policy_tests {
    use super::*;
    use magicllm::ProcessingLocality;

    #[test]
    fn guard_matrix_local_mode_is_the_unchanged_local_only_guard() {
        assert_eq!(
            require_permitted_provider(ProcessingLocality::Local, None),
            Err(DistillUnavailable::OperationUnbound)
        );
        assert_eq!(
            require_permitted_provider(
                ProcessingLocality::Local,
                Some(("p-local", &LLMProviderKind::Ollama))
            ),
            Ok(VerifiedLocalBinding {
                profile: "p-local".to_string(),
                kind: LLMProviderKind::Ollama,
            })
        );
        assert_eq!(
            require_permitted_provider(
                ProcessingLocality::Local,
                Some(("p-remote", &LLMProviderKind::OpenAI))
            ),
            Err(DistillUnavailable::NonLocalProvider("openai".to_string()))
        );
    }

    #[test]
    fn guard_matrix_cloud_mode_accepts_the_bound_provider_of_any_kind() {
        // Unbound is still OFF — the router default is never consulted.
        assert_eq!(
            require_permitted_provider(ProcessingLocality::Cloud, None),
            Err(DistillUnavailable::OperationUnbound)
        );
        // The when_cloud arm (remote) is accepted, and the verdict carries
        // the exact profile the dispatch must pin to.
        assert_eq!(
            require_permitted_provider(
                ProcessingLocality::Cloud,
                Some(("p-remote", &LLMProviderKind::OpenAI))
            ),
            Ok(VerifiedLocalBinding {
                profile: "p-remote".to_string(),
                kind: LLMProviderKind::OpenAI,
            })
        );
        // Ollama remains legal in cloud mode (an operation whose cloud arm
        // intentionally stays local, or a mixed install).
        assert!(require_permitted_provider(
            ProcessingLocality::Cloud,
            Some(("p-local", &LLMProviderKind::Ollama))
        )
        .is_ok());
    }

    #[test]
    fn every_refusal_preserves_the_backlog() {
        for refusal in [
            DistillUnavailable::RouterUnavailable,
            DistillUnavailable::OperationUnbound,
            DistillUnavailable::NonLocalProvider("openai".to_string()),
        ] {
            assert!(refusal.preserves_backlog(), "{refusal:?} must preserve");
        }
    }
}
