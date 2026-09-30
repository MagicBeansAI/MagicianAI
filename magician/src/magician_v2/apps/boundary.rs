//! Request, dispatch and scoped-store enforcement boundaries.
//!
//! These adapters deliberately perform no I/O and expose no route. Trusted
//! transport/store adapters mint the non-deserializable evidence immediately
//! before calling them. The returned fences are likewise not transport values:
//! later phases make consequential executors and the app store require one.

use std::{
    net::IpAddr,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    authority::{
        resolve_app_authority, AppAuthorityError, AppAuthorityResolutionInput,
        AppScopeAuthentication, AuthenticatedAppScope, ResolvedAppAuthority,
    },
    models::{
        AppContractError, AppContractLimits, AppDataClassification, AppDigest, AppInstallationId,
        AppModelProcessing, AppMutationCommand, AppQueryRequest, AppReference, AppRevision,
        AppScopeBindingRef, ValidateAppContract,
    },
    records::{AppMutationOrigin, AppScope},
};
use crate::magician_v2::{
    agents::{AgentInvocationContext, InvocationSourceKind, InvocationSurface},
    json_traversal::canonical_json_bytes,
};

/// Transport-authenticated request identity.
///
/// The wrapped value has private fields and no `Deserialize` implementation.
/// Caller-supplied principal/workspace values are accepted only as an exact
/// assertion against this server-owned identity; they never select a scope.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct VerifiedAppTransportSession(AuthenticatedAppScope);

impl VerifiedAppTransportSession {
    #[allow(clippy::too_many_arguments)]
    pub fn from_verified_session(
        scope: AppScope,
        scope_binding_ref: super::models::AppScopeBindingRef,
        actor_ref: AppReference,
        session_ref: AppReference,
        authentication_revision: AppRevision,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        Ok(Self(AuthenticatedAppScope::from_verified_session(
            scope,
            scope_binding_ref,
            actor_ref,
            session_ref,
            authentication_revision,
            issued_at,
            expires_at,
        )?))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_loopback(
        peer_ip: IpAddr,
        single_user_deployment: bool,
        scope: AppScope,
        scope_binding_ref: super::models::AppScopeBindingRef,
        actor_ref: AppReference,
        session_ref: AppReference,
        authentication_revision: AppRevision,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        Ok(Self(AuthenticatedAppScope::from_trusted_loopback(
            peer_ip,
            single_user_deployment,
            scope,
            scope_binding_ref,
            actor_ref,
            session_ref,
            authentication_revision,
            issued_at,
            expires_at,
        )?))
    }

    pub fn bind_request(
        &self,
        requested_scope: Option<&AppScope>,
        now: &DateTime<Utc>,
    ) -> Result<AuthenticatedAppScope, AppBoundaryError> {
        self.0.ensure_live_at(now)?;
        if requested_scope.is_some_and(|requested| requested != self.0.scope()) {
            return Err(AppBoundaryError::CallerScopeMismatch);
        }
        Ok(self.0.clone())
    }
}

/// Fresh authority evidence minted from the current app stores.
///
/// Construction performs the complete authority resolution and is crate-only.
/// It cannot be reconstructed from a projected record, model output or request
/// body. Callers must mint a new value at each consequential boundary.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AppCurrentAuthorityEvidence(ResolvedAppAuthority);

impl AppCurrentAuthorityEvidence {
    pub fn from_trusted_stores(
        input: AppAuthorityResolutionInput<'_>,
    ) -> Result<Self, AppBoundaryError> {
        Ok(Self(resolve_app_authority(input)?))
    }

    pub fn resolved(&self) -> &ResolvedAppAuthority {
        &self.0
    }

    /// Carry a resolution that the workflow owner refreshed at this exact
    /// boundary into the move-only store-fence API. Callers cannot build the
    /// inner authority from transport data because `ResolvedAppAuthority`
    /// itself is produced only by the authority resolver.
    pub(crate) fn from_current_resolution(resolved: ResolvedAppAuthority) -> Self {
        Self(resolved)
    }
}

/// Exact authority identity retained between projection and an effect.
///
/// This is serialization-only for durable internal bindings. Replaying or
/// editing serialized bytes cannot mint the current evidence required below.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppProjectedAuthorityBinding {
    scope_binding_ref: super::models::AppScopeBindingRef,
    authentication_revision: AppRevision,
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    grant_revision: AppRevision,
    schema_revision: AppRevision,
    surface_revision: Option<AppRevision>,
    authority_digest: super::models::AppDigest,
}

impl AppProjectedAuthorityBinding {
    pub fn from_current(current: &AppCurrentAuthorityEvidence) -> Self {
        let resolved = current.resolved();
        Self {
            scope_binding_ref: resolved.scope_binding_ref.clone(),
            authentication_revision: resolved.authentication_revision,
            installation_id: resolved.installation_id.clone(),
            installation_generation: resolved.installation_generation,
            package_revision_ref: resolved.package_revision_ref.clone(),
            grant_revision: resolved.grant_revision,
            schema_revision: resolved.schema_revision,
            surface_revision: resolved.surface_revision,
            authority_digest: resolved.authority_digest.clone(),
        }
    }

    fn matches(&self, current: &ResolvedAppAuthority) -> bool {
        self.scope_binding_ref == current.scope_binding_ref
            && self.authentication_revision == current.authentication_revision
            && self.installation_id == current.installation_id
            && self.installation_generation == current.installation_generation
            && self.package_revision_ref == current.package_revision_ref
            && self.grant_revision == current.grant_revision
            && self.schema_revision == current.schema_revision
            && self.surface_revision == current.surface_revision
            && self.authority_digest == current.authority_digest
    }
}

/// Non-forgeable authorization consumed by a consequential tool dispatcher.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDispatchAuthorityFence {
    authority: AppProjectedAuthorityBinding,
    tool: AppReference,
    context_reads: Vec<AppReference>,
}

impl AppDispatchAuthorityFence {
    pub fn tool(&self) -> &AppReference {
        &self.tool
    }

    pub fn context_reads(&self) -> &[AppReference] {
        &self.context_reads
    }

    pub fn authority(&self) -> &AppProjectedAuthorityBinding {
        &self.authority
    }
}

/// Revalidate a projected authority identity immediately before dispatch.
pub fn authorize_app_dispatch(
    current: AppCurrentAuthorityEvidence,
    projected: &AppProjectedAuthorityBinding,
    tool: AppReference,
    mut context_reads: Vec<AppReference>,
) -> Result<AppDispatchAuthorityFence, AppBoundaryError> {
    ensure_current_binding(current.resolved(), projected)?;
    let limits = AppContractLimits::default();
    if context_reads.len() > limits.max_collection_items() {
        return Err(AppBoundaryError::ContextReadLimit {
            limit: limits.max_collection_items(),
        });
    }
    if !current.resolved().permits_tool(&tool) {
        return Err(AppBoundaryError::ToolDenied(tool.to_string()));
    }
    let mut unique = std::collections::BTreeSet::new();
    for context in &context_reads {
        if !unique.insert(context) {
            return Err(AppBoundaryError::DuplicateContextRead);
        }
        if !current.resolved().permits_context_read(context) {
            return Err(AppBoundaryError::ContextReadDenied(context.to_string()));
        }
    }
    context_reads.sort();
    Ok(AppDispatchAuthorityFence {
        authority: projected.clone(),
        tool,
        context_reads,
    })
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppStoreOperation {
    Query,
    Mutation,
}

/// Provider boundary that will receive a trusted personal-agent projection.
///
/// This is deliberately not deserializable. The execution runtime chooses the
/// class from its current provider attestation; a model/tool argument cannot
/// claim that a remote model is local or deterministic.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppAgentProcessingClass {
    Deterministic,
    LocalModel,
    RemoteModel,
}

/// Canonical provider-routing capability for a personal-agent read. A trusted
/// provider registry mints this after endpoint attestation and classification
/// policy resolution; callers cannot supply class/ceiling values directly to
/// the app-data authority constructor.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPersonalAgentProviderGrant {
    processing_class: AppAgentProcessingClass,
    maximum_classification: AppDataClassification,
    capability_revision: AppRevision,
    provider_configuration_digest: AppDigest,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// Request-bound owner credential for the guarded personal-agent app surface.
///
/// The HTTP adapter mints this value only from a verified request identity and
/// binds it to the exact persisted chat session, target agent and one
/// server-generated request reference. It is
/// deliberately neither serializable nor deserializable: model arguments,
/// task records and replayed JSON cannot reconstruct or persist it. The chat
/// runtime may clone only its `Arc` carrier while preserving the in-process
/// guarded-result continuation; the capability value itself is move-only.
pub struct AppOwnerExecutionCredential {
    authenticated_scope: AuthenticatedAppScope,
    request_ref: AppReference,
    chat_session_id: String,
    target_agent_id: String,
    realtime_voice: Option<AppRealtimeVoicePhysicalBinding>,
    realtime_route_authorizer: Option<Arc<dyn AppRealtimeVoiceRouteAuthorizer>>,
    realtime_turn: Option<Mutex<Option<AppRealtimeVoiceTurnBinding>>>,
    invalidation: Option<CancellationToken>,
}

/// Reopens operator-owned physical realtime configuration without putting a
/// config snapshot or provider-trust claim into the credential itself.
pub trait AppRealtimeVoiceRouteAuthorizer: Send + Sync {
    fn ensure_current_realtime_voice_route(
        &self,
        credential: &AppOwnerExecutionCredential,
        now: DateTime<Utc>,
    ) -> bool;
}

/// Server-owned admission for one authenticated realtime-voice control
/// connection. The media registration boundary mints this value and the
/// control WebSocket consumes it exactly once from an in-process registry.
/// It deliberately carries no wire representation and is invalidated when the
/// socket ends, rotates, reconnects, or outlives the authenticated request.
pub struct AppRealtimeVoiceOwnerSessionCredential {
    authenticated_scope: AuthenticatedAppScope,
    request_ref: AppReference,
    voice_session_id: String,
    target_agent_id: String,
    invalidation: CancellationToken,
}

/// Exact physical realtime route selected by the server-side provider
/// registry. Strings here are observations, never authority by themselves;
/// every consequential read/publish boundary reopens the operator trust
/// catalog and requires the resulting configuration digest to match.
struct AppRealtimeVoicePhysicalBinding {
    voice_session_id: String,
    profile_name: String,
    provider: String,
    model: String,
    base_url: Option<String>,
    topology: String,
    storage_policy: String,
    transport_cohort: AppDigest,
}

struct AppRealtimeVoiceTurnBinding {
    chat_turn_id: String,
    invalidation: CancellationToken,
}

/// Content-free final delivery fence for one provider-bound realtime turn.
/// It is safe to move through the in-process actor mailbox, but has no
/// serialization implementation and cannot be supplied by a native client.
pub struct AppRealtimeVoiceDeliveryFence {
    owner: Arc<AppOwnerExecutionCredential>,
    execution_ref: AppReference,
    turn_invalidation: CancellationToken,
}

impl std::fmt::Debug for AppRealtimeVoiceDeliveryFence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppRealtimeVoiceDeliveryFence")
            .field("credential", &"<opaque>")
            .finish()
    }
}

impl AppRealtimeVoiceDeliveryFence {
    pub fn ensure_current(&self, now: DateTime<Utc>) -> Result<(), AppBoundaryError> {
        self.owner.ensure_current_realtime_voice_route(now)?;
        if self.turn_invalidation.is_cancelled()
            || !self.execution_ref.as_str().starts_with("owner-execution:")
        {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        Ok(())
    }
}

impl AppRealtimeVoiceOwnerSessionCredential {
    pub fn from_authenticated_session(
        authenticated_scope: AuthenticatedAppScope,
        voice_session_id: impl Into<String>,
        target_agent_id: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if !matches!(
            authenticated_scope.authentication(),
            AppScopeAuthentication::AuthenticatedSession
                | AppScopeAuthentication::TrustedLoopbackSingleUser
        ) {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let voice_session_id = voice_session_id.into();
        let target_agent_id = target_agent_id.into();
        if voice_session_id.trim().is_empty()
            || target_agent_id.trim().is_empty()
            || voice_session_id.len() > 256
            || target_agent_id.len() > 256
        {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let request_ref = AppReference::parse(format!(
            "owner-voice-request:{}",
            uuid::Uuid::new_v4().simple()
        ))
        .map_err(|_| AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        Ok(Self {
            authenticated_scope,
            request_ref,
            voice_session_id,
            target_agent_id,
            invalidation: CancellationToken::new(),
        })
    }

    pub fn matches_authenticated_scope(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        voice_session_id: &str,
        now: DateTime<Utc>,
    ) -> bool {
        self.is_live_at(now)
            && authenticated_scope.ensure_live_at(&now).is_ok()
            && self.voice_session_id == voice_session_id
            && self.authenticated_scope.scope_binding_ref()
                == authenticated_scope.scope_binding_ref()
            && self.authenticated_scope.authentication_revision()
                == authenticated_scope.authentication_revision()
            && self.authenticated_scope.actor_ref() == authenticated_scope.actor_ref()
            && self.authenticated_scope.session_ref() == authenticated_scope.session_ref()
    }

    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        !self.invalidation.is_cancelled() && self.authenticated_scope.ensure_live_at(&now).is_ok()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn bind_physical_profile(
        &self,
        chat_session_id: impl Into<String>,
        profile_name: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        base_url: Option<String>,
        topology: impl Into<String>,
        storage_policy: impl Into<String>,
        route_authorizer: Arc<dyn AppRealtimeVoiceRouteAuthorizer>,
        now: DateTime<Utc>,
    ) -> Result<AppOwnerExecutionCredential, AppBoundaryError> {
        self.authenticated_scope.ensure_live_at(&now)?;
        if self.invalidation.is_cancelled() {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let chat_session_id = chat_session_id.into();
        let profile_name = profile_name.into();
        let provider = provider.into();
        let model = model.into();
        let topology = topology.into();
        let storage_policy = storage_policy.into();
        for value in [
            chat_session_id.as_str(),
            profile_name.as_str(),
            provider.as_str(),
            model.as_str(),
            topology.as_str(),
            storage_policy.as_str(),
        ] {
            if value.trim().is_empty()
                || value.len() > 512
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
            }
        }
        if storage_policy != "no_provider_storage" {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let transport_cohort = AppDigest::blake3(
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}",
                profile_name,
                provider,
                model,
                base_url.as_deref().unwrap_or_default(),
                topology,
                storage_policy,
            )
            .as_bytes(),
        );
        Ok(AppOwnerExecutionCredential {
            authenticated_scope: self.authenticated_scope.clone(),
            request_ref: self.request_ref.clone(),
            chat_session_id,
            target_agent_id: self.target_agent_id.clone(),
            realtime_voice: Some(AppRealtimeVoicePhysicalBinding {
                voice_session_id: self.voice_session_id.clone(),
                profile_name,
                provider,
                model,
                base_url,
                topology,
                storage_policy,
                transport_cohort,
            }),
            realtime_route_authorizer: Some(route_authorizer),
            realtime_turn: Some(Mutex::new(None)),
            invalidation: Some(self.invalidation.clone()),
        })
    }

    pub fn invalidate(&self) {
        self.invalidation.cancel();
    }
}

/// Exact turn-bound authority derived from the authenticated request
/// credential and typed invocation. It is stable across compiled tool calls in
/// one credential/turn, distinct across requests or turns, and remains
/// non-serializable.
pub(crate) struct BoundAppOwnerExecutionCredential {
    authenticated_scope: AuthenticatedAppScope,
    execution_ref: AppReference,
}

impl BoundAppOwnerExecutionCredential {
    pub(crate) fn into_parts(self) -> (AuthenticatedAppScope, AppReference) {
        (self.authenticated_scope, self.execution_ref)
    }
}

impl AppOwnerExecutionCredential {
    pub fn from_authenticated_chat(
        authenticated_scope: AuthenticatedAppScope,
        chat_session_id: impl Into<String>,
        target_agent_id: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if !matches!(
            authenticated_scope.authentication(),
            AppScopeAuthentication::AuthenticatedSession
                | AppScopeAuthentication::TrustedLoopbackSingleUser
        ) {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let chat_session_id = chat_session_id.into();
        let target_agent_id = target_agent_id.into();
        if chat_session_id.trim().is_empty()
            || target_agent_id.trim().is_empty()
            || chat_session_id.len() > 256
            || target_agent_id.len() > 256
        {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let request_ref =
            AppReference::parse(format!("owner-request:{}", uuid::Uuid::new_v4().simple()))
                .map_err(|_| AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        Ok(Self {
            authenticated_scope,
            request_ref,
            chat_session_id,
            target_agent_id,
            realtime_voice: None,
            realtime_route_authorizer: None,
            realtime_turn: None,
            invalidation: None,
        })
    }

    /// Narrow the request credential to the only result-guard-preserving
    /// personal-agent surface implemented today.
    pub(crate) fn bind_guard_preserving_chat_inline(
        &self,
        invocation: &AgentInvocationContext,
        now: DateTime<Utc>,
    ) -> Result<BoundAppOwnerExecutionCredential, AppBoundaryError> {
        self.authenticated_scope.ensure_live_at(&now)?;
        if self
            .invalidation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let expected_surface = if self.realtime_voice.is_some() {
            InvocationSurface::RealtimeVoice
        } else {
            InvocationSurface::Chat
        };
        let direct_chat_inline = invocation.source_agent_id.is_none()
            && invocation.surface == expected_surface
            && invocation.feature_mode == crate::magician_v2::agents::FeatureMode::None
            && matches!(
                invocation.source_kind,
                InvocationSourceKind::Direct | InvocationSourceKind::ChatInline
            )
            && invocation
                .chat_session_id
                .as_deref()
                .is_some_and(|session_id| session_id == self.chat_session_id)
            && invocation
                .chat_turn_id
                .as_deref()
                .is_some_and(|turn_id| !turn_id.trim().is_empty() && turn_id.len() <= 256)
            && invocation.target_agent_id == self.target_agent_id
            && invocation.principal == self.authenticated_scope.scope().principal.as_str()
            && invocation.workspace == self.authenticated_scope.scope().workspace.as_str();
        if !direct_chat_inline {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let turn_id = invocation
            .chat_turn_id
            .as_deref()
            .ok_or(AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        if self.realtime_voice.is_some() {
            self.current_realtime_turn_invalidation(turn_id)?;
        }
        let execution_digest = AppDigest::blake3(
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                self.authenticated_scope.scope_binding_ref(),
                self.authenticated_scope.authentication_revision().get(),
                self.authenticated_scope.actor_ref(),
                self.authenticated_scope.session_ref(),
                self.request_ref,
                self.chat_session_id,
                turn_id,
                self.target_agent_id,
                self.realtime_voice
                    .as_ref()
                    .map(|binding| binding.transport_cohort.as_str())
                    .unwrap_or_default(),
            )
            .as_bytes(),
        );
        let execution_ref = AppReference::parse(format!(
            "owner-execution:{}",
            execution_digest.as_str().trim_start_matches("blake3:")
        ))
        .map_err(|_| AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        Ok(BoundAppOwnerExecutionCredential {
            authenticated_scope: self.authenticated_scope.clone(),
            execution_ref,
        })
    }

    pub(crate) fn permits_guard_preserving_chat_inline(
        &self,
        invocation: &AgentInvocationContext,
        now: DateTime<Utc>,
    ) -> bool {
        self.bind_guard_preserving_chat_inline(invocation, now)
            .is_ok()
    }

    /// Session-start discovery may project the governed app family only for a
    /// server-bound realtime owner credential. Consequential dispatch still
    /// requires `bind_guard_preserving_chat_inline`, including an exact turn.
    pub(crate) fn permits_realtime_voice_catalog(
        &self,
        principal: &str,
        workspace: &str,
        chat_session_id: &str,
        target_agent_id: &str,
        now: DateTime<Utc>,
    ) -> bool {
        self.ensure_live_realtime_voice(now).is_ok()
            && self.chat_session_id == chat_session_id
            && self.target_agent_id == target_agent_id
            && self.authenticated_scope.scope().principal.as_str() == principal
            && self.authenticated_scope.scope().workspace.as_str() == workspace
    }

    pub(crate) fn ensure_live_realtime_voice(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        self.authenticated_scope.ensure_live_at(&now)?;
        if self.realtime_voice.is_none()
            || self
                .invalidation
                .as_ref()
                .is_none_or(CancellationToken::is_cancelled)
        {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        Ok(())
    }

    fn ensure_current_realtime_voice_route(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        self.ensure_live_realtime_voice(now)?;
        if !self
            .realtime_route_authorizer
            .as_ref()
            .is_some_and(|authorizer| authorizer.ensure_current_realtime_voice_route(self, now))
        {
            return Err(AppBoundaryError::StalePersonalAgentProviderGrant);
        }
        Ok(())
    }

    pub(crate) fn realtime_profile_name(&self) -> Option<&str> {
        self.realtime_voice
            .as_ref()
            .map(|binding| binding.profile_name.as_str())
    }

    /// Advance the server-owned realtime turn epoch. Advancing or explicitly
    /// cancelling the epoch invalidates every delayed fence from the prior
    /// utterance; a replayed tool result cannot make the old epoch current.
    pub fn begin_realtime_turn(
        &self,
        chat_turn_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        self.begin_realtime_turn_with_invalidation(chat_turn_id, CancellationToken::new(), now)
    }

    pub fn begin_realtime_turn_with_invalidation(
        &self,
        chat_turn_id: &str,
        turn_invalidation: CancellationToken,
        now: DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        self.ensure_live_realtime_voice(now)?;
        if turn_invalidation.is_cancelled()
            || chat_turn_id.trim().is_empty()
            || chat_turn_id.len() > 256
            || chat_turn_id.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AppBoundaryError::InvalidAppOwnerExecutionCredential);
        }
        let turn = self
            .realtime_turn
            .as_ref()
            .ok_or(AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        let mut turn = turn
            .lock()
            .map_err(|_| AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        if turn.as_ref().is_some_and(|current| {
            current.chat_turn_id == chat_turn_id && !current.invalidation.is_cancelled()
        }) {
            return Ok(());
        }
        if let Some(current) = turn.take() {
            current.invalidation.cancel();
        }
        *turn = Some(AppRealtimeVoiceTurnBinding {
            chat_turn_id: chat_turn_id.to_owned(),
            invalidation: turn_invalidation,
        });
        Ok(())
    }

    pub fn invalidate_realtime_turn(&self) {
        if let Some(turn) = self.realtime_turn.as_ref() {
            if let Ok(mut turn) = turn.lock() {
                if let Some(current) = turn.take() {
                    current.invalidation.cancel();
                }
            }
        }
    }

    pub(crate) fn current_realtime_turn_invalidation(
        &self,
        chat_turn_id: &str,
    ) -> Result<CancellationToken, AppBoundaryError> {
        let turn = self
            .realtime_turn
            .as_ref()
            .ok_or(AppBoundaryError::InvalidAppOwnerExecutionCredential)?
            .lock()
            .map_err(|_| AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        let current = turn
            .as_ref()
            .filter(|current| {
                current.chat_turn_id == chat_turn_id && !current.invalidation.is_cancelled()
            })
            .ok_or(AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        Ok(current.invalidation.clone())
    }

    pub(crate) fn realtime_physical_binding(
        &self,
    ) -> Option<(&str, &str, &str, &str, Option<&str>, &str, &str, &AppDigest)> {
        let binding = self.realtime_voice.as_ref()?;
        Some((
            binding.voice_session_id.as_str(),
            binding.profile_name.as_str(),
            binding.provider.as_str(),
            binding.model.as_str(),
            binding.base_url.as_deref(),
            binding.topology.as_str(),
            binding.storage_policy.as_str(),
            &binding.transport_cohort,
        ))
    }

    pub(crate) fn ensure_realtime_physical_route(
        &self,
        voice_session_id: &str,
        profile_name: &str,
        provider: &str,
        model: &str,
        base_url: Option<&str>,
        topology: &str,
        storage_policy: &str,
        now: DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        self.ensure_live_realtime_voice(now)?;
        let binding = self
            .realtime_voice
            .as_ref()
            .ok_or(AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        let current_cohort = AppDigest::blake3(
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}",
                profile_name,
                provider,
                model,
                base_url.unwrap_or_default(),
                topology,
                storage_policy,
            )
            .as_bytes(),
        );
        if binding.voice_session_id != voice_session_id
            || binding.profile_name != profile_name
            || binding.provider != provider
            || binding.model != model
            || binding.base_url.as_deref() != base_url
            || binding.topology != topology
            || binding.storage_policy != storage_policy
            || binding.transport_cohort != current_cohort
        {
            return Err(AppBoundaryError::StalePersonalAgentProviderGrant);
        }
        Ok(())
    }

    pub fn delivery_fence(
        self: &Arc<Self>,
        invocation: &AgentInvocationContext,
        now: DateTime<Utc>,
    ) -> Result<AppRealtimeVoiceDeliveryFence, AppBoundaryError> {
        let turn_id = invocation
            .chat_turn_id
            .as_deref()
            .ok_or(AppBoundaryError::InvalidAppOwnerExecutionCredential)?;
        let bound = self.bind_guard_preserving_chat_inline(invocation, now)?;
        Ok(AppRealtimeVoiceDeliveryFence {
            owner: Arc::clone(self),
            execution_ref: bound.execution_ref,
            turn_invalidation: self.current_realtime_turn_invalidation(turn_id)?,
        })
    }
}

impl AppPersonalAgentProviderGrant {
    // Phase 3's trusted provider registry is the first production minter.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_trusted_provider_registry(
        processing_class: AppAgentProcessingClass,
        maximum_classification: AppDataClassification,
        capability_revision: AppRevision,
        provider_configuration_digest: AppDigest,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        if expires_at <= issued_at {
            return Err(AppBoundaryError::StalePersonalAgentProviderGrant);
        }
        Ok(Self {
            processing_class,
            maximum_classification,
            capability_revision,
            provider_configuration_digest,
            issued_at,
            expires_at,
        })
    }

    pub(crate) fn processing_class(&self) -> AppAgentProcessingClass {
        self.processing_class
    }
}

/// Runtime-resolved proof that the current execution is owner-facing rather
/// than autonomous, delegated, handed over or public. It also binds the exact
/// authenticated session to the canonical provider grant.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectOwnerExecutionEvidence {
    scope_binding_ref: AppScopeBindingRef,
    authentication_revision: AppRevision,
    actor_ref: AppReference,
    session_ref: AppReference,
    execution_ref: AppReference,
    provider_grant: AppPersonalAgentProviderGrant,
}

impl AppDirectOwnerExecutionEvidence {
    // Phase 3's execution adapter is the first production minter.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_resolved_execution(
        authenticated_scope: &AuthenticatedAppScope,
        invocation: &AgentInvocationContext,
        execution_ref: AppReference,
        provider_grant: AppPersonalAgentProviderGrant,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let direct = invocation.source_agent_id.is_none()
            && matches!(
                invocation.surface,
                InvocationSurface::Chat | InvocationSurface::RealtimeVoice
            )
            && invocation.feature_mode == crate::magician_v2::agents::FeatureMode::None
            && matches!(
                invocation.source_kind,
                InvocationSourceKind::Direct | InvocationSourceKind::ChatInline
            );
        if !direct
            || invocation.principal != authenticated_scope.scope().principal.as_str()
            || invocation.workspace != authenticated_scope.scope().workspace.as_str()
            || now < provider_grant.issued_at
            || now >= provider_grant.expires_at
        {
            return Err(AppBoundaryError::IndirectPersonalAgentExecution);
        }
        Ok(Self {
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            authentication_revision: authenticated_scope.authentication_revision(),
            actor_ref: authenticated_scope.actor_ref().clone(),
            session_ref: authenticated_scope.session_ref().clone(),
            execution_ref,
            provider_grant,
        })
    }
}

/// Server-owned authority for one direct personal-agent data read.
///
/// Phase 2E's generic adapter requires this value in addition to authenticated
/// scope. It has no `Deserialize` implementation or public constructor, so a
/// generic tool payload cannot turn a delegated/outward execution into a
/// direct owner-facing personal agent.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPersonalAgentReadAuthority {
    scope_binding_ref: AppScopeBindingRef,
    authentication_revision: AppRevision,
    actor_ref: AppReference,
    session_ref: AppReference,
    execution_ref: AppReference,
    processing_class: AppAgentProcessingClass,
    maximum_classification: AppDataClassification,
    capability_revision: AppRevision,
    provider_configuration_digest: AppDigest,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// Content-free final-publication and launch fence derived from one exact
/// personal-agent read authority. It cannot authorize a store read and is
/// deliberately neither cloneable nor serializable; its only job is to prove
/// that the same authenticated session and provider attestation remain current
/// after asynchronous work, immediately before bytes or effects are released.
pub(crate) struct AppPersonalAgentPublicationFence {
    scope_binding_ref: AppScopeBindingRef,
    authentication_revision: AppRevision,
    actor_ref: AppReference,
    session_ref: AppReference,
    execution_ref: AppReference,
    processing_class: AppAgentProcessingClass,
    maximum_classification: AppDataClassification,
    capability_revision: AppRevision,
    provider_configuration_digest: AppDigest,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppPersonalAgentReadAuthority {
    /// Mint from a current direct-owner execution after the execution runtime
    /// has already resolved audience and provider authority.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_current_execution(
        authenticated_scope: &AuthenticatedAppScope,
        evidence: AppDirectOwnerExecutionEvidence,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if evidence.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || evidence.authentication_revision != authenticated_scope.authentication_revision()
            || evidence.actor_ref != *authenticated_scope.actor_ref()
            || evidence.session_ref != *authenticated_scope.session_ref()
            || now < evidence.provider_grant.issued_at
            || now >= evidence.provider_grant.expires_at
        {
            return Err(AppBoundaryError::StalePersonalAgentAuthority);
        }
        Ok(Self {
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            authentication_revision: authenticated_scope.authentication_revision(),
            actor_ref: authenticated_scope.actor_ref().clone(),
            session_ref: authenticated_scope.session_ref().clone(),
            execution_ref: evidence.execution_ref,
            processing_class: evidence.provider_grant.processing_class,
            maximum_classification: evidence.provider_grant.maximum_classification,
            capability_revision: evidence.provider_grant.capability_revision,
            provider_configuration_digest: evidence.provider_grant.provider_configuration_digest,
            issued_at: now,
            expires_at: authenticated_scope
                .expires_at()
                .to_owned()
                .min(evidence.provider_grant.expires_at),
        })
    }

    pub(crate) fn ensure_current(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        authenticated_scope.ensure_live_at(now)?;
        if now < &self.issued_at
            || now >= &self.expires_at
            || self.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || self.authentication_revision != authenticated_scope.authentication_revision()
            || self.actor_ref != *authenticated_scope.actor_ref()
            || self.session_ref != *authenticated_scope.session_ref()
        {
            return Err(AppBoundaryError::StalePersonalAgentAuthority);
        }
        Ok(())
    }

    pub fn execution_ref(&self) -> &AppReference {
        &self.execution_ref
    }

    pub fn processing_class(&self) -> AppAgentProcessingClass {
        self.processing_class
    }

    pub fn maximum_classification(&self) -> AppDataClassification {
        self.maximum_classification
    }

    pub(crate) fn publication_fence(&self) -> AppPersonalAgentPublicationFence {
        AppPersonalAgentPublicationFence {
            scope_binding_ref: self.scope_binding_ref.clone(),
            authentication_revision: self.authentication_revision,
            actor_ref: self.actor_ref.clone(),
            session_ref: self.session_ref.clone(),
            execution_ref: self.execution_ref.clone(),
            processing_class: self.processing_class,
            maximum_classification: self.maximum_classification,
            capability_revision: self.capability_revision,
            provider_configuration_digest: self.provider_configuration_digest.clone(),
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        }
    }
}

impl AppPersonalAgentPublicationFence {
    pub(crate) fn ensure_current(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        current_provider_grant: &AppPersonalAgentProviderGrant,
        now: &DateTime<Utc>,
    ) -> Result<(), AppBoundaryError> {
        authenticated_scope.ensure_live_at(now)?;
        if now < &self.issued_at
            || now >= &self.expires_at
            || self.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || self.authentication_revision != authenticated_scope.authentication_revision()
            || self.actor_ref != *authenticated_scope.actor_ref()
            || self.session_ref != *authenticated_scope.session_ref()
            || self.processing_class != current_provider_grant.processing_class
            || self.maximum_classification != current_provider_grant.maximum_classification
            || self.capability_revision != current_provider_grant.capability_revision
            || self.provider_configuration_digest
                != current_provider_grant.provider_configuration_digest
            || now < &current_provider_grant.issued_at
            || now >= &current_provider_grant.expires_at
        {
            return Err(AppBoundaryError::StalePersonalAgentAuthority);
        }
        Ok(())
    }

    pub(crate) fn audience(&self) -> AppStoreReadAudience {
        AppStoreReadAudience::PersonalAgent {
            execution_ref: self.execution_ref.clone(),
            processing_class: self.processing_class,
            maximum_classification: self.maximum_classification,
        }
    }

    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// Read audience carried inside the move-only store fence and consumed by the
/// query owner before any cursor is published.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppStoreReadAudience {
    AppRuntime,
    AuthenticatedOwner,
    PersonalAgent {
        execution_ref: AppReference,
        processing_class: AppAgentProcessingClass,
        maximum_classification: AppDataClassification,
    },
}

impl AppStoreReadAudience {
    pub fn permits_policy(
        &self,
        classification: AppDataClassification,
        model_processing: AppModelProcessing,
    ) -> bool {
        match self {
            Self::AppRuntime | Self::AuthenticatedOwner => true,
            Self::PersonalAgent {
                processing_class,
                maximum_classification,
                ..
            } => {
                classification <= *maximum_classification
                    && match processing_class {
                        AppAgentProcessingClass::Deterministic => true,
                        AppAgentProcessingClass::LocalModel => {
                            model_processing >= AppModelProcessing::LocalOnly
                        },
                        AppAgentProcessingClass::RemoteModel => {
                            model_processing == AppModelProcessing::RemoteAllowed
                        },
                    }
            },
        }
    }
}

/// Fresh exact store tuple loaded by an authenticated adapter.
///
/// Unlike [`AppCurrentAuthorityEvidence`], this is owner/data-plane evidence,
/// not executable app authority. It cannot authorize tool dispatch. The store
/// rechecks every member when consuming the derived fence.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AppCurrentStoreEvidence {
    binding: AppProjectedAuthorityBinding,
}

impl AppCurrentStoreEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_store(
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        Self::from_trusted_store_with_surface(
            authenticated_scope,
            installation_id,
            installation_generation,
            package_revision_ref,
            grant_revision,
            schema_revision,
            None,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_surface(
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        surface_revision: AppRevision,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        Self::from_trusted_store_with_surface(
            authenticated_scope,
            installation_id,
            installation_generation,
            package_revision_ref,
            grant_revision,
            schema_revision,
            Some(surface_revision),
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_trusted_store_with_surface(
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        surface_revision: Option<AppRevision>,
        now: DateTime<Utc>,
    ) -> Result<Self, AppBoundaryError> {
        authenticated_scope.ensure_live_at(&now)?;
        #[derive(Serialize)]
        struct StoreEvidenceDigest<'a> {
            scope_binding_ref: &'a AppScopeBindingRef,
            actor_ref: &'a AppReference,
            session_ref: &'a AppReference,
            authentication_revision: AppRevision,
            installation_id: &'a AppInstallationId,
            installation_generation: u64,
            package_revision_ref: &'a AppReference,
            grant_revision: AppRevision,
            schema_revision: AppRevision,
            surface_revision: Option<AppRevision>,
        }
        let authority_digest = digest_store_contract(&StoreEvidenceDigest {
            scope_binding_ref: authenticated_scope.scope_binding_ref(),
            actor_ref: authenticated_scope.actor_ref(),
            session_ref: authenticated_scope.session_ref(),
            authentication_revision: authenticated_scope.authentication_revision(),
            installation_id: &installation_id,
            installation_generation,
            package_revision_ref: &package_revision_ref,
            grant_revision,
            schema_revision,
            surface_revision,
        })?;
        Ok(Self {
            binding: AppProjectedAuthorityBinding {
                scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
                authentication_revision: authenticated_scope.authentication_revision(),
                installation_id,
                installation_generation,
                package_revision_ref,
                grant_revision,
                schema_revision,
                surface_revision,
                authority_digest,
            },
        })
    }
}

/// Non-forgeable authorization consumed by the future scoped app store.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppStoreAuthorityFence {
    authority: AppProjectedAuthorityBinding,
    operation: AppStoreOperation,
    contract_digest: super::models::AppDigest,
    read_audience: Option<AppStoreReadAudience>,
    mutation_origin: Option<AppMutationOrigin>,
    mutation_key: Option<AppDigest>,
}

impl AppStoreAuthorityFence {
    #[cfg(any(test, feature = "test-fixtures"))]
    #[allow(clippy::too_many_arguments)]
    pub fn for_store_test(
        request: &AppQueryRequest,
        scope_binding_ref: super::models::AppScopeBindingRef,
        authentication_revision: AppRevision,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
    ) -> Self {
        Self {
            authority: AppProjectedAuthorityBinding {
                scope_binding_ref,
                authentication_revision,
                installation_id: request.source_installation_id.clone(),
                installation_generation,
                package_revision_ref,
                grant_revision,
                schema_revision,
                surface_revision: Some(schema_revision),
                authority_digest: super::models::AppDigest::blake3(b"store-test-authority"),
            },
            operation: AppStoreOperation::Query,
            contract_digest: digest_store_contract(request).expect("valid store test contract"),
            read_audience: Some(AppStoreReadAudience::AppRuntime),
            mutation_origin: None,
            mutation_key: None,
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    #[allow(clippy::too_many_arguments)]
    pub fn for_mutation_test(
        command: &AppMutationCommand,
        origin: AppMutationOrigin,
        scope_binding_ref: super::models::AppScopeBindingRef,
        authentication_revision: AppRevision,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
    ) -> Self {
        let mutation_key = mutation_key(&installation_id, &origin, &command.idempotency_key)
            .expect("valid store test mutation identity");
        Self {
            authority: AppProjectedAuthorityBinding {
                scope_binding_ref,
                authentication_revision,
                installation_id,
                installation_generation,
                package_revision_ref,
                grant_revision,
                schema_revision,
                surface_revision: Some(schema_revision),
                authority_digest: AppDigest::blake3(b"store-test-authority"),
            },
            operation: AppStoreOperation::Mutation,
            contract_digest: digest_store_contract(command).expect("valid mutation test contract"),
            read_audience: None,
            mutation_origin: Some(origin),
            mutation_key: Some(mutation_key),
        }
    }

    pub fn operation(&self) -> AppStoreOperation {
        self.operation
    }

    pub fn authority(&self) -> &AppProjectedAuthorityBinding {
        &self.authority
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        &self.authority.installation_id
    }

    pub fn contract_digest(&self) -> &super::models::AppDigest {
        &self.contract_digest
    }

    pub fn binds_query(&self, request: &AppQueryRequest) -> Result<bool, AppBoundaryError> {
        Ok(self.operation == AppStoreOperation::Query
            && self.contract_digest == digest_store_contract(request)?)
    }

    pub fn binds_mutation(&self, command: &AppMutationCommand) -> Result<bool, AppBoundaryError> {
        Ok(self.operation == AppStoreOperation::Mutation
            && self.contract_digest == digest_store_contract(command)?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn consume_for_active_query(
        self,
        request: &AppQueryRequest,
        scope_binding_ref: &super::models::AppScopeBindingRef,
        authentication_revision: AppRevision,
        installation_id: &AppInstallationId,
        installation_generation: u64,
        package_revision_ref: &AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        active_surface_revision: Option<AppRevision>,
    ) -> Result<AppStoreReadAudience, AppBoundaryError> {
        if !self.binds_query(request)? {
            return Err(AppBoundaryError::StoreContractMismatch);
        }
        let projected = self.authority;
        if &projected.scope_binding_ref != scope_binding_ref
            || projected.authentication_revision != authentication_revision
            || &projected.installation_id != installation_id
            || projected.installation_generation != installation_generation
            || &projected.package_revision_ref != package_revision_ref
            || projected.grant_revision != grant_revision
            || projected.schema_revision != schema_revision
            || (projected.surface_revision.is_some()
                && projected.surface_revision != active_surface_revision)
        {
            return Err(AppBoundaryError::StaleProjectedAuthority);
        }
        self.read_audience
            .ok_or(AppBoundaryError::MissingStoreReadAudience)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn consume_for_active_mutation(
        self,
        command: &AppMutationCommand,
        scope_binding_ref: &super::models::AppScopeBindingRef,
        authentication_revision: AppRevision,
        installation_id: &AppInstallationId,
        installation_generation: u64,
        package_revision_ref: &AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        active_surface_revision: Option<AppRevision>,
    ) -> Result<AppConsumedMutationAuthority, AppBoundaryError> {
        if !self.binds_mutation(command)? {
            return Err(AppBoundaryError::StoreContractMismatch);
        }
        let projected = self.authority;
        if &projected.scope_binding_ref != scope_binding_ref
            || projected.authentication_revision != authentication_revision
            || &projected.installation_id != installation_id
            || projected.installation_generation != installation_generation
            || &projected.package_revision_ref != package_revision_ref
            || projected.grant_revision != grant_revision
            || projected.schema_revision != schema_revision
            || (projected.surface_revision.is_some()
                && projected.surface_revision != active_surface_revision)
        {
            return Err(AppBoundaryError::StaleProjectedAuthority);
        }
        Ok(AppConsumedMutationAuthority {
            origin: self
                .mutation_origin
                .ok_or(AppBoundaryError::MissingMutationOrigin)?,
            mutation_key: self
                .mutation_key
                .ok_or(AppBoundaryError::MissingMutationOrigin)?,
        })
    }
}

/// Authorize one authenticated owner query over the exact store tuple.
pub fn authorize_app_owner_store_query(
    current: AppCurrentStoreEvidence,
    request: &AppQueryRequest,
) -> Result<AppStoreAuthorityFence, AppBoundaryError> {
    authorize_store_adapter_query(current, request, AppStoreReadAudience::AuthenticatedOwner)
}

/// Authorize one direct personal-agent query. The authority is checked against
/// the same authenticated scope that loaded the store tuple.
pub fn authorize_personal_agent_store_query(
    authenticated_scope: &AuthenticatedAppScope,
    current: AppCurrentStoreEvidence,
    authority: AppPersonalAgentReadAuthority,
    request: &AppQueryRequest,
    now: DateTime<Utc>,
) -> Result<AppStoreAuthorityFence, AppBoundaryError> {
    authority.ensure_current(authenticated_scope, &now)?;
    authorize_store_adapter_query(
        current,
        request,
        AppStoreReadAudience::PersonalAgent {
            execution_ref: authority.execution_ref,
            processing_class: authority.processing_class,
            maximum_classification: authority.maximum_classification,
        },
    )
}

fn authorize_store_adapter_query(
    current: AppCurrentStoreEvidence,
    request: &AppQueryRequest,
    audience: AppStoreReadAudience,
) -> Result<AppStoreAuthorityFence, AppBoundaryError> {
    request
        .validate_app_contract(&AppContractLimits::default())
        .map_err(AppBoundaryError::InvalidStoreContract)?;
    if request.source_installation_id != current.binding.installation_id {
        return Err(AppBoundaryError::InstallationMismatch);
    }
    Ok(AppStoreAuthorityFence {
        authority: current.binding,
        operation: AppStoreOperation::Query,
        contract_digest: digest_store_contract(request)?,
        read_audience: Some(audience),
        mutation_origin: None,
        mutation_key: None,
    })
}

/// Authorize one authenticated owner mutation. Mutation provenance is minted
/// from the verified session and protocol idempotency key, never from JSON.
pub fn authorize_app_owner_store_mutation(
    current: AppCurrentStoreEvidence,
    command: &AppMutationCommand,
    origin: AppMutationOrigin,
) -> Result<AppStoreAuthorityFence, AppBoundaryError> {
    command
        .validate_app_contract(&AppContractLimits::default())
        .map_err(AppBoundaryError::InvalidStoreContract)?;
    if command.expected_schema_revision != current.binding.schema_revision {
        return Err(AppBoundaryError::SchemaRevisionMismatch);
    }
    let mutation_key = mutation_key(
        &current.binding.installation_id,
        &origin,
        &command.idempotency_key,
    )?;
    Ok(AppStoreAuthorityFence {
        authority: current.binding,
        operation: AppStoreOperation::Mutation,
        contract_digest: digest_store_contract(command)?,
        read_audience: None,
        mutation_origin: Some(origin),
        mutation_key: Some(mutation_key),
    })
}

/// Consumed, server-derived mutation identity. It cannot be deserialized or
/// cloned and is produced only together with the exact current store fence.
#[derive(Debug)]
pub struct AppConsumedMutationAuthority {
    origin: AppMutationOrigin,
    mutation_key: AppDigest,
}

impl AppConsumedMutationAuthority {
    pub fn into_parts(self) -> (AppMutationOrigin, AppDigest) {
        (self.origin, self.mutation_key)
    }
}

/// Authorize an app-store query against the current installation boundary.
pub fn authorize_app_store_query(
    current: AppCurrentAuthorityEvidence,
    projected: &AppProjectedAuthorityBinding,
    request: &AppQueryRequest,
) -> Result<AppStoreAuthorityFence, AppBoundaryError> {
    ensure_current_binding(current.resolved(), projected)?;
    request
        .validate_app_contract(&AppContractLimits::default())
        .map_err(AppBoundaryError::InvalidStoreContract)?;
    if request.source_installation_id != current.resolved().installation_id {
        return Err(AppBoundaryError::InstallationMismatch);
    }
    Ok(AppStoreAuthorityFence {
        authority: projected.clone(),
        operation: AppStoreOperation::Query,
        contract_digest: digest_store_contract(request)?,
        read_audience: Some(AppStoreReadAudience::AppRuntime),
        mutation_origin: None,
        mutation_key: None,
    })
}

/// Authorize an app-store mutation against the route-bound installation and
/// exact current schema revision.
pub fn authorize_app_store_mutation(
    current: AppCurrentAuthorityEvidence,
    projected: &AppProjectedAuthorityBinding,
    route_installation_id: &AppInstallationId,
    command: &AppMutationCommand,
    origin: AppMutationOrigin,
) -> Result<AppStoreAuthorityFence, AppBoundaryError> {
    ensure_current_binding(current.resolved(), projected)?;
    command
        .validate_app_contract(&AppContractLimits::default())
        .map_err(AppBoundaryError::InvalidStoreContract)?;
    if route_installation_id != &current.resolved().installation_id {
        return Err(AppBoundaryError::InstallationMismatch);
    }
    if command.expected_schema_revision != current.resolved().schema_revision {
        return Err(AppBoundaryError::SchemaRevisionMismatch);
    }
    let mutation_key = mutation_key(route_installation_id, &origin, &command.idempotency_key)?;
    Ok(AppStoreAuthorityFence {
        authority: projected.clone(),
        operation: AppStoreOperation::Mutation,
        contract_digest: digest_store_contract(command)?,
        read_audience: None,
        mutation_origin: Some(origin),
        mutation_key: Some(mutation_key),
    })
}

pub fn mutation_key(
    installation_id: &AppInstallationId,
    origin: &AppMutationOrigin,
    idempotency_key: &AppReference,
) -> Result<AppDigest, AppBoundaryError> {
    #[derive(Serialize)]
    struct MutationIdentity<'a> {
        installation_id: &'a AppInstallationId,
        origin: &'a AppMutationOrigin,
        idempotency_key: &'a AppReference,
    }
    digest_store_contract(&MutationIdentity {
        installation_id,
        origin,
        idempotency_key,
    })
}

fn digest_store_contract(
    value: &impl Serialize,
) -> Result<super::models::AppDigest, AppBoundaryError> {
    let value = serde_json::to_value(value)
        .map_err(|error| AppBoundaryError::ContractEncoding(error.to_string()))?;
    let bytes = canonical_json_bytes(&value)
        .map_err(|error| AppBoundaryError::ContractEncoding(error.to_string()))?;
    Ok(super::models::AppDigest::blake3(&bytes))
}

fn ensure_current_binding(
    current: &ResolvedAppAuthority,
    projected: &AppProjectedAuthorityBinding,
) -> Result<(), AppBoundaryError> {
    if !projected.matches(current) {
        return Err(AppBoundaryError::StaleProjectedAuthority);
    }
    Ok(())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppBoundaryError {
    #[error(transparent)]
    Authority(#[from] AppAuthorityError),
    #[error("caller-supplied app scope does not match the authenticated session")]
    CallerScopeMismatch,
    #[error("projected app authority is stale or belongs to another scope")]
    StaleProjectedAuthority,
    #[error("app operation targets another installation")]
    InstallationMismatch,
    #[error("app operation carries a stale schema revision")]
    SchemaRevisionMismatch,
    #[error("personal-agent app-data authority is stale or belongs to another session")]
    StalePersonalAgentAuthority,
    #[error("personal-agent provider capability is stale")]
    StalePersonalAgentProviderGrant,
    #[error("app-data personal-agent reads require a current direct owner execution")]
    IndirectPersonalAgentExecution,
    #[error("app-owner execution credential is absent, stale or outside guarded direct chat")]
    InvalidAppOwnerExecutionCredential,
    #[error("app-store query authority does not name a read audience")]
    MissingStoreReadAudience,
    #[error("app dispatch denied undeclared or ungranted tool `{0}`")]
    ToolDenied(String),
    #[error("app dispatch denied undeclared or ungranted context read `{0}`")]
    ContextReadDenied(String),
    #[error("app dispatch repeats a context read")]
    DuplicateContextRead,
    #[error("app dispatch exceeds the {limit} context-read ceiling")]
    ContextReadLimit { limit: usize },
    #[error("invalid app-store contract: {0}")]
    InvalidStoreContract(AppContractError),
    #[error("failed to encode exact app-store contract identity: {0}")]
    ContractEncoding(String),
    #[error("app-store authority does not bind the exact operation contract")]
    StoreContractMismatch,
    #[error("app-store mutation authority has no server-derived origin")]
    MissingMutationOrigin,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::{
        agents::{FeatureMode, InvocationSurface},
        apps::{
            authority::AppAuthorityCeiling,
            lifecycle::{AppInstallationLifecycle, AppInstallationStatus},
            models::{
                AppDataClassification, AppFieldPath, AppName, AppProtocolVersion,
                AppScopeBindingRef,
            },
            records::{
                AppBackgroundExecution, AppDataHandlingPolicy, AppExternalEgress, AppGrantRevision,
                AppInstallation, AppMemoryPromotion, AppNetworkPolicy, AppPersonalAgentAccess,
                AppResourceCeiling, AppSchemaCompatibility, AppSchemaRevision,
            },
        },
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    struct AlwaysCurrentRealtimeRoute;

    impl AppRealtimeVoiceRouteAuthorizer for AlwaysCurrentRealtimeRoute {
        fn ensure_current_realtime_voice_route(
            &self,
            _credential: &AppOwnerExecutionCredential,
            _now: DateTime<Utc>,
        ) -> bool {
            true
        }
    }

    fn installation_id(value: &str) -> AppInstallationId {
        AppInstallationId::parse(value).unwrap()
    }

    fn scope(principal: &str) -> AppScope {
        AppScope {
            principal: reference(principal),
            workspace: reference("default"),
        }
    }

    fn transport() -> VerifiedAppTransportSession {
        VerifiedAppTransportSession::from_verified_session(
            scope("anonymous"),
            AppScopeBindingRef::parse("scope_anonymous_default").unwrap(),
            reference("actor:owner"),
            reference("session:1"),
            revision(7),
            time(0),
            time(30),
        )
        .unwrap()
    }

    fn policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Ordinary,
            model_processing: super::super::models::AppModelProcessing::None,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 100,
            max_output_tokens: 100,
            max_cost_microusd: 100,
            max_paid_tool_invocations: 10,
            max_active_seconds: 60,
            max_lifetime_seconds: 120,
            max_browser_network_actions: 10,
            max_concurrent_foreground_runs: 2,
            max_concurrent_background_runs: 1,
            max_records: 100,
            max_payload_bytes: 1_000,
            max_attachment_bytes: 1_000,
            max_monthly_tokens: 1_000,
            max_monthly_cost_microusd: 1_000,
        }
    }

    fn ceiling(tool: &AppReference, context: &AppReference) -> AppAuthorityCeiling {
        AppAuthorityCeiling {
            tools: BTreeSet::from([tool.clone()]),
            context_reads: BTreeSet::from([context.clone()]),
            data_handling_policy: policy(),
            background_execution: AppBackgroundExecution::Denied,
            network_policy: AppNetworkPolicy::Denied,
            resources: resources(),
        }
    }

    fn current(
        authenticated: &AuthenticatedAppScope,
        grant_revision: u64,
    ) -> AppCurrentAuthorityEvidence {
        current_result(authenticated, grant_revision, |_, _| {}).unwrap()
    }

    fn current_result(
        authenticated: &AuthenticatedAppScope,
        grant_revision: u64,
        mutate: impl FnOnce(&mut AppInstallation, &mut AppGrantRevision),
    ) -> Result<AppCurrentAuthorityEvidence, AppBoundaryError> {
        current_result_with_parent(authenticated, grant_revision, mutate, None)
    }

    fn current_result_with_parent(
        authenticated: &AuthenticatedAppScope,
        grant_revision: u64,
        mutate: impl FnOnce(&mut AppInstallation, &mut AppGrantRevision),
        parent: Option<&AppAuthorityCeiling>,
    ) -> Result<AppCurrentAuthorityEvidence, AppBoundaryError> {
        let id = installation_id("install_1");
        let package = reference("package:1");
        let tool = reference("tool:content_search");
        let context = reference("context:notes");
        let revision = revision(grant_revision);
        let mut installation = AppInstallation {
            scope: authenticated.scope().clone(),
            installation_id: id.clone(),
            package_revision_ref: package.clone(),
            lifecycle: AppInstallationLifecycle {
                status: AppInstallationStatus::Enabled,
                generation: grant_revision,
                update_return_status: None,
            },
            grant_revision: Some(revision),
            active_schema_revision: Some(revision),
            active_surface_revision: Some(revision),
            created_at: time(0),
            updated_at: time(1),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        };
        let mut grant = AppGrantRevision {
            installation_id: id.clone(),
            revision,
            package_revision_ref: package.clone(),
            requested_tools: vec![tool.clone()],
            granted_tools: vec![tool.clone()],
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: None,
            granted_memory_read: None,
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_context_reads: vec![context.clone()],
            granted_context_reads: vec![context.clone()],
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: policy(),
            granted_data_handling_policy: policy(),
            granted_data_handling_policy_digest: super::super::models::AppDigest::blake3(b"policy"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: resources(),
            granted_resource_ceiling: resources(),
            approved_by: reference("actor:owner"),
            approved_at: time(1),
            authority_digest: super::super::models::AppDigest::blake3(
                format!("authority-{grant_revision}").as_bytes(),
            ),
            revoked_at: None,
        };
        mutate(&mut installation, &mut grant);
        let schema = AppSchemaRevision {
            installation_id: id,
            revision,
            package_revision_ref: package,
            canonical_entity_schema: json!({}),
            canonical_data_handling_policy: policy(),
            compiled_validation_schema: json!({}),
            compiled_index_plan: json!({}),
            compatibility_with_previous: AppSchemaCompatibility::Initial,
            migration_plan_ref: None,
            created_at: time(1),
        };
        let agent = ceiling(&tool, &context);
        let trust = ceiling(&tool, &context);
        AppCurrentAuthorityEvidence::from_trusted_stores(AppAuthorityResolutionInput {
            authenticated_scope: authenticated,
            installation: &installation,
            grant: &grant,
            schema: &schema,
            surface: None,
            agent: &agent,
            trust: &trust,
            parent,
            now: time(2),
        })
    }

    fn query(id: &str) -> AppQueryRequest {
        AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: installation_id(id),
            entity: AppName::parse("note").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 10,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("list").unwrap(),
        }
    }

    fn mutation(schema_revision: u64) -> AppMutationCommand {
        AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: reference("mutation-key:test"),
            atomicity: super::super::models::AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: revision(schema_revision),
            operations: vec![super::super::models::AppMutationOperation::Create {
                entity: AppName::parse("note").unwrap(),
                temporary_id: AppName::parse("new_note").unwrap(),
                record_id: None,
                payload: json!({"title": "hello"}),
            }],
            expected_record_revisions: Vec::new(),
        }
    }

    fn mutation_origin() -> AppMutationOrigin {
        AppMutationOrigin::Surface {
            surface_session_id: reference("surface-session:test"),
            client_mutation_id: reference("client-mutation:test"),
        }
    }

    #[test]
    fn request_scope_is_derived_from_transport_and_caller_scope_is_only_an_assertion() {
        let transport = transport();
        let authenticated = transport.bind_request(None, &time(2)).unwrap();
        assert_eq!(authenticated.scope(), &scope("anonymous"));
        assert!(matches!(
            transport.bind_request(Some(&scope("other")), &time(2)),
            Err(AppBoundaryError::CallerScopeMismatch)
        ));
        assert!(transport.bind_request(None, &time(30)).is_err());
    }

    #[test]
    fn loopback_fallback_uses_the_socket_peer_not_a_forwarded_claim() {
        assert!(VerifiedAppTransportSession::from_trusted_loopback(
            "192.0.2.1".parse().unwrap(),
            true,
            scope("anonymous"),
            AppScopeBindingRef::parse("scope_anonymous_default").unwrap(),
            reference("actor:owner"),
            reference("session:loopback"),
            revision(1),
            time(0),
            time(30),
        )
        .is_err());
    }

    #[test]
    fn dispatch_rechecks_current_identity_and_denies_invented_tools() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let projected_current = current(&authenticated, 1);
        let projected = AppProjectedAuthorityBinding::from_current(&projected_current);
        let fence = authorize_app_dispatch(
            projected_current,
            &projected,
            reference("tool:content_search"),
            vec![reference("context:notes")],
        )
        .unwrap();
        assert_eq!(fence.tool(), &reference("tool:content_search"));

        assert!(matches!(
            authorize_app_dispatch(
                current(&authenticated, 1),
                &projected,
                reference("tool:invented"),
                Vec::new(),
            ),
            Err(AppBoundaryError::ToolDenied(_))
        ));
        let changed = current(&authenticated, 2);
        assert!(matches!(
            authorize_app_dispatch(
                changed,
                &projected,
                reference("tool:content_search"),
                Vec::new(),
            ),
            Err(AppBoundaryError::StaleProjectedAuthority)
        ));
    }

    #[test]
    fn scoped_store_rejects_cross_installation_and_stale_schema_calls() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let evidence = current(&authenticated, 1);
        let projected = AppProjectedAuthorityBinding::from_current(&evidence);
        let original_query = query("install_1");
        let query_fence = authorize_app_store_query(evidence, &projected, &original_query).unwrap();
        assert_eq!(query_fence.operation(), AppStoreOperation::Query);
        assert!(query_fence.binds_query(&original_query).unwrap());
        let mut substituted_query = original_query.clone();
        substituted_query.limit = 9;
        assert!(!query_fence.binds_query(&substituted_query).unwrap());
        assert!(matches!(
            authorize_app_store_query(
                current(&authenticated, 1),
                &projected,
                &query("install_other"),
            ),
            Err(AppBoundaryError::InstallationMismatch)
        ));
        assert!(matches!(
            authorize_app_store_mutation(
                current(&authenticated, 1),
                &projected,
                &installation_id("install_1"),
                &mutation(2),
                mutation_origin(),
            ),
            Err(AppBoundaryError::SchemaRevisionMismatch)
        ));

        let original_mutation = mutation(1);
        let mutation_fence = authorize_app_store_mutation(
            current(&authenticated, 1),
            &projected,
            &installation_id("install_1"),
            &original_mutation,
            mutation_origin(),
        )
        .unwrap();
        assert!(mutation_fence.binds_mutation(&original_mutation).unwrap());
        let mut substituted_mutation = original_mutation.clone();
        substituted_mutation.idempotency_key = reference("mutation-key:substituted");
        assert!(!mutation_fence
            .binds_mutation(&substituted_mutation)
            .unwrap());
    }

    #[test]
    fn surface_store_fence_rechecks_the_active_surface_inside_the_transaction() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let installation_id = installation_id("install_1");
        let package_revision_ref = reference("package:1");
        let schema_revision = revision(1);
        let projected_surface_revision = revision(7);
        let command = mutation(1);
        let evidence = AppCurrentStoreEvidence::from_trusted_surface(
            &authenticated,
            installation_id.clone(),
            1,
            package_revision_ref.clone(),
            schema_revision,
            schema_revision,
            projected_surface_revision,
            time(2),
        )
        .unwrap();
        let fence =
            authorize_app_owner_store_mutation(evidence, &command, mutation_origin()).unwrap();
        assert!(matches!(
            fence.consume_for_active_mutation(
                &command,
                authenticated.scope_binding_ref(),
                authenticated.authentication_revision(),
                &installation_id,
                1,
                &package_revision_ref,
                schema_revision,
                schema_revision,
                Some(revision(8)),
            ),
            Err(AppBoundaryError::StaleProjectedAuthority)
        ));
    }

    #[test]
    fn boundary_evidence_cannot_be_minted_for_revoked_unavailable_or_cross_scope_state() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        assert!(matches!(
            current_result(&authenticated, 1, |_, grant| {
                grant.revoked_at = Some(time(2));
            }),
            Err(AppBoundaryError::Authority(AppAuthorityError::GrantRevoked))
        ));
        assert!(matches!(
            current_result(&authenticated, 1, |installation, _| {
                installation.lifecycle.status = AppInstallationStatus::Disabled;
                installation.disabled_at = Some(time(2));
            }),
            Err(AppBoundaryError::Authority(
                AppAuthorityError::InstallationUnavailable { .. }
            ))
        ));
        assert!(matches!(
            current_result(&authenticated, 1, |installation, _| {
                installation.scope = scope("other");
            }),
            Err(AppBoundaryError::Authority(
                AppAuthorityError::ScopeMismatch
            ))
        ));
    }

    #[test]
    fn dispatch_boundary_applies_the_delegated_parent_ceiling() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let tool = reference("tool:content_search");
        let context = reference("context:notes");
        let mut parent = ceiling(&tool, &context);
        parent.tools.clear();
        let current =
            current_result_with_parent(&authenticated, 1, |_, _| {}, Some(&parent)).unwrap();
        let projected = AppProjectedAuthorityBinding::from_current(&current);
        assert!(matches!(
            authorize_app_dispatch(current, &projected, tool, vec![context]),
            Err(AppBoundaryError::ToolDenied(_))
        ));
    }

    #[test]
    fn delegated_execution_cannot_mint_direct_owner_app_data_authority() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let grant = AppPersonalAgentProviderGrant::from_trusted_provider_registry(
            AppAgentProcessingClass::RemoteModel,
            AppDataClassification::Personal,
            revision(1),
            AppDigest::blake3(b"provider-config"),
            time(1),
            time(10),
        )
        .unwrap();
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: Some("delegate".to_owned()),
            target_agent_id: "primary".to_owned(),
            surface: InvocationSurface::Delegation,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Delegated,
            chat_session_id: None,
            chat_turn_id: None,
        };
        assert!(matches!(
            AppDirectOwnerExecutionEvidence::from_resolved_execution(
                &authenticated,
                &invocation,
                reference("execution:delegated"),
                grant,
                time(2),
            ),
            Err(AppBoundaryError::IndirectPersonalAgentExecution)
        ));

        let malformed_direct_delegation = AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: None,
            target_agent_id: "primary".to_owned(),
            surface: InvocationSurface::Delegation,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: Some("session:forged-delegation".to_owned()),
            chat_turn_id: Some("turn:forged-delegation".to_owned()),
        };
        let grant = AppPersonalAgentProviderGrant::from_trusted_provider_registry(
            AppAgentProcessingClass::LocalModel,
            AppDataClassification::Sensitive,
            revision(1),
            AppDigest::blake3(b"provider"),
            time(1),
            time(10),
        )
        .unwrap();
        assert!(matches!(
            AppDirectOwnerExecutionEvidence::from_resolved_execution(
                &authenticated,
                &malformed_direct_delegation,
                reference("execution:malformed-delegation"),
                grant,
                time(2),
            ),
            Err(AppBoundaryError::IndirectPersonalAgentExecution)
        ));
    }

    #[test]
    fn owner_execution_credential_admits_only_exact_guarded_chat_inline() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let credential = AppOwnerExecutionCredential::from_authenticated_chat(
            authenticated,
            "chat-session-1",
            "primary",
            time(2),
        )
        .unwrap();
        let invocation = |surface, source_kind| AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: None,
            target_agent_id: "primary".to_owned(),
            surface,
            feature_mode: FeatureMode::None,
            source_kind,
            chat_session_id: Some("chat-session-1".to_owned()),
            chat_turn_id: Some("turn-1".to_owned()),
        };

        for source_kind in [
            InvocationSourceKind::Direct,
            InvocationSourceKind::ChatInline,
        ] {
            assert!(credential
                .bind_guard_preserving_chat_inline(
                    &invocation(InvocationSurface::Chat, source_kind),
                    time(3),
                )
                .is_ok());
        }
        let same_turn_first = credential
            .bind_guard_preserving_chat_inline(
                &invocation(InvocationSurface::Chat, InvocationSourceKind::Direct),
                time(3),
            )
            .unwrap()
            .into_parts()
            .1;
        let same_turn_second = credential
            .bind_guard_preserving_chat_inline(
                &invocation(InvocationSurface::Chat, InvocationSourceKind::Direct),
                time(4),
            )
            .unwrap()
            .into_parts()
            .1;
        assert_eq!(same_turn_first, same_turn_second);
        let repeated_client_turn_credential = AppOwnerExecutionCredential::from_authenticated_chat(
            credential.authenticated_scope.clone(),
            "chat-session-1",
            "primary",
            time(2),
        )
        .unwrap();
        let repeated_client_turn_ref = repeated_client_turn_credential
            .bind_guard_preserving_chat_inline(
                &invocation(InvocationSurface::Chat, InvocationSourceKind::Direct),
                time(4),
            )
            .unwrap()
            .into_parts()
            .1;
        assert_ne!(same_turn_first, repeated_client_turn_ref);
        let mut next_turn = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        next_turn.chat_turn_id = Some("turn-2".to_owned());
        let next_turn_ref = credential
            .bind_guard_preserving_chat_inline(&next_turn, time(4))
            .unwrap()
            .into_parts()
            .1;
        assert_ne!(same_turn_first, next_turn_ref);
        let other_session_credential = AppOwnerExecutionCredential::from_authenticated_chat(
            credential.authenticated_scope.clone(),
            "chat-session-2",
            "primary",
            time(2),
        )
        .unwrap();
        let mut other_session = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        other_session.chat_session_id = Some("chat-session-2".to_owned());
        let other_session_ref = other_session_credential
            .bind_guard_preserving_chat_inline(&other_session, time(4))
            .unwrap()
            .into_parts()
            .1;
        assert_ne!(same_turn_first, other_session_ref);
        for surface in [
            InvocationSurface::RealtimeVoice,
            InvocationSurface::Task,
            InvocationSurface::Delegation,
            InvocationSurface::Handover,
            InvocationSurface::ThinkingMap,
            InvocationSurface::Tutor,
            InvocationSurface::AppCopilot,
            InvocationSurface::ContextualAssist,
            InvocationSurface::PublicEnvoy,
            InvocationSurface::Meeting,
            InvocationSurface::Plane,
        ] {
            assert!(credential
                .bind_guard_preserving_chat_inline(
                    &invocation(surface, InvocationSourceKind::Direct),
                    time(3),
                )
                .is_err());
        }
        for source_kind in [
            InvocationSourceKind::Autonomous,
            InvocationSourceKind::Delegated,
            InvocationSourceKind::Handover,
            InvocationSourceKind::ProductFeature,
            InvocationSourceKind::Public,
        ] {
            assert!(credential
                .bind_guard_preserving_chat_inline(
                    &invocation(InvocationSurface::Chat, source_kind),
                    time(3),
                )
                .is_err());
        }

        let mut delegated = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        delegated.source_agent_id = Some("delegate".to_owned());
        assert!(credential
            .bind_guard_preserving_chat_inline(&delegated, time(3))
            .is_err());

        let mut wrong_session = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        wrong_session.chat_session_id = Some("other-session".to_owned());
        assert!(credential
            .bind_guard_preserving_chat_inline(&wrong_session, time(3))
            .is_err());

        let mut wrong_agent = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        wrong_agent.target_agent_id = "other-agent".to_owned();
        assert!(credential
            .bind_guard_preserving_chat_inline(&wrong_agent, time(3))
            .is_err());

        let mut product_feature = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        product_feature.feature_mode = FeatureMode::Vibedev;
        assert!(credential
            .bind_guard_preserving_chat_inline(&product_feature, time(3))
            .is_err());

        let mut oversized_turn = invocation(InvocationSurface::Chat, InvocationSourceKind::Direct);
        oversized_turn.chat_turn_id = Some("x".repeat(257));
        assert!(credential
            .bind_guard_preserving_chat_inline(&oversized_turn, time(3))
            .is_err());

        assert!(credential
            .bind_guard_preserving_chat_inline(
                &invocation(InvocationSurface::Chat, InvocationSourceKind::Direct),
                time(30),
            )
            .is_err());
    }

    #[test]
    fn publication_fence_rejects_provider_revision_or_configuration_drift() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: None,
            target_agent_id: "primary".to_owned(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::ChatInline,
            chat_session_id: Some("chat-session-1".to_owned()),
            chat_turn_id: Some("turn-1".to_owned()),
        };
        let provider_grant = |capability_revision, configuration: &'static [u8]| {
            AppPersonalAgentProviderGrant::from_trusted_provider_registry(
                AppAgentProcessingClass::LocalModel,
                AppDataClassification::Sensitive,
                revision(capability_revision),
                AppDigest::blake3(configuration),
                time(1),
                time(10),
            )
            .unwrap()
        };
        let evidence = AppDirectOwnerExecutionEvidence::from_resolved_execution(
            &authenticated,
            &invocation,
            reference("execution:turn-1"),
            provider_grant(1, b"provider-config"),
            time(2),
        )
        .unwrap();
        let authority = AppPersonalAgentReadAuthority::from_current_execution(
            &authenticated,
            evidence,
            time(2),
        )
        .unwrap();
        let fence = authority.publication_fence();

        assert!(fence
            .ensure_current(
                &authenticated,
                &provider_grant(1, b"provider-config"),
                &time(3),
            )
            .is_ok());
        assert!(fence
            .ensure_current(
                &authenticated,
                &provider_grant(2, b"provider-config"),
                &time(3),
            )
            .is_err());
        assert!(fence
            .ensure_current(
                &authenticated,
                &provider_grant(1, b"changed-provider-config"),
                &time(3),
            )
            .is_err());
    }

    #[test]
    fn realtime_owner_credential_rejects_reconnect_scope_turn_and_profile_substitution() {
        let authenticated = transport().bind_request(None, &time(2)).unwrap();
        let session = Arc::new(
            AppRealtimeVoiceOwnerSessionCredential::from_authenticated_session(
                authenticated.clone(),
                "voice-1",
                "primary",
                time(2),
            )
            .unwrap(),
        );
        assert!(session.matches_authenticated_scope(&authenticated, "voice-1", time(3)));
        assert!(!session.matches_authenticated_scope(&authenticated, "voice-2", time(3)));
        let owner = Arc::new(
            session
                .bind_physical_profile(
                    "chat-voice-1",
                    "voice-local",
                    "openai_realtime_backend",
                    "local-realtime-model",
                    Some("http://127.0.0.1:9090/realtime".to_owned()),
                    "backend_proxied",
                    "no_provider_storage",
                    Arc::new(AlwaysCurrentRealtimeRoute),
                    time(3),
                )
                .unwrap(),
        );
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: None,
            target_agent_id: "primary".to_owned(),
            surface: InvocationSurface::RealtimeVoice,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: Some("chat-voice-1".to_owned()),
            chat_turn_id: Some("turn-1".to_owned()),
        };
        owner.begin_realtime_turn("turn-1", time(3)).unwrap();
        assert!(owner
            .bind_guard_preserving_chat_inline(&invocation, time(3))
            .is_ok());
        let mut crossed_session = invocation.clone();
        crossed_session.chat_session_id = Some("chat-other".to_owned());
        assert!(owner
            .bind_guard_preserving_chat_inline(&crossed_session, time(3))
            .is_err());
        assert!(owner
            .ensure_realtime_physical_route(
                "voice-1",
                "voice-local",
                "openai_realtime_backend",
                "substituted-model",
                Some("http://127.0.0.1:9090/realtime"),
                "backend_proxied",
                "no_provider_storage",
                time(3),
            )
            .is_err());
        assert!(owner
            .ensure_realtime_physical_route(
                "voice-1",
                "voice-local",
                "openai_realtime_backend",
                "local-realtime-model",
                Some("http://127.0.0.1:9090/realtime"),
                "backend_proxied",
                "provider_storage_unknown",
                time(3),
            )
            .is_err());
        let fence = owner.delivery_fence(&invocation, time(3)).unwrap();
        assert!(fence.ensure_current(time(4)).is_ok());
        owner.begin_realtime_turn("turn-2", time(4)).unwrap();
        assert!(fence.ensure_current(time(4)).is_err());
        owner.invalidate_realtime_turn();
        let mut cancelled_turn = invocation.clone();
        cancelled_turn.chat_turn_id = Some("turn-2".to_owned());
        assert!(owner
            .bind_guard_preserving_chat_inline(&cancelled_turn, time(4))
            .is_err());
        session.invalidate();
        assert!(fence.ensure_current(time(4)).is_err());
        assert!(!session.matches_authenticated_scope(&authenticated, "voice-1", time(4)));
    }

    #[test]
    fn trusted_boundary_evidence_and_fences_are_not_transport_deserializable() {
        static_assertions::assert_not_impl_any!(
            VerifiedAppTransportSession: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppCurrentAuthorityEvidence: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppProjectedAuthorityBinding: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppDispatchAuthorityFence: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppStoreAuthorityFence: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppCurrentStoreEvidence: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppPersonalAgentReadAuthority: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppPersonalAgentPublicationFence: Clone, serde::Serialize, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppOwnerExecutionCredential: Clone, std::fmt::Debug, serde::Serialize, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppRealtimeVoiceOwnerSessionCredential: Clone, std::fmt::Debug, serde::Serialize, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppRealtimeVoiceDeliveryFence: Clone, serde::Serialize, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            BoundAppOwnerExecutionCredential: Clone, std::fmt::Debug, serde::Serialize, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppPersonalAgentProviderGrant: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppDirectOwnerExecutionEvidence: Clone, serde::de::DeserializeOwned
        );
    }
}
