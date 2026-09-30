//! Local-only reply drafter (Channel Assist Phase 2, iMessage Assist Task 3).
//!
//! Given the message being replied to plus the locally-distilled thread
//! context (and an optional owner redraft hint), this op drafts a SHORT,
//! natural, first-person reply the owner reviews before sending.
//!
//! # Local-pinned, like the distiller — not body-blind like the classifier
//!
//! The drafter is shown the RAW `latest_message` being replied to (the
//! classifier, by contrast, sees only local summaries and may run remote).
//! So this op is pinned to the local (ollama) family exactly like
//! [`super::distill`]: [`RouterReplyDraftLlm::bound`] is true only when
//! `channel_reply_draft` is EXPLICITLY bound in `operation_mapping` to an
//! Ollama-kind profile; anything else (unbound, or bound to a remote
//! provider) reports the drafter as unavailable and no content is ever sent
//! to a remote model. Every dispatch re-runs the guard and pins to the
//! verified binding (magicllm `router_profile_override` + provider lock),
//! mirroring the distiller's three-layer guarantee.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use magicllm::LLMProviderKind;

use magician::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use magician::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter,
};
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

/// Operation name the reply drafter resolves and dispatches through. Bound in
/// CONFIG (per the house config-swap rule) to a LOCAL (ollama) profile — see
/// the `channel_reply_draft` line in `operation_mapping`. Unbound (or bound to
/// a non-ollama profile) ⇒ the drafter is unavailable.
pub const CHANNEL_REPLY_DRAFT_OPERATION: &str = "channel_reply_draft";

// ---------------------------------------------------------------------------
// Fail-closed locality guard (mirrors distill::resolve_local_provider)
// ---------------------------------------------------------------------------

/// The guard's positive verdict: the exact profile it verified as local.
/// Dispatch is PINNED to this — [`RouterReplyDraftLlm`] forwards `profile` as
/// the magicllm `router_profile_override` (locked profile, no re-resolution)
/// and `kind` as the `router_required_provider_kind` dispatch-time provider
/// lock, so the raw message being replied to can only reach a local model.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedLocalBinding {
    profile: String,
    kind: LLMProviderKind,
}

/// PURE guard core: `None` = unbound; only [`LLMProviderKind::Ollama`]
/// (magicllm's local-inference family) passes. Production resolution now
/// routes through the shared policy-aware seam; this pure core remains as
/// the pinned local-mode unit contract.
#[cfg(any(test, feature = "test-fixtures"))]
fn require_local_binding(
    binding: Option<(&str, &LLMProviderKind)>,
) -> Option<VerifiedLocalBinding> {
    match binding {
        Some((profile, LLMProviderKind::Ollama)) => Some(VerifiedLocalBinding {
            profile: profile.to_string(),
            kind: LLMProviderKind::Ollama,
        }),
        _ => None,
    }
}

/// Resolve the fail-closed locality guard against the live router config.
/// `Some` ⇒ the operation is explicitly bound to a profile the current
/// `privacy.processing.mode` permits (Ollama under local, the `when_cloud`
/// arm under cloud) and the dispatch pins to; `None` ⇒ unbound or bound
/// against the mode (drafter unavailable — no content is ever sent to a
/// provider the policy has not chosen).
fn resolve_local_binding(router: &OperationLlmRouter) -> Option<VerifiedLocalBinding> {
    let verified = magician::magician_v2::llm_dispatch_seam::resolve_local_provider_for_operation(
        Some(router),
        CHANNEL_REPLY_DRAFT_OPERATION,
    )
    .ok()?;
    Some(VerifiedLocalBinding {
        profile: verified.profile,
        kind: verified.kind,
    })
}

// ---------------------------------------------------------------------------
// LLM seam
// ---------------------------------------------------------------------------

/// The single seam through which reply-draft prompts reach a model. Tests
/// inject canned implementations; production is [`RouterReplyDraftLlm`].
#[async_trait]
pub trait ReplyDraftLlm: Send + Sync {
    /// Whether `channel_reply_draft` is explicitly bound to a LOCAL (ollama)
    /// profile. `false` ⇒ the caller reports "draft unavailable" (no remote
    /// dispatch, ever).
    fn bound(&self) -> bool;

    /// Draft a reply for the given system/user prompts, returning the drafted
    /// reply text (the `draft` field of the model's strict-JSON reply).
    async fn draft(&self, system: &str, user: &str) -> Result<String>;
}

/// Production seam: local-pinned dispatch through the operation router. Holds
/// the broadcaster + scope so each call emits `LLMResponseReceived` telemetry
/// (the channel-assist ops bypass the executor layer that normally emits — see
/// `super::telemetry`).
pub struct RouterReplyDraftLlm {
    router: Arc<OperationLlmRouter>,
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    principal: String,
    workspace: String,
}

impl RouterReplyDraftLlm {
    pub fn new(
        router: Arc<OperationLlmRouter>,
        broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        let principal = principal.into();
        let workspace = workspace.into();
        Self {
            router: Arc::new(router.with_scope_context(Some(magicllm::LlmScope::new(
                principal.clone(),
                workspace.clone(),
            )))),
            broadcaster,
            principal,
            workspace,
        }
    }
}

#[async_trait]
impl ReplyDraftLlm for RouterReplyDraftLlm {
    fn bound(&self) -> bool {
        resolve_local_binding(self.router.as_ref()).is_some()
    }

    async fn draft(&self, system: &str, user: &str) -> Result<String> {
        // Re-run the guard immediately before dispatch and pin to the binding
        // THIS run verified (never the operation's re-resolved default).
        let binding = resolve_local_binding(self.router.as_ref()).ok_or_else(|| {
            anyhow!(
                "reply draft unavailable: operation '{CHANNEL_REPLY_DRAFT_OPERATION}' is not bound \
                 to a local (ollama) profile in operation_mapping"
            )
        })?;

        let operation = LLMOperation::Other(CHANNEL_REPLY_DRAFT_OPERATION.to_string());
        let started = std::time::Instant::now();
        // The reply draft is a strict-JSON reply: the local arm gets
        // provider-enforced JSON from `metadata.format: json`; a remote
        // `when_cloud` arm needs the format on the request itself.
        let response = if binding.kind == LLMProviderKind::Ollama {
            self.router
                .generate_for_operation_with_system_pinned(
                    &operation,
                    Some(system),
                    user,
                    &binding.profile,
                    Some(binding.kind.clone()),
                )
                .await
        } else {
            self.router
                .generate_for_operation_with_system_pinned_and_response_format(
                    &operation,
                    Some(system),
                    user,
                    &binding.profile,
                    Some(binding.kind.clone()),
                    magicllm::LLMResponseFormat::JsonObject,
                )
                .await
        }
        .context("channel reply draft LLM call failed")?;
        let parsed = parse_draft_json(&response.content);
        if let Some(broadcaster) = self.broadcaster.as_ref() {
            let telemetry = OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                &self.principal,
                &self.workspace,
                "channel_assist",
            );
            let latency_ms = started.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_validated_success(
                    CHANNEL_REPLY_DRAFT_OPERATION,
                    &response,
                    latency_ms,
                    OperationLlmCallAttribution::default(),
                    "channel_reply_draft_json",
                ),
                Err(error) => telemetry.emit_validation_failure(
                    CHANNEL_REPLY_DRAFT_OPERATION,
                    &response,
                    latency_ms,
                    OperationLlmCallAttribution::default(),
                    "channel_reply_draft_json",
                    &error.to_string(),
                ),
            }
        }
        parsed
    }
}

// ---------------------------------------------------------------------------
// Strict-JSON output parsing
// ---------------------------------------------------------------------------

/// Parse the model's reply as the strict `{"draft": "..."}` contract.
/// Tolerates prose/code-fence wrapping by slicing the outermost `{…}` (small
/// local models fence despite instructions). A missing/empty `draft` field is
/// an error (there is nothing to surface).
pub fn parse_draft_json(raw: &str) -> Result<String> {
    let json_slice = extract_json_object(raw)
        .ok_or_else(|| anyhow!("reply draft response contained no JSON object: {raw:?}"))?;
    let value: serde_json::Value = serde_json::from_str(json_slice)
        .with_context(|| format!("reply draft response was not valid JSON: {json_slice:?}"))?;
    let draft = value
        .get("draft")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("reply draft response missing string 'draft' field: {raw:?}"))?
        .trim();
    if draft.is_empty() {
        return Err(anyhow!("reply draft response had an empty 'draft' field"));
    }
    Ok(draft.to_string())
}

/// Slice the outermost `{…}` from a possibly fenced/prose-wrapped response.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&raw[start..=end])
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_draft() {
        assert_eq!(parse_draft_json(r#"{"draft":"hey!"}"#).unwrap(), "hey!");
    }

    #[test]
    fn parses_code_fenced_draft() {
        let raw = "```json\n{\n  \"draft\": \"Sounds good, see you Friday!\"\n}\n```";
        assert_eq!(
            parse_draft_json(raw).unwrap(),
            "Sounds good, see you Friday!"
        );
    }

    #[test]
    fn parses_draft_with_prose_wrapping() {
        let raw = "Sure, here is a reply: {\"draft\": \"Thanks, will do.\"} hope that helps";
        assert_eq!(parse_draft_json(raw).unwrap(), "Thanks, will do.");
    }

    #[test]
    fn errors_on_missing_draft_field() {
        let err = parse_draft_json(r#"{"reply":"hey!"}"#).unwrap_err();
        assert!(
            err.to_string().contains("missing string 'draft'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn errors_on_empty_draft() {
        assert!(parse_draft_json(r#"{"draft":"   "}"#).is_err());
    }

    #[test]
    fn errors_on_non_json() {
        assert!(parse_draft_json("no json here").is_err());
    }

    #[test]
    fn require_local_binding_accepts_only_ollama() {
        assert!(require_local_binding(Some(("p", &LLMProviderKind::Ollama))).is_some());
        assert!(require_local_binding(Some(("p", &LLMProviderKind::OpenAI))).is_none());
        assert!(require_local_binding(None).is_none());
    }
}
