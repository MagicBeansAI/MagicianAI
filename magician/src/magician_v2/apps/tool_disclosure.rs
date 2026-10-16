//! Final protected-content boundary for app tool calls and their results.
//!
//! Tool names, model-produced arguments and returned labels are never
//! authority. A trusted dispatcher must supply an exact server-attested
//! transport target, a current resolved app authority and server-resolved
//! handling labels. Successful admission returns a move-only permit bound to
//! one execution and one invocation. It must be minted after every asynchronous
//! engagement/trust wait and checked immediately before I/O; it is not queue
//! authority. The permit is consumed when the exact bounded result bytes are
//! labeled, preventing it from being replayed for a second result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::{AppAuthorityError, ResolvedAppAuthority},
    models::{
        AppDataClassification, AppDigest, AppHandlingLabels, AppModelProcessing,
        AppProtocolVersion, AppReference,
    },
    policy::{
        authorize_processing_target, decide_hidden_consumer, AppConsumerCapability,
        AppConsumerDecision, AppConsumerDecisionReason, AppHiddenConsumer, AppPolicyError,
        AppProcessingTarget, AttestedAppEndpoint, ResolvedAppDataHandlingPolicy,
        ResolvedAppHandlingLabels,
    },
    records::AppNetworkPolicy,
};

/// Exact transport selected by a trusted tool dispatcher.
///
/// This value is intentionally neither serializable nor deserializable. The
/// constructors are crate-private so a package, tool result or API body cannot
/// claim that an external call is deterministic local processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestedAppToolTarget {
    tool_ref: AppReference,
    transport: AttestedAppToolTransport,
    result_policy: AppToolResultPolicy,
}

/// Trusted source semantics for the exact tool result. This can be serialized
/// only into a server digest identity, is not deserializable, and travels only
/// with the dispatcher-minted target.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppToolResultPolicy {
    /// The runtime is a deterministic, effect-free transform of bytes already
    /// disclosed under the invocation labels and cannot introduce a new source.
    PureTransformInheritsInput,
    /// A trusted local clock read. The bytes are not user data; continuation
    /// is allowed after joining Ordinary/RemoteAllowed source labels with the
    /// current session floor. Not a pure transform and not a host/file/HTTP
    /// source.
    TrustedLocalClock,
    /// Output may contain content from a local/external source independent of
    /// the invocation and therefore requires the conservative result floor.
    IntroducesLocalContent,
    /// A closed, scope-bound host read whose processing ceiling comes from
    /// the exact reviewed app grant. Its contents remain Secret-classified;
    /// input labels and narrower live policy still intersect at retention.
    ReviewedScopedHostRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttestedAppToolTransport {
    DeterministicLocal {
        runtime_ref: AppReference,
    },
    External {
        endpoint: AttestedAppEndpoint,
        destination: AppReference,
        /// Further hosts one call may reach (an app OS-jail tool that
        /// declares or is granted several). Each is checked like
        /// `destination`; empty for every single-destination target, which
        /// keeps its exact transport identity.
        additional_destinations: Vec<AppReference>,
    },
}

impl AttestedAppToolTarget {
    /// Mint only after the dispatcher has resolved a no-egress deterministic
    /// runtime implementation. Tool/provider naming is not sufficient.
    #[allow(dead_code)] // Load-bearing once the generic action dispatcher adopts this seam.
    pub fn from_trusted_local_dispatcher(
        tool_ref: AppReference,
        runtime_ref: AppReference,
    ) -> Self {
        Self {
            tool_ref,
            transport: AttestedAppToolTransport::DeterministicLocal { runtime_ref },
            result_policy: AppToolResultPolicy::IntroducesLocalContent,
        }
    }

    /// Only the closed Apps host-read binders may select this source contract.
    pub(super) fn from_reviewed_scoped_host_read(
        tool_ref: AppReference,
        runtime_ref: AppReference,
    ) -> Self {
        Self {
            tool_ref,
            transport: AttestedAppToolTransport::DeterministicLocal { runtime_ref },
            result_policy: AppToolResultPolicy::ReviewedScopedHostRead,
        }
    }

    /// Mint only after the dispatcher has resolved a wall-clock read whose
    /// arguments cannot change the host, network or filesystem. The result is
    /// new local content, not a rewrite of the input.
    pub fn from_trusted_local_clock_dispatcher(
        tool_ref: AppReference,
        runtime_ref: AppReference,
    ) -> Self {
        Self {
            tool_ref,
            transport: AttestedAppToolTransport::DeterministicLocal { runtime_ref },
            result_policy: AppToolResultPolicy::TrustedLocalClock,
        }
    }

    /// Mint only for a dispatcher implementation which has proved the exact
    /// operation is a deterministic, effect-free transform of admitted input.
    pub fn from_trusted_pure_transform_dispatcher(
        tool_ref: AppReference,
        runtime_ref: AppReference,
    ) -> Self {
        Self {
            tool_ref,
            transport: AttestedAppToolTransport::DeterministicLocal { runtime_ref },
            result_policy: AppToolResultPolicy::PureTransformInheritsInput,
        }
    }

    /// Mint only after the dispatcher has resolved the concrete endpoint and
    /// canonical destination independently of model-produced arguments.
    #[allow(dead_code)] // Load-bearing once the generic action dispatcher adopts this seam.
    pub fn from_trusted_external_dispatcher(
        tool_ref: AppReference,
        endpoint: AttestedAppEndpoint,
        destination: AppReference,
    ) -> Self {
        Self::from_trusted_external_dispatcher_set(tool_ref, endpoint, destination, Vec::new())
    }

    /// As [`Self::from_trusted_external_dispatcher`], for a call whose
    /// dispatcher admits several resolved destinations. Disclosure requires
    /// every one of them; the any-public-host marker
    /// (`destination:any-public-host`) requires the owner's explicit
    /// "any public host" grant instead of a destination list entry.
    pub fn from_trusted_external_dispatcher_set(
        tool_ref: AppReference,
        endpoint: AttestedAppEndpoint,
        destination: AppReference,
        additional_destinations: Vec<AppReference>,
    ) -> Self {
        Self {
            tool_ref,
            transport: AttestedAppToolTransport::External {
                endpoint,
                destination,
                additional_destinations,
            },
            result_policy: AppToolResultPolicy::IntroducesLocalContent,
        }
    }

    pub fn result_policy(&self) -> AppToolResultPolicy {
        self.result_policy
    }

    pub fn tool_ref(&self) -> &AppReference {
        &self.tool_ref
    }

    pub fn runtime_ref(&self) -> Option<&AppReference> {
        match &self.transport {
            AttestedAppToolTransport::DeterministicLocal { runtime_ref } => Some(runtime_ref),
            AttestedAppToolTransport::External { .. } => None,
        }
    }

    pub fn endpoint(&self) -> Option<&AttestedAppEndpoint> {
        match &self.transport {
            AttestedAppToolTransport::DeterministicLocal { .. } => None,
            AttestedAppToolTransport::External { endpoint, .. } => Some(endpoint),
        }
    }

    pub fn destination(&self) -> Option<&AppReference> {
        match &self.transport {
            AttestedAppToolTransport::DeterministicLocal { .. } => None,
            AttestedAppToolTransport::External { destination, .. } => Some(destination),
        }
    }

    fn digest(&self) -> Result<AppDigest, AppToolDisclosureError> {
        #[derive(Serialize)]
        #[serde(tag = "transport", rename_all = "snake_case")]
        enum TransportIdentity<'a> {
            DeterministicLocal {
                tool_ref: &'a AppReference,
                runtime_ref: &'a AppReference,
                result_policy: AppToolResultPolicy,
            },
            External {
                tool_ref: &'a AppReference,
                endpoint_ref: &'a AppReference,
                endpoint_trust_revision: u64,
                endpoint_configuration_digest: &'a AppDigest,
                destination: &'a AppReference,
                #[serde(skip_serializing_if = "<[AppReference]>::is_empty")]
                additional_destinations: &'a [AppReference],
                result_policy: AppToolResultPolicy,
            },
        }

        let identity = match &self.transport {
            AttestedAppToolTransport::DeterministicLocal { runtime_ref } => {
                TransportIdentity::DeterministicLocal {
                    tool_ref: &self.tool_ref,
                    runtime_ref,
                    result_policy: self.result_policy,
                }
            },
            AttestedAppToolTransport::External {
                endpoint,
                destination,
                additional_destinations,
            } => TransportIdentity::External {
                tool_ref: &self.tool_ref,
                endpoint_ref: endpoint.endpoint_ref(),
                endpoint_trust_revision: endpoint.trust_revision().get(),
                endpoint_configuration_digest: endpoint.configuration_digest(),
                destination,
                additional_destinations,
                result_policy: self.result_policy,
            },
        };
        digest_serializable(&identity)
    }

    pub(crate) fn identity_digest(&self) -> Result<AppDigest, AppToolDisclosureError> {
        self.digest()
    }
}

/// Single-use authority to disclose already-labeled app content to one exact
/// tool invocation. It carries metadata only, is non-serializable and is not
/// cloneable; result labeling consumes it.
pub struct AppToolDisclosurePermit {
    execution_ref: AppReference,
    invocation_ref: AppReference,
    target: AttestedAppToolTarget,
    transport_digest: AppDigest,
    disclosure_digest: AppDigest,
    disclosure_byte_count: u64,
    disclosure_byte_ceiling: u64,
    authority_digest: AppDigest,
    input_labels: ResolvedAppHandlingLabels,
    classification_floor: AppDataClassification,
    model_processing_ceiling: AppModelProcessing,
    result_byte_ceiling: u64,
    admitted_at: DateTime<Utc>,
}

/// Move-only evidence that a dispatched app tool was the exact deterministic,
/// effect-free transform admitted by the disclosure boundary. It carries no
/// payload bytes and cannot be reconstructed from a provider name or error
/// string. Resource accounting may consume it to record a committed zero-use
/// outcome when the trusted in-process runtime returns an error after dispatch.
pub struct AppToolSafeLocalNoEffect {
    execution_ref: AppReference,
    invocation_ref: AppReference,
}

impl std::fmt::Debug for AppToolSafeLocalNoEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppToolSafeLocalNoEffect")
            .field("verified", &true)
            .finish_non_exhaustive()
    }
}

impl AppToolSafeLocalNoEffect {
    pub fn matches(&self, execution_ref: &AppReference, invocation_ref: &AppReference) -> bool {
        self.execution_ref == *execution_ref && self.invocation_ref == *invocation_ref
    }
}

impl std::fmt::Debug for AppToolDisclosurePermit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppToolDisclosurePermit")
            .field("execution_ref", &self.execution_ref)
            .field("invocation_ref", &self.invocation_ref)
            .field("tool_ref", self.target.tool_ref())
            .field("transport_digest", &self.transport_digest)
            .field("disclosure_digest", &self.disclosure_digest)
            .field("disclosure_byte_count", &self.disclosure_byte_count)
            .field("authority_digest", &self.authority_digest)
            .field("admitted_at", &self.admitted_at)
            .finish()
    }
}

impl AppToolDisclosurePermit {
    pub fn execution_ref(&self) -> &AppReference {
        &self.execution_ref
    }

    pub fn invocation_ref(&self) -> &AppReference {
        &self.invocation_ref
    }

    pub fn target(&self) -> &AttestedAppToolTarget {
        &self.target
    }

    /// Consume this exact disclosure permit as typed safe-local failure
    /// evidence. Only the trusted pure-transform target constructor can make
    /// this succeed; deterministic-local tools which introduce new content
    /// and every external target remain ineligible.
    pub fn into_safe_local_no_effect(
        self,
    ) -> Result<AppToolSafeLocalNoEffect, AppToolDisclosureError> {
        if !matches!(
            self.target.transport,
            AttestedAppToolTransport::DeterministicLocal { .. }
        ) || self.target.result_policy != AppToolResultPolicy::PureTransformInheritsInput
        {
            return Err(AppToolDisclosureError::SafeLocalNoEffectUnavailable);
        }
        Ok(AppToolSafeLocalNoEffect {
            execution_ref: self.execution_ref,
            invocation_ref: self.invocation_ref,
        })
    }

    pub fn authority_digest(&self) -> &AppDigest {
        &self.authority_digest
    }

    pub fn transport_digest(&self) -> &AppDigest {
        &self.transport_digest
    }

    pub fn disclosure_digest(&self) -> &AppDigest {
        &self.disclosure_digest
    }

    pub fn disclosure_byte_count(&self) -> u64 {
        self.disclosure_byte_count
    }

    pub(crate) fn disclosure_byte_ceiling(&self) -> u64 {
        self.disclosure_byte_ceiling
    }

    pub(crate) fn result_byte_ceiling(&self) -> u64 {
        self.result_byte_ceiling
    }

    /// Final dispatcher-side comparison against the immutable bytes it is
    /// about to send and the actual I/O timestamp. External permits stop
    /// matching when their endpoint attestation expires. Callers must mint the
    /// permit after all queue, engagement and trust waits and invoke this check
    /// immediately before I/O; a permit must never cross a generic work queue.
    pub fn matches_disclosure_bytes_at(&self, bytes: &[u8], now: &DateTime<Utc>) -> bool {
        now >= &self.admitted_at
            && match &self.target.transport {
                AttestedAppToolTransport::DeterministicLocal { .. } => true,
                AttestedAppToolTransport::External { endpoint, .. } => endpoint.is_live_at(now),
            }
            && u64::try_from(bytes.len()).is_ok_and(|length| {
                length == self.disclosure_byte_count
                    && AppDigest::blake3(bytes) == self.disclosure_digest
            })
    }
}

/// Revalidate the complete authority and destination immediately before one
/// protected app tool dispatch. This must run after all asynchronous
/// engagement/trust waits; its permit is not safe to enqueue for later I/O.
#[allow(clippy::too_many_arguments)]
pub fn authorize_app_tool_disclosure(
    execution_ref: AppReference,
    invocation_ref: AppReference,
    authority: &ResolvedAppAuthority,
    labels: &ResolvedAppHandlingLabels,
    policy: &ResolvedAppDataHandlingPolicy,
    target: AttestedAppToolTarget,
    disclosure_bytes: &[u8],
    max_disclosure_bytes: usize,
    now: DateTime<Utc>,
) -> Result<AppToolDisclosurePermit, AppToolDisclosureError> {
    authorize_app_tool_disclosure_with_result_ceiling(
        execution_ref,
        invocation_ref,
        authority,
        labels,
        policy,
        target,
        disclosure_bytes,
        max_disclosure_bytes,
        authority.effective_resources.max_payload_bytes,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn authorize_app_tool_disclosure_with_result_ceiling(
    execution_ref: AppReference,
    invocation_ref: AppReference,
    authority: &ResolvedAppAuthority,
    labels: &ResolvedAppHandlingLabels,
    policy: &ResolvedAppDataHandlingPolicy,
    target: AttestedAppToolTarget,
    disclosure_bytes: &[u8],
    max_disclosure_bytes: usize,
    reviewed_result_byte_ceiling: u64,
    now: DateTime<Utc>,
) -> Result<AppToolDisclosurePermit, AppToolDisclosureError> {
    let disclosure_byte_count = u64::try_from(disclosure_bytes.len())
        .map_err(|_| AppToolDisclosureError::DisclosureLengthOverflow)?;
    let requested_disclosure_limit = u64::try_from(max_disclosure_bytes).unwrap_or(u64::MAX);
    // Transient provider input is not persistent app-store payload. The
    // caller supplies a finite reviewed action ceiling; storage.max_bytes is
    // enforced only when bytes enter durable app-owned state.
    let effective_disclosure_limit = requested_disclosure_limit;
    if effective_disclosure_limit == 0 || disclosure_byte_count > effective_disclosure_limit {
        return Err(AppToolDisclosureError::DisclosureBytesExceeded {
            actual: disclosure_bytes.len(),
            limit: usize::try_from(effective_disclosure_limit).unwrap_or(usize::MAX),
        });
    }
    let canonical_authority = authority.canonical_authority_digest()?;
    if canonical_authority != authority.authority_digest {
        return Err(AppToolDisclosureError::AuthorityIntegrityMismatch);
    }
    if policy.authority_digest() != &authority.authority_digest {
        return Err(AppToolDisclosureError::PolicyAuthorityMismatch);
    }
    if !authority.permits_tool(target.tool_ref()) {
        return Err(AppToolDisclosureError::ToolDenied {
            tool_ref: target.tool_ref().clone(),
        });
    }

    match &target.transport {
        AttestedAppToolTransport::DeterministicLocal { .. } => {
            authorize_processing_target(labels, policy, AppProcessingTarget::Deterministic, &now)?;
        },
        AttestedAppToolTransport::External {
            endpoint,
            destination,
            additional_destinations,
        } => {
            for destination in std::iter::once(destination).chain(additional_destinations) {
                if destination.as_str() == ANY_PUBLIC_HOST_DESTINATION {
                    // Only the owner's explicit, authority-bearing grant
                    // admits the any-host marker; no destination list entry
                    // (not even one naming the marker) does.
                    if !authority.effective_any_public_host {
                        return Err(AppToolDisclosureError::NetworkDestinationDenied {
                            destination: destination.clone(),
                        });
                    }
                    authorize_processing_target(
                        labels,
                        policy,
                        AppProcessingTarget::ExternalAnyPublicHost { endpoint },
                        &now,
                    )?;
                    continue;
                }
                ensure_network_destination(authority, destination)?;
                authorize_processing_target(
                    labels,
                    policy,
                    AppProcessingTarget::ExternalTool {
                        endpoint,
                        destination,
                    },
                    &now,
                )?;
            }
        },
    }

    let transport_digest = target.digest()?;
    let disclosure_digest = AppDigest::blake3(disclosure_bytes);
    Ok(AppToolDisclosurePermit {
        execution_ref,
        invocation_ref,
        target,
        transport_digest,
        disclosure_digest,
        disclosure_byte_count,
        disclosure_byte_ceiling: effective_disclosure_limit,
        authority_digest: authority.authority_digest.clone(),
        input_labels: labels.clone(),
        classification_floor: policy.policy().classification_floor,
        model_processing_ceiling: policy.policy().model_processing,
        result_byte_ceiling: reviewed_result_byte_ceiling,
        admitted_at: now,
    })
}

/// The destination an app OS-jail call reaching any public host is attested
/// with (see `os_jail_egress::APP_OS_JAIL_ANY_PUBLIC_HOST_REF`).
const ANY_PUBLIC_HOST_DESTINATION: &str = "destination:any-public-host";

fn ensure_network_destination(
    authority: &ResolvedAppAuthority,
    destination: &AppReference,
) -> Result<(), AppToolDisclosureError> {
    match &authority.effective_network_policy {
        AppNetworkPolicy::Denied => Err(AppToolDisclosureError::NetworkDestinationDenied {
            destination: destination.clone(),
        }),
        AppNetworkPolicy::ApprovedDestinations { destinations }
            if destinations.iter().any(|approved| approved == destination) =>
        {
            Ok(())
        },
        AppNetworkPolicy::ApprovedDestinations { .. } => {
            Err(AppToolDisclosureError::NetworkDestinationDenied {
                destination: destination.clone(),
            })
        },
    }
}

/// Server-derived label for exact tool-result bytes. The result can be
/// serialized into an internal record, but cannot be deserialized from a tool
/// or client and therefore cannot become trusted policy evidence on its own.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppToolResultLabel {
    protocol_version: AppProtocolVersion,
    execution_ref: AppReference,
    invocation_ref: AppReference,
    tool_ref: AppReference,
    transport_digest: AppDigest,
    disclosure_digest: AppDigest,
    disclosure_byte_count: u64,
    authority_digest: AppDigest,
    #[serde(skip_serializing_if = "Option::is_none")]
    effect_binding_digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_effect_result_digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_effect_result_bytes: Option<u64>,
    content_digest: AppDigest,
    byte_count: u64,
    handling_labels: ResolvedAppHandlingLabels,
    handling_digest: AppDigest,
    labeled_at: DateTime<Utc>,
}

impl AppToolResultLabel {
    pub fn execution_ref(&self) -> &AppReference {
        &self.execution_ref
    }

    pub fn invocation_ref(&self) -> &AppReference {
        &self.invocation_ref
    }

    pub fn tool_ref(&self) -> &AppReference {
        &self.tool_ref
    }

    pub fn transport_digest(&self) -> &AppDigest {
        &self.transport_digest
    }

    pub fn disclosure_digest(&self) -> &AppDigest {
        &self.disclosure_digest
    }

    pub fn disclosure_byte_count(&self) -> u64 {
        self.disclosure_byte_count
    }

    pub fn authority_digest(&self) -> &AppDigest {
        &self.authority_digest
    }

    pub fn effect_binding_digest(&self) -> Option<&AppDigest> {
        self.effect_binding_digest.as_ref()
    }

    pub fn raw_effect_result_digest(&self) -> Option<&AppDigest> {
        self.raw_effect_result_digest.as_ref()
    }

    pub fn raw_effect_result_bytes(&self) -> Option<u64> {
        self.raw_effect_result_bytes
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }

    pub fn handling_labels(&self) -> &ResolvedAppHandlingLabels {
        &self.handling_labels
    }

    pub fn labeled_at(&self) -> &DateTime<Utc> {
        &self.labeled_at
    }

    /// Persistable content-free evidence. It can only reject a mismatch after
    /// resume; it cannot recreate either labels or dispatch authority.
    pub fn checkpoint(&self) -> AppToolResultCheckpoint {
        AppToolResultCheckpoint {
            protocol_version: self.protocol_version,
            execution_ref: self.execution_ref.clone(),
            invocation_ref: self.invocation_ref.clone(),
            tool_ref: self.tool_ref.clone(),
            transport_digest: self.transport_digest.clone(),
            disclosure_digest: self.disclosure_digest.clone(),
            disclosure_byte_count: self.disclosure_byte_count,
            authority_digest: self.authority_digest.clone(),
            effect_binding_digest: self.effect_binding_digest.clone(),
            raw_effect_result_digest: self.raw_effect_result_digest.clone(),
            raw_effect_result_bytes: self.raw_effect_result_bytes,
            content_digest: self.content_digest.clone(),
            byte_count: self.byte_count,
            handling_digest: self.handling_digest.clone(),
        }
    }
}

/// Consume a disclosure permit and derive joined result labels from the exact
/// bounded bytes plus independently server-resolved labels for the tool output.
/// A tool-provided label in its response body is not suitable for this input.
pub fn label_app_tool_result_bytes(
    permit: AppToolDisclosurePermit,
    result_bytes: &[u8],
    trusted_result_labels: &ResolvedAppHandlingLabels,
    max_result_bytes: usize,
    labeled_at: DateTime<Utc>,
) -> Result<AppToolResultLabel, AppToolDisclosureError> {
    label_app_tool_result_bytes_with_effect(
        permit,
        result_bytes,
        trusted_result_labels,
        None,
        None,
        max_result_bytes,
        labeled_at,
    )
}

pub(crate) fn label_app_tool_result_bytes_for_effect(
    permit: AppToolDisclosurePermit,
    result_bytes: &[u8],
    trusted_result_labels: &ResolvedAppHandlingLabels,
    effect_binding_digest: AppDigest,
    raw_effect_result_digest: AppDigest,
    raw_effect_result_bytes: u64,
    max_result_bytes: usize,
    labeled_at: DateTime<Utc>,
) -> Result<AppToolResultLabel, AppToolDisclosureError> {
    label_app_tool_result_bytes_with_effect(
        permit,
        result_bytes,
        trusted_result_labels,
        Some(effect_binding_digest),
        Some((raw_effect_result_digest, raw_effect_result_bytes)),
        max_result_bytes,
        labeled_at,
    )
}

fn label_app_tool_result_bytes_with_effect(
    permit: AppToolDisclosurePermit,
    result_bytes: &[u8],
    trusted_result_labels: &ResolvedAppHandlingLabels,
    effect_binding_digest: Option<AppDigest>,
    raw_effect_result: Option<(AppDigest, u64)>,
    max_result_bytes: usize,
    labeled_at: DateTime<Utc>,
) -> Result<AppToolResultLabel, AppToolDisclosureError> {
    let byte_count = u64::try_from(result_bytes.len())
        .map_err(|_| AppToolDisclosureError::ResultLengthOverflow)?;
    let requested_result_limit = u64::try_from(max_result_bytes).unwrap_or(u64::MAX);
    let effective_result_limit = requested_result_limit.min(permit.result_byte_ceiling);
    if effect_binding_digest.is_some() != raw_effect_result.is_some() {
        return Err(AppToolDisclosureError::ResultCheckpointMismatch);
    }
    if effective_result_limit == 0 || byte_count > effective_result_limit {
        return Err(AppToolDisclosureError::ResultBytesExceeded {
            actual: result_bytes.len(),
            limit: usize::try_from(effective_result_limit).unwrap_or(usize::MAX),
        });
    }
    if labeled_at < permit.admitted_at {
        return Err(AppToolDisclosureError::ResultPredatesDispatch);
    }
    let content_digest = AppDigest::blake3(result_bytes);
    let input = permit.input_labels.labels();
    let result = trusted_result_labels.labels();
    let classification = input
        .classification
        .max(result.classification)
        .max(permit.classification_floor);
    let model_processing = input
        .model_processing
        .min(result.model_processing)
        .min(permit.model_processing_ceiling);

    #[derive(Serialize)]
    struct PolicyLineage<'a> {
        protocol_version: AppProtocolVersion,
        execution_ref: &'a AppReference,
        invocation_ref: &'a AppReference,
        tool_ref: &'a AppReference,
        transport_digest: &'a AppDigest,
        disclosure_digest: &'a AppDigest,
        disclosure_byte_count: u64,
        authority_digest: &'a AppDigest,
        effect_binding_digest: Option<&'a AppDigest>,
        raw_effect_result_digest: Option<&'a AppDigest>,
        raw_effect_result_bytes: Option<u64>,
        input_policy_digest: &'a AppDigest,
        result_policy_digest: &'a AppDigest,
        classification: AppDataClassification,
        model_processing: AppModelProcessing,
    }
    let policy_digest = digest_serializable(&PolicyLineage {
        protocol_version: AppProtocolVersion::V1,
        execution_ref: &permit.execution_ref,
        invocation_ref: &permit.invocation_ref,
        tool_ref: permit.target.tool_ref(),
        transport_digest: &permit.transport_digest,
        disclosure_digest: &permit.disclosure_digest,
        disclosure_byte_count: permit.disclosure_byte_count,
        authority_digest: &permit.authority_digest,
        effect_binding_digest: effect_binding_digest.as_ref(),
        raw_effect_result_digest: raw_effect_result.as_ref().map(|(digest, _)| digest),
        raw_effect_result_bytes: raw_effect_result.as_ref().map(|(_, bytes)| *bytes),
        input_policy_digest: &input.policy_digest,
        result_policy_digest: &result.policy_digest,
        classification,
        model_processing,
    })?;

    #[derive(Serialize)]
    struct ProvenanceLineage<'a> {
        protocol_version: AppProtocolVersion,
        execution_ref: &'a AppReference,
        invocation_ref: &'a AppReference,
        tool_ref: &'a AppReference,
        transport_digest: &'a AppDigest,
        disclosure_digest: &'a AppDigest,
        disclosure_byte_count: u64,
        authority_digest: &'a AppDigest,
        effect_binding_digest: Option<&'a AppDigest>,
        raw_effect_result_digest: Option<&'a AppDigest>,
        raw_effect_result_bytes: Option<u64>,
        input_provenance_digest: &'a AppDigest,
        result_provenance_digest: &'a AppDigest,
        content_digest: &'a AppDigest,
        byte_count: u64,
    }
    let provenance_digest = digest_serializable(&ProvenanceLineage {
        protocol_version: AppProtocolVersion::V1,
        execution_ref: &permit.execution_ref,
        invocation_ref: &permit.invocation_ref,
        tool_ref: permit.target.tool_ref(),
        transport_digest: &permit.transport_digest,
        disclosure_digest: &permit.disclosure_digest,
        disclosure_byte_count: permit.disclosure_byte_count,
        authority_digest: &permit.authority_digest,
        effect_binding_digest: effect_binding_digest.as_ref(),
        raw_effect_result_digest: raw_effect_result.as_ref().map(|(digest, _)| digest),
        raw_effect_result_bytes: raw_effect_result.as_ref().map(|(_, bytes)| *bytes),
        input_provenance_digest: &input.provenance_digest,
        result_provenance_digest: &result.provenance_digest,
        content_digest: &content_digest,
        byte_count,
    })?;

    let handling_labels = ResolvedAppHandlingLabels::from_trusted_policy(AppHandlingLabels {
        classification,
        model_processing,
        policy_digest,
        provenance_digest,
    });
    let handling_digest = handling_digest(&handling_labels)?;
    let (raw_effect_result_digest, raw_effect_result_bytes) = raw_effect_result
        .map(|(digest, bytes)| (Some(digest), Some(bytes)))
        .unwrap_or((None, None));
    Ok(AppToolResultLabel {
        protocol_version: AppProtocolVersion::V1,
        execution_ref: permit.execution_ref,
        invocation_ref: permit.invocation_ref,
        tool_ref: permit.target.tool_ref,
        transport_digest: permit.transport_digest,
        disclosure_digest: permit.disclosure_digest,
        disclosure_byte_count: permit.disclosure_byte_count,
        authority_digest: permit.authority_digest,
        effect_binding_digest,
        raw_effect_result_digest,
        raw_effect_result_bytes,
        content_digest,
        byte_count,
        handling_labels,
        handling_digest,
        labeled_at,
    })
}

/// Label one server-owned app-store read without pretending it was an
/// app-declared external capability. The store query is structural workflow
/// machinery: its exact request, current authority and store-derived result
/// labels are all supplied by the workflow owner, never by response JSON.
#[allow(clippy::too_many_arguments)]
pub(crate) fn label_app_internal_store_result_bytes(
    execution_ref: AppReference,
    invocation_ref: AppReference,
    authority: &ResolvedAppAuthority,
    input_labels: &ResolvedAppHandlingLabels,
    effective_policy: &ResolvedAppDataHandlingPolicy,
    request_bytes: &[u8],
    result_bytes: &[u8],
    store_result_labels: &ResolvedAppHandlingLabels,
    max_result_bytes: usize,
    labeled_at: DateTime<Utc>,
) -> Result<AppToolResultLabel, AppToolDisclosureError> {
    if request_bytes.is_empty() || result_bytes.is_empty() {
        return Err(AppToolDisclosureError::ResultCheckpointMismatch);
    }
    if authority.canonical_authority_digest()? != authority.authority_digest
        || effective_policy.authority_digest() != &authority.authority_digest
    {
        return Err(AppToolDisclosureError::AuthorityIntegrityMismatch);
    }
    let byte_count = u64::try_from(result_bytes.len())
        .map_err(|_| AppToolDisclosureError::ResultLengthOverflow)?;
    let limit = u64::try_from(max_result_bytes)
        .unwrap_or(u64::MAX)
        .min(authority.effective_resources.max_payload_bytes);
    if byte_count > limit {
        return Err(AppToolDisclosureError::ResultBytesExceeded {
            actual: result_bytes.len(),
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
        });
    }
    let disclosure_byte_count = u64::try_from(request_bytes.len())
        .map_err(|_| AppToolDisclosureError::DisclosureLengthOverflow)?;
    let tool_ref = AppReference::parse("capability:app_store_query")
        .map_err(|error| AppToolDisclosureError::DigestEncoding(error.to_string()))?;
    let transport_digest = digest_serializable(&serde_json::json!({
        "kind": "server_internal_app_store_query_v1",
        "tool_ref": &tool_ref,
        "authority_digest": &authority.authority_digest,
    }))?;
    let disclosure_digest = AppDigest::blake3(request_bytes);
    let content_digest = AppDigest::blake3(result_bytes);
    let input = input_labels.labels();
    let result = store_result_labels.labels();
    let classification = input
        .classification
        .max(result.classification)
        .max(effective_policy.policy().classification_floor);
    let model_processing = input
        .model_processing
        .min(result.model_processing)
        .min(effective_policy.policy().model_processing);
    let policy_digest = digest_serializable(&serde_json::json!({
        "kind": "app_internal_store_result_policy_v1",
        "execution_ref": &execution_ref,
        "invocation_ref": &invocation_ref,
        "tool_ref": &tool_ref,
        "transport_digest": &transport_digest,
        "disclosure_digest": &disclosure_digest,
        "disclosure_byte_count": disclosure_byte_count,
        "authority_digest": &authority.authority_digest,
        "input_policy_digest": &input.policy_digest,
        "result_policy_digest": &result.policy_digest,
        "classification": classification,
        "model_processing": model_processing,
    }))?;
    let provenance_digest = digest_serializable(&serde_json::json!({
        "kind": "app_internal_store_result_provenance_v1",
        "execution_ref": &execution_ref,
        "invocation_ref": &invocation_ref,
        "tool_ref": &tool_ref,
        "transport_digest": &transport_digest,
        "authority_digest": &authority.authority_digest,
        "input_provenance_digest": &input.provenance_digest,
        "result_provenance_digest": &result.provenance_digest,
        "content_digest": &content_digest,
        "byte_count": byte_count,
    }))?;
    let handling_labels = ResolvedAppHandlingLabels::from_trusted_policy(AppHandlingLabels {
        classification,
        model_processing,
        policy_digest,
        provenance_digest,
    });
    let handling_digest = handling_digest(&handling_labels)?;
    Ok(AppToolResultLabel {
        protocol_version: AppProtocolVersion::V1,
        execution_ref,
        invocation_ref,
        tool_ref,
        transport_digest,
        disclosure_digest,
        disclosure_byte_count,
        authority_digest: authority.authority_digest.clone(),
        effect_binding_digest: None,
        raw_effect_result_digest: None,
        raw_effect_result_bytes: None,
        content_digest,
        byte_count,
        handling_labels,
        handling_digest,
        labeled_at,
    })
}

/// Content-free pause/resume evidence. Deserialization is safe because this
/// type never authorizes disclosure; a fresh result label must match it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppToolResultCheckpoint {
    protocol_version: AppProtocolVersion,
    execution_ref: AppReference,
    invocation_ref: AppReference,
    tool_ref: AppReference,
    transport_digest: AppDigest,
    disclosure_digest: AppDigest,
    disclosure_byte_count: u64,
    authority_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effect_binding_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_effect_result_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_effect_result_bytes: Option<u64>,
    content_digest: AppDigest,
    byte_count: u64,
    handling_digest: AppDigest,
}

impl AppToolResultCheckpoint {
    pub fn execution_ref(&self) -> &AppReference {
        &self.execution_ref
    }

    pub fn invocation_ref(&self) -> &AppReference {
        &self.invocation_ref
    }

    pub fn tool_ref(&self) -> &AppReference {
        &self.tool_ref
    }

    pub fn disclosure_digest(&self) -> &AppDigest {
        &self.disclosure_digest
    }

    pub fn authority_digest(&self) -> &AppDigest {
        &self.authority_digest
    }

    pub fn effect_binding_digest(&self) -> Option<&AppDigest> {
        self.effect_binding_digest.as_ref()
    }

    pub fn raw_effect_result_digest(&self) -> Option<&AppDigest> {
        self.raw_effect_result_digest.as_ref()
    }

    pub fn raw_effect_result_bytes(&self) -> Option<u64> {
        self.raw_effect_result_bytes
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }

    pub fn handling_digest(&self) -> &AppDigest {
        &self.handling_digest
    }

    /// A checkpoint is only a rejection token. This verifies exact bytes but
    /// does not make a deserialized checkpoint authoritative; callers must
    /// additionally require membership in the server-owned run sidecar and
    /// re-resolve current app authority.
    pub fn matches_content_bytes(&self, exact_bytes: &[u8]) -> bool {
        u64::try_from(exact_bytes.len()).ok() == Some(self.byte_count)
            && AppDigest::blake3(exact_bytes) == self.content_digest
    }

    pub fn matches_result(&self, result: &AppToolResultLabel) -> bool {
        self.protocol_version == result.protocol_version
            && self.execution_ref == result.execution_ref
            && self.invocation_ref == result.invocation_ref
            && self.tool_ref == result.tool_ref
            && self.transport_digest == result.transport_digest
            && self.disclosure_digest == result.disclosure_digest
            && self.disclosure_byte_count == result.disclosure_byte_count
            && self.authority_digest == result.authority_digest
            && self.effect_binding_digest == result.effect_binding_digest
            && self.raw_effect_result_digest == result.raw_effect_result_digest
            && self.raw_effect_result_bytes == result.raw_effect_result_bytes
            && self.content_digest == result.content_digest
            && self.byte_count == result.byte_count
            && self.handling_digest == result.handling_digest
            && handling_digest(&result.handling_labels)
                .is_ok_and(|digest| digest == self.handling_digest)
    }
}

/// Exact bounded tool-result bytes paired with content-free server evidence.
/// This transport record is serializable for history/pause continuity, but it
/// never authorizes itself: every resume must match its checkpoint against the
/// exact execution sidecar and freshly resolved workflow authority.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppLabeledToolResultRecord {
    checkpoint: AppToolResultCheckpoint,
    /// Server-resolved labels captured with the exact result. Deserialization
    /// never makes these authoritative: replay verifies their digest and then
    /// re-runs current hidden-consumer policy before exposing `value`.
    handling_labels: AppHandlingLabels,
    value: Value,
}

/// Non-authoritative digest carried by a durable app pause. It proves which
/// exact bounded pause bytes were admitted to protected workflow-continuation
/// storage, but cannot recreate labels, policy or current app authority.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowContinuationCheckpoint {
    protocol_version: AppProtocolVersion,
    execution_ref: AppReference,
    authority_digest: AppDigest,
    content_digest: AppDigest,
    byte_count: u64,
    handling_digest: AppDigest,
}

impl std::fmt::Debug for AppWorkflowContinuationCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppWorkflowContinuationCheckpoint")
            .field("execution_ref", &self.execution_ref)
            .field("authority_digest", &self.authority_digest)
            .field("content_digest", &self.content_digest)
            .field("byte_count", &self.byte_count)
            .field("handling_digest", &self.handling_digest)
            .finish()
    }
}

impl AppWorkflowContinuationCheckpoint {
    pub fn execution_ref(&self) -> &AppReference {
        &self.execution_ref
    }

    pub fn authority_digest(&self) -> &AppDigest {
        &self.authority_digest
    }

    pub fn matches_bytes(&self, bytes: &[u8]) -> bool {
        u64::try_from(bytes.len()).is_ok_and(|length| length == self.byte_count)
            && AppDigest::blake3(bytes) == self.content_digest
    }
}

/// Admit the complete exact pause payload to the dedicated protected
/// continuation sink. The returned checkpoint is only a replay rejection
/// token; callers must re-run this function with current server labels/policy
/// before hydrating the bytes.
pub fn authorize_app_workflow_continuation(
    execution_ref: AppReference,
    authority: &ResolvedAppAuthority,
    labels: &ResolvedAppHandlingLabels,
    bytes: &[u8],
    now: DateTime<Utc>,
) -> Result<AppWorkflowContinuationCheckpoint, AppToolDisclosureError> {
    let byte_count =
        u64::try_from(bytes.len()).map_err(|_| AppToolDisclosureError::DisclosureLengthOverflow)?;
    if byte_count == 0 || byte_count > authority.effective_resources.max_payload_bytes {
        return Err(AppToolDisclosureError::DisclosureBytesExceeded {
            actual: bytes.len(),
            limit: usize::try_from(authority.effective_resources.max_payload_bytes)
                .unwrap_or(usize::MAX),
        });
    }
    if authority.canonical_authority_digest()? != authority.authority_digest {
        return Err(AppToolDisclosureError::AuthorityIntegrityMismatch);
    }
    let policy = ResolvedAppDataHandlingPolicy::from_resolved_authority(authority);
    let capability = AppConsumerCapability::from_trusted_registry(
        AppHiddenConsumer::WorkflowContinuation,
        true,
        AppDataClassification::Secret,
        super::policy::AppPayloadCapability::ProtectedLabeledRetention,
        authority.grant_revision,
    );
    let decision = decide_hidden_consumer(Some(labels), &policy, &capability, None, &now);
    if decision.decision != AppConsumerDecision::FullContent {
        return Err(AppToolDisclosureError::HiddenConsumerDenied {
            consumer: AppHiddenConsumer::WorkflowContinuation,
            reason: decision.reason,
        });
    }
    Ok(AppWorkflowContinuationCheckpoint {
        protocol_version: AppProtocolVersion::V1,
        execution_ref,
        authority_digest: authority.authority_digest.clone(),
        content_digest: AppDigest::blake3(bytes),
        byte_count,
        handling_digest: handling_digest(labels)?,
    })
}

/// Re-admit an exact durable pause under current authority. A checkpoint that
/// was copied, reordered, modified or created under an older authority/policy
/// cannot authorize any goal/history bytes.
pub fn reauthorize_app_workflow_continuation(
    checkpoint: &AppWorkflowContinuationCheckpoint,
    execution_ref: &AppReference,
    authority: &ResolvedAppAuthority,
    labels: &ResolvedAppHandlingLabels,
    bytes: &[u8],
    now: DateTime<Utc>,
) -> Result<(), AppToolDisclosureError> {
    if checkpoint.execution_ref() != execution_ref
        || checkpoint.authority_digest() != &authority.authority_digest
        || checkpoint.handling_digest != handling_digest(labels)?
        || !checkpoint.matches_bytes(bytes)
    {
        return Err(AppToolDisclosureError::ResultCheckpointMismatch);
    }
    let current =
        authorize_app_workflow_continuation(execution_ref.clone(), authority, labels, bytes, now)?;
    if &current == checkpoint {
        Ok(())
    } else {
        Err(AppToolDisclosureError::ResultCheckpointMismatch)
    }
}

impl std::fmt::Debug for AppLabeledToolResultRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppLabeledToolResultRecord")
            .field("execution_ref", self.checkpoint.execution_ref())
            .field("invocation_ref", self.checkpoint.invocation_ref())
            .field("tool_ref", self.checkpoint.tool_ref())
            .field("content_digest", self.checkpoint.content_digest())
            .field("byte_count", &self.checkpoint.byte_count())
            .field("handling_digest", self.checkpoint.handling_digest())
            .finish()
    }
}

impl AppLabeledToolResultRecord {
    /// Build the ordinary workflow-continuation checkpoint for a result whose
    /// bytes and handling labels were already produced by the Artifact-owned
    /// sealed callable-agent carrier. This constructor is intentionally
    /// visible only inside the Apps owner modules; an executor/provider cannot
    /// promote arbitrary labels into trusted continuation state.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_artifact_agent_tool_owner(
        execution_ref: AppReference,
        invocation_ref: AppReference,
        tool_ref: AppReference,
        authority_digest: AppDigest,
        carrier_digest: AppDigest,
        value: Value,
        handling_labels: AppHandlingLabels,
    ) -> Result<Self, AppToolDisclosureError> {
        let exact_bytes = crate::magician_v2::json_traversal::canonical_json_bytes(&value)
            .map_err(|error| AppToolDisclosureError::DigestEncoding(error.to_string()))?;
        let byte_count = u64::try_from(exact_bytes.len())
            .map_err(|_| AppToolDisclosureError::ResultLengthOverflow)?;
        let content_digest = AppDigest::blake3(&exact_bytes);
        let transport_digest = digest_serializable(&serde_json::json!({
            "schema": "magician.app-agent-tool-artifact-carrier.v1",
            "execution_ref": &execution_ref,
            "invocation_ref": &invocation_ref,
            "tool_ref": &tool_ref,
            "authority_digest": &authority_digest,
            "carrier_digest": &carrier_digest,
        }))?;
        let resolved = ResolvedAppHandlingLabels::from_trusted_policy(handling_labels.clone());
        let checkpoint = AppToolResultCheckpoint {
            protocol_version: AppProtocolVersion::V1,
            execution_ref,
            invocation_ref,
            tool_ref,
            transport_digest,
            disclosure_digest: carrier_digest,
            disclosure_byte_count: 0,
            authority_digest,
            effect_binding_digest: None,
            raw_effect_result_digest: None,
            raw_effect_result_bytes: None,
            content_digest,
            byte_count,
            handling_digest: handling_digest(&resolved)?,
        };
        let record = Self {
            checkpoint,
            handling_labels,
            value,
        };
        let _ = record.validate_exact_bytes()?;
        Ok(record)
    }

    pub fn from_server_label(
        label: &AppToolResultLabel,
        value: Value,
    ) -> Result<Self, AppToolDisclosureError> {
        let exact_bytes = crate::magician_v2::json_traversal::canonical_json_bytes(&value)
            .map_err(|error| AppToolDisclosureError::DigestEncoding(error.to_string()))?;
        let checkpoint = label.checkpoint();
        if !checkpoint.matches_result(label) || !checkpoint.matches_content_bytes(&exact_bytes) {
            return Err(AppToolDisclosureError::ResultCheckpointMismatch);
        }
        Ok(Self {
            checkpoint,
            handling_labels: label.handling_labels().labels().clone(),
            value,
        })
    }

    pub fn checkpoint(&self) -> &AppToolResultCheckpoint {
        &self.checkpoint
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub(crate) fn handling_labels(&self) -> &AppHandlingLabels {
        &self.handling_labels
    }

    pub fn validate_exact_bytes(&self) -> Result<Vec<u8>, AppToolDisclosureError> {
        let exact_bytes = crate::magician_v2::json_traversal::canonical_json_bytes(&self.value)
            .map_err(|error| AppToolDisclosureError::DigestEncoding(error.to_string()))?;
        let resolved_labels =
            ResolvedAppHandlingLabels::from_trusted_policy(self.handling_labels.clone());
        if self.checkpoint.matches_content_bytes(&exact_bytes)
            && handling_digest(&resolved_labels)? == self.checkpoint.handling_digest
        {
            Ok(exact_bytes)
        } else {
            Err(AppToolDisclosureError::ResultCheckpointMismatch)
        }
    }
}

/// Re-authorize an exact sidecar-owned result for one current hidden consumer.
/// The persisted labels are only accepted after their canonical digest and the
/// checkpoint's original authority match current server policy.
pub fn reauthorize_app_labeled_tool_result(
    record: &AppLabeledToolResultRecord,
    policy: &ResolvedAppDataHandlingPolicy,
    capability: &AppConsumerCapability,
    target: Option<AppProcessingTarget<'_>>,
    now: DateTime<Utc>,
) -> Result<AppHiddenConsumerAdmission, AppToolDisclosureError> {
    let _ = record.validate_exact_bytes()?;
    if policy.authority_digest() != record.checkpoint.authority_digest() {
        return Err(AppToolDisclosureError::PolicyAuthorityMismatch);
    }
    let labels = ResolvedAppHandlingLabels::from_trusted_policy(record.handling_labels.clone());
    let decision = decide_hidden_consumer(Some(&labels), policy, capability, target, &now);
    if decision.decision == AppConsumerDecision::Denied {
        return Err(AppToolDisclosureError::HiddenConsumerDenied {
            consumer: capability.consumer(),
            reason: decision.reason,
        });
    }
    let receipt = AppHiddenConsumerReceipt {
        consumer: capability.consumer(),
        reason: decision.reason,
        authority_digest: record.checkpoint.authority_digest().clone(),
        content_digest: record.checkpoint.content_digest().clone(),
        handling_digest: record.checkpoint.handling_digest().clone(),
        admitted_at: now,
    };
    match decision.decision {
        AppConsumerDecision::FullContent => Ok(AppHiddenConsumerAdmission::FullContent(
            AppHiddenContentPermit { receipt },
        )),
        AppConsumerDecision::MetadataOnly => Ok(AppHiddenConsumerAdmission::MetadataOnly(
            AppHiddenMetadataReceipt { receipt },
        )),
        AppConsumerDecision::Denied => Err(AppToolDisclosureError::HiddenConsumerDecisionInvariant),
    }
}

#[derive(Debug)]
struct AppHiddenConsumerReceipt {
    consumer: AppHiddenConsumer,
    reason: AppConsumerDecisionReason,
    authority_digest: AppDigest,
    content_digest: AppDigest,
    handling_digest: AppDigest,
    admitted_at: DateTime<Utc>,
}

/// Move-only authority for a hidden consumer to receive exact protected bytes.
/// Metadata-only consumers can never obtain this type.
#[derive(Debug)]
pub struct AppHiddenContentPermit {
    receipt: AppHiddenConsumerReceipt,
}

/// Content-free receipt for a hidden consumer that may observe only metadata.
#[derive(Debug)]
pub struct AppHiddenMetadataReceipt {
    receipt: AppHiddenConsumerReceipt,
}

/// Typed hidden-consumer outcome. Callers that handle payload bytes must
/// require [`AppHiddenContentPermit`], making a metadata-only decision
/// impossible to use as content authority by mistake.
#[derive(Debug)]
pub enum AppHiddenConsumerAdmission {
    FullContent(AppHiddenContentPermit),
    MetadataOnly(AppHiddenMetadataReceipt),
}

impl AppHiddenConsumerAdmission {
    pub fn consumer(&self) -> AppHiddenConsumer {
        match self {
            Self::FullContent(permit) => permit.receipt.consumer,
            Self::MetadataOnly(receipt) => receipt.receipt.consumer,
        }
    }

    pub fn decision(&self) -> AppConsumerDecision {
        match self {
            Self::FullContent(_) => AppConsumerDecision::FullContent,
            Self::MetadataOnly(_) => AppConsumerDecision::MetadataOnly,
        }
    }

    pub fn reason(&self) -> AppConsumerDecisionReason {
        match self {
            Self::FullContent(permit) => permit.receipt.reason,
            Self::MetadataOnly(receipt) => receipt.receipt.reason,
        }
    }

    pub fn content_digest(&self) -> &AppDigest {
        match self {
            Self::FullContent(permit) => &permit.receipt.content_digest,
            Self::MetadataOnly(receipt) => &receipt.receipt.content_digest,
        }
    }

    pub fn authority_digest(&self) -> &AppDigest {
        match self {
            Self::FullContent(permit) => &permit.receipt.authority_digest,
            Self::MetadataOnly(receipt) => &receipt.receipt.authority_digest,
        }
    }

    pub fn handling_digest(&self) -> &AppDigest {
        match self {
            Self::FullContent(permit) => &permit.receipt.handling_digest,
            Self::MetadataOnly(receipt) => &receipt.receipt.handling_digest,
        }
    }

    pub fn admitted_at(&self) -> &DateTime<Utc> {
        match self {
            Self::FullContent(permit) => &permit.receipt.admitted_at,
            Self::MetadataOnly(receipt) => &receipt.receipt.admitted_at,
        }
    }
}

impl AppHiddenContentPermit {
    pub fn consumer(&self) -> AppHiddenConsumer {
        self.receipt.consumer
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.receipt.content_digest
    }
}

impl AppHiddenMetadataReceipt {
    pub fn consumer(&self) -> AppHiddenConsumer {
        self.receipt.consumer
    }
}

/// Decide whether a hidden consumer may receive bytes for one exact labeled
/// tool result. Denial is an error; metadata-only and full-content admissions
/// remain distinct so a caller cannot accidentally treat metadata authority as
/// permission to read payload bytes.
pub fn authorize_app_hidden_consumer(
    result: &AppToolResultLabel,
    policy: &ResolvedAppDataHandlingPolicy,
    capability: &AppConsumerCapability,
    target: Option<AppProcessingTarget<'_>>,
    now: DateTime<Utc>,
) -> Result<AppHiddenConsumerAdmission, AppToolDisclosureError> {
    if policy.authority_digest() != &result.authority_digest {
        return Err(AppToolDisclosureError::PolicyAuthorityMismatch);
    }
    let decision = decide_hidden_consumer(
        Some(&result.handling_labels),
        policy,
        capability,
        target,
        &now,
    );
    if decision.decision == AppConsumerDecision::Denied {
        return Err(AppToolDisclosureError::HiddenConsumerDenied {
            consumer: capability.consumer(),
            reason: decision.reason,
        });
    }
    let receipt = AppHiddenConsumerReceipt {
        consumer: capability.consumer(),
        reason: decision.reason,
        authority_digest: result.authority_digest.clone(),
        content_digest: result.content_digest.clone(),
        handling_digest: handling_digest(&result.handling_labels)?,
        admitted_at: now,
    };
    match decision.decision {
        AppConsumerDecision::FullContent => Ok(AppHiddenConsumerAdmission::FullContent(
            AppHiddenContentPermit { receipt },
        )),
        AppConsumerDecision::MetadataOnly => Ok(AppHiddenConsumerAdmission::MetadataOnly(
            AppHiddenMetadataReceipt { receipt },
        )),
        AppConsumerDecision::Denied => Err(AppToolDisclosureError::HiddenConsumerDecisionInvariant),
    }
}

fn handling_digest(
    labels: &ResolvedAppHandlingLabels,
) -> Result<AppDigest, AppToolDisclosureError> {
    digest_serializable(labels.labels())
}

fn digest_serializable(value: &impl Serialize) -> Result<AppDigest, AppToolDisclosureError> {
    serde_json::to_vec(value)
        .map(|bytes| AppDigest::blake3(&bytes))
        .map_err(|error| AppToolDisclosureError::DigestEncoding(error.to_string()))
}

#[derive(Debug, Error)]
pub enum AppToolDisclosureError {
    #[error(transparent)]
    Authority(#[from] AppAuthorityError),
    #[error(transparent)]
    Policy(#[from] AppPolicyError),
    #[error("resolved app authority failed its canonical integrity check")]
    AuthorityIntegrityMismatch,
    #[error("resolved app policy is bound to a different authority")]
    PolicyAuthorityMismatch,
    #[error("app authority does not permit tool `{tool_ref}`")]
    ToolDenied { tool_ref: AppReference },
    #[error("app network authority does not permit destination `{destination}`")]
    NetworkDestinationDenied { destination: AppReference },
    #[error("tool disclosure contains {actual} bytes; maximum is {limit}")]
    DisclosureBytesExceeded { actual: usize, limit: usize },
    #[error("tool disclosure byte length cannot be represented by the contract")]
    DisclosureLengthOverflow,
    #[error("tool result contains {actual} bytes; maximum is {limit}")]
    ResultBytesExceeded { actual: usize, limit: usize },
    #[error("tool result byte length cannot be represented by the contract")]
    ResultLengthOverflow,
    #[error("tool result timestamp predates its disclosure admission")]
    ResultPredatesDispatch,
    #[error("tool disclosure does not prove a deterministic local no-effect outcome")]
    SafeLocalNoEffectUnavailable,
    #[error("app tool-result checkpoint does not match the exact result bytes")]
    ResultCheckpointMismatch,
    #[error("hidden-consumer policy returned an inconsistent denied decision")]
    HiddenConsumerDecisionInvariant,
    #[error("failed to encode app tool disclosure identity: {0}")]
    DigestEncoding(String),
    #[error("hidden consumer `{consumer:?}` is denied: {reason:?}")]
    HiddenConsumerDenied {
        consumer: AppHiddenConsumer,
        reason: AppConsumerDecisionReason,
    },
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::magician_v2::apps::{
        authority::{AppScopeAuthentication, ResolvedAppAuthority},
        models::{AppInstallationId, AppRevision, AppScopeBindingRef},
        policy::{AppEndpointClass, AppPayloadCapability},
        records::{
            AppBackgroundExecution, AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion,
            AppNetworkPolicy, AppPersonalAgentAccess, AppResourceCeiling,
        },
    };

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("valid reference")
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).expect("positive revision")
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 17, 10, 0, 0)
            .single()
            .expect("valid timestamp")
    }

    fn policy(destinations: &[&str]) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: if destinations.is_empty() {
                AppExternalEgress::Denied
            } else {
                AppExternalEgress::ApprovedDestinations
            },
            approved_destinations: destinations
                .iter()
                .map(|destination| reference(destination))
                .collect(),
        }
    }

    fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 1_000,
            max_output_tokens: 1_000,
            max_cost_microusd: 1_000,
            max_paid_tool_invocations: 10,
            max_active_seconds: 60,
            max_lifetime_seconds: 120,
            max_browser_network_actions: 10,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 0,
            max_records: 10,
            max_payload_bytes: 4_096,
            max_attachment_bytes: 4_096,
            max_monthly_tokens: 10_000,
            max_monthly_cost_microusd: 10_000,
        }
    }

    fn authority(
        data_destinations: &[&str],
        network_destinations: &[&str],
    ) -> ResolvedAppAuthority {
        let tool_ref = reference("tool:mail.send");
        let mut authority = ResolvedAppAuthority {
            scope_binding_ref: AppScopeBindingRef::parse("scope_binding_test").unwrap(),
            actor_ref: reference("actor:owner"),
            session_ref: reference("session:test"),
            authentication: AppScopeAuthentication::AuthenticatedSession,
            authentication_revision: revision(1),
            installation_id: AppInstallationId::parse("install_test").unwrap(),
            installation_generation: 1,
            package_revision_ref: reference("package:test@1"),
            grant_revision: revision(1),
            grant_authority_digest: AppDigest::blake3(b"grant"),
            schema_revision: revision(1),
            surface_revision: Some(revision(1)),
            authority_digest: AppDigest::blake3(b"placeholder"),
            effective_tools: BTreeSet::from([tool_ref]),
            effective_context_reads: BTreeSet::new(),
            effective_data_handling_policy: policy(data_destinations),
            effective_background_execution: AppBackgroundExecution::Denied,
            effective_network_policy: if network_destinations.is_empty() {
                AppNetworkPolicy::Denied
            } else {
                AppNetworkPolicy::ApprovedDestinations {
                    destinations: network_destinations
                        .iter()
                        .map(|destination| reference(destination))
                        .collect(),
                }
            },
            effective_resources: resources(),
            effective_any_public_host: false,
            resolved_at: now(),
        };
        authority.authority_digest = authority.canonical_authority_digest().unwrap();
        authority
    }

    fn labels(
        classification: AppDataClassification,
        processing: AppModelProcessing,
        seed: &str,
    ) -> ResolvedAppHandlingLabels {
        ResolvedAppHandlingLabels::from_trusted_policy(AppHandlingLabels {
            classification,
            model_processing: processing,
            policy_digest: AppDigest::blake3(format!("policy-{seed}").as_bytes()),
            provenance_digest: AppDigest::blake3(format!("provenance-{seed}").as_bytes()),
        })
    }

    fn endpoint() -> AttestedAppEndpoint {
        AttestedAppEndpoint::from_trusted_resolver(
            reference("endpoint:mail"),
            AppEndpointClass::External,
            false,
            revision(1),
            AppDigest::blake3(b"mail-endpoint"),
            now(),
            now() + Duration::minutes(5),
        )
        .unwrap()
    }

    fn external_target(destination: &str) -> AttestedAppToolTarget {
        AttestedAppToolTarget::from_trusted_external_dispatcher(
            reference("tool:mail.send"),
            endpoint(),
            reference(destination),
        )
    }

    fn local_target() -> AttestedAppToolTarget {
        AttestedAppToolTarget::from_trusted_local_dispatcher(
            reference("tool:mail.send"),
            reference("runtime:deterministic-mail-fixture"),
        )
    }

    fn clock_target() -> AttestedAppToolTarget {
        AttestedAppToolTarget::from_trusted_local_clock_dispatcher(
            reference("tool:mail.send"),
            reference("runtime:compiled:time_math:now:v1"),
        )
    }

    fn pure_transform_target() -> AttestedAppToolTarget {
        AttestedAppToolTarget::from_trusted_pure_transform_dispatcher(
            reference("tool:mail.send"),
            reference("runtime:pure-transform-fixture"),
        )
    }

    fn authorize(
        authority: &ResolvedAppAuthority,
        target: AttestedAppToolTarget,
    ) -> Result<AppToolDisclosurePermit, AppToolDisclosureError> {
        let policy = ResolvedAppDataHandlingPolicy::from_resolved_authority(authority);
        authorize_app_tool_disclosure(
            reference("execution:1"),
            reference("invocation:1"),
            authority,
            &labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "input",
            ),
            &policy,
            target,
            br#"{"query":"approved"}"#,
            1_024,
            now(),
        )
    }

    #[test]
    fn external_dispatch_requires_tool_data_egress_and_network_authority() {
        let allowed = authority(&["destination:mail"], &["destination:mail"]);
        authorize(&allowed, external_target("destination:mail")).unwrap();

        let no_data_egress = authority(&[], &["destination:mail"]);
        assert!(matches!(
            authorize(&no_data_egress, external_target("destination:mail")),
            Err(AppToolDisclosureError::Policy(
                AppPolicyError::ExternalDestinationDenied
            ))
        ));

        let no_network = authority(&["destination:mail"], &[]);
        assert!(matches!(
            authorize(&no_network, external_target("destination:mail")),
            Err(AppToolDisclosureError::NetworkDestinationDenied { .. })
        ));

        let other_destination = authority(&["destination:mail"], &["destination:mail"]);
        assert!(matches!(
            authorize(
                &other_destination,
                external_target("destination:unreviewed")
            ),
            Err(AppToolDisclosureError::NetworkDestinationDenied { .. })
        ));
    }

    #[test]
    fn authority_mutation_and_cross_authority_policy_reuse_fail_closed() {
        let mut mutated = authority(&[], &[]);
        mutated.effective_tools.clear();
        assert!(matches!(
            authorize(&mutated, local_target()),
            Err(AppToolDisclosureError::AuthorityIntegrityMismatch)
        ));

        let first = authority(&[], &[]);
        let mut second = authority(&[], &[]);
        second.session_ref = reference("session:other");
        second.authority_digest = second.canonical_authority_digest().unwrap();
        let stale_policy = ResolvedAppDataHandlingPolicy::from_resolved_authority(&first);
        assert!(matches!(
            authorize_app_tool_disclosure(
                reference("execution:1"),
                reference("invocation:1"),
                &second,
                &labels(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                    "input"
                ),
                &stale_policy,
                local_target(),
                br#"{"query":"approved"}"#,
                1_024,
                now()
            ),
            Err(AppToolDisclosureError::PolicyAuthorityMismatch)
        ));
    }

    #[test]
    fn only_exact_pure_transform_permit_can_prove_safe_local_no_effect() {
        let local_authority = authority(&[], &[]);
        let evidence = authorize(&local_authority, pure_transform_target())
            .unwrap()
            .into_safe_local_no_effect()
            .unwrap();
        assert!(evidence.matches(&reference("execution:1"), &reference("invocation:1")));
        assert!(!evidence.matches(&reference("execution:other"), &reference("invocation:1")));

        assert!(matches!(
            authorize(&local_authority, local_target())
                .unwrap()
                .into_safe_local_no_effect(),
            Err(AppToolDisclosureError::SafeLocalNoEffectUnavailable)
        ));
        assert!(matches!(
            authorize(&local_authority, clock_target())
                .unwrap()
                .into_safe_local_no_effect(),
            Err(AppToolDisclosureError::SafeLocalNoEffectUnavailable)
        ));
        assert_eq!(
            clock_target().result_policy(),
            AppToolResultPolicy::TrustedLocalClock
        );
        let external_authority = authority(&["destination:mail"], &["destination:mail"]);
        assert!(matches!(
            authorize(&external_authority, external_target("destination:mail"))
                .unwrap()
                .into_safe_local_no_effect(),
            Err(AppToolDisclosureError::SafeLocalNoEffectUnavailable)
        ));
    }

    #[test]
    fn exact_result_bytes_receive_conservative_joined_lineage() {
        let local_authority = authority(&[], &[]);
        let permit = authorize(&local_authority, local_target()).unwrap();
        assert!(permit.matches_disclosure_bytes_at(br#"{"query":"approved"}"#, &now()));
        assert!(!permit.matches_disclosure_bytes_at(br#"{"query":"changed"}"#, &now()));
        assert!(!permit.matches_disclosure_bytes_at(
            br#"{"query":"approved"}"#,
            &(now() - Duration::microseconds(1))
        ));
        let result = label_app_tool_result_bytes(
            permit,
            b"sensitive result",
            &labels(
                AppDataClassification::Secret,
                AppModelProcessing::None,
                "result",
            ),
            1_024,
            now() + Duration::seconds(1),
        )
        .unwrap();
        assert_eq!(
            result.content_digest(),
            &AppDigest::blake3(b"sensitive result")
        );
        assert_eq!(result.byte_count(), 16);
        assert_eq!(
            result.handling_labels().labels().classification,
            AppDataClassification::Secret
        );
        assert_eq!(
            result.handling_labels().labels().model_processing,
            AppModelProcessing::None
        );
        assert!(result.checkpoint().matches_result(&result));
    }

    #[test]
    fn external_dispatch_permit_expires_with_its_endpoint_attestation() {
        let authority = authority(&["destination:mail"], &["destination:mail"]);
        let permit = authorize(&authority, external_target("destination:mail")).unwrap();
        let bytes = br#"{"query":"approved"}"#;

        assert!(permit.matches_disclosure_bytes_at(bytes, &(now() + Duration::minutes(4))));
        assert!(!permit.matches_disclosure_bytes_at(bytes, &(now() + Duration::minutes(5))));
        assert!(!permit.matches_disclosure_bytes_at(bytes, &(now() + Duration::hours(1))));
    }

    #[test]
    fn result_ceiling_and_checkpoint_tampering_fail_closed() {
        let local_authority = authority(&[], &[]);
        let policy = ResolvedAppDataHandlingPolicy::from_resolved_authority(&local_authority);
        assert!(matches!(
            authorize_app_tool_disclosure(
                reference("execution:1"),
                reference("invocation:1"),
                &local_authority,
                &labels(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                    "input"
                ),
                &policy,
                local_target(),
                b"12345",
                4,
                now()
            ),
            Err(AppToolDisclosureError::DisclosureBytesExceeded {
                actual: 5,
                limit: 4
            })
        ));

        let mut resource_limited = authority(&[], &[]);
        resource_limited.effective_resources.max_payload_bytes = 4;
        resource_limited.authority_digest = resource_limited.canonical_authority_digest().unwrap();
        let resource_policy =
            ResolvedAppDataHandlingPolicy::from_resolved_authority(&resource_limited);
        let transient_input_permit = authorize_app_tool_disclosure(
            reference("execution:1"),
            reference("invocation:1"),
            &resource_limited,
            &labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "input",
            ),
            &resource_policy,
            local_target(),
            b"12345",
            1_024,
            now(),
        )
        .expect("transient input uses its reviewed transport ceiling, not storage.max_bytes");
        assert_eq!(transient_input_permit.disclosure_byte_ceiling(), 1_024);
        let resource_permit = authorize_app_tool_disclosure(
            reference("execution:1"),
            reference("invocation:2"),
            &resource_limited,
            &labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "input",
            ),
            &resource_policy,
            local_target(),
            b"ok",
            1_024,
            now(),
        )
        .unwrap();
        assert!(matches!(
            label_app_tool_result_bytes(
                resource_permit,
                b"12345",
                &labels(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                    "result"
                ),
                1_024,
                now()
            ),
            Err(AppToolDisclosureError::ResultBytesExceeded {
                actual: 5,
                limit: 4
            })
        ));

        let permit = authorize(&local_authority, local_target()).unwrap();
        assert!(matches!(
            label_app_tool_result_bytes(
                permit,
                b"12345",
                &labels(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                    "result"
                ),
                4,
                now()
            ),
            Err(AppToolDisclosureError::ResultBytesExceeded {
                actual: 5,
                limit: 4
            })
        ));

        let result = label_app_tool_result_bytes(
            authorize(&local_authority, local_target()).unwrap(),
            b"ok",
            &labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "result",
            ),
            4,
            now(),
        )
        .unwrap();
        let mut checkpoint = result.checkpoint();
        checkpoint.content_digest = AppDigest::blake3(b"tampered");
        assert!(!checkpoint.matches_result(&result));
    }

    #[test]
    fn hidden_consumers_get_explicit_full_metadata_or_denied_decisions() {
        let authority = authority(&[], &[]);
        let policy = ResolvedAppDataHandlingPolicy::from_resolved_authority(&authority);
        let result = label_app_tool_result_bytes(
            authorize(&authority, local_target()).unwrap(),
            b"result",
            &labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "result",
            ),
            64,
            now(),
        )
        .unwrap();

        let analytics = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::Analytics,
            true,
            AppDataClassification::Secret,
            AppPayloadCapability::ProtectedLabeledRetention,
            revision(1),
        );
        let analytics_admission =
            authorize_app_hidden_consumer(&result, &policy, &analytics, None, now()).unwrap();
        assert_eq!(
            analytics_admission.decision(),
            AppConsumerDecision::MetadataOnly
        );

        let reflection = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::Reflection,
            true,
            AppDataClassification::Secret,
            AppPayloadCapability::EphemeralLabeledContent,
            revision(1),
        );
        assert!(matches!(
            authorize_app_hidden_consumer(&result, &policy, &reflection, None, now()),
            Err(AppToolDisclosureError::HiddenConsumerDenied {
                reason: AppConsumerDecisionReason::ProcessingTargetMissing,
                ..
            })
        ));

        let local_endpoint = AttestedAppEndpoint::from_trusted_resolver(
            reference("endpoint:local-model"),
            AppEndpointClass::LoopbackManaged,
            true,
            revision(1),
            AppDigest::blake3(b"local-model"),
            now(),
            now() + Duration::minutes(5),
        )
        .unwrap();
        let model_ref = reference("model:local");
        let reflection_admission = authorize_app_hidden_consumer(
            &result,
            &policy,
            &reflection,
            Some(AppProcessingTarget::Model {
                endpoint: &local_endpoint,
                model_ref: &model_ref,
            }),
            now(),
        )
        .unwrap();
        assert_eq!(
            reflection_admission.decision(),
            AppConsumerDecision::FullContent
        );

        let continuation = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::WorkflowContinuation,
            true,
            AppDataClassification::Secret,
            AppPayloadCapability::ProtectedLabeledRetention,
            revision(1),
        );
        assert_eq!(
            authorize_app_hidden_consumer(&result, &policy, &continuation, None, now())
                .unwrap()
                .decision(),
            AppConsumerDecision::FullContent
        );
        let unprotected_continuation = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::WorkflowContinuation,
            true,
            AppDataClassification::Secret,
            AppPayloadCapability::EphemeralLabeledContent,
            revision(1),
        );
        assert_eq!(
            authorize_app_hidden_consumer(
                &result,
                &policy,
                &unprotected_continuation,
                None,
                now(),
            )
            .unwrap()
            .decision(),
            AppConsumerDecision::MetadataOnly
        );
    }

    #[test]
    fn labeled_result_debug_is_content_free() {
        let authority = authority(&[], &[]);
        let value = serde_json::json!({"private_phrase": "never-print-this-value"});
        let bytes = crate::magician_v2::json_traversal::canonical_json_bytes(&value).unwrap();
        let label = label_app_tool_result_bytes(
            authorize(&authority, local_target()).unwrap(),
            &bytes,
            &labels(
                AppDataClassification::Secret,
                AppModelProcessing::LocalOnly,
                "debug-result",
            ),
            1_024,
            now(),
        )
        .unwrap();
        let record = AppLabeledToolResultRecord::from_server_label(&label, value).unwrap();
        let rendered = format!("{record:?}");
        assert!(!rendered.contains("never-print-this-value"));
        assert!(!rendered.contains("private_phrase"));
        assert!(rendered.contains("content_digest"));
        assert!(rendered.contains("byte_count"));
    }

    #[test]
    fn workflow_continuation_requires_exact_bytes_labels_and_current_authority() {
        let authority = authority(&[], &[]);
        let handling = labels(
            AppDataClassification::Secret,
            AppModelProcessing::LocalOnly,
            "pause",
        );
        let execution_ref = reference("execution:pause-1");
        let secret = b"exact protected pause bytes: never-log-this";
        let checkpoint = authorize_app_workflow_continuation(
            execution_ref.clone(),
            &authority,
            &handling,
            secret,
            now(),
        )
        .unwrap();

        reauthorize_app_workflow_continuation(
            &checkpoint,
            &execution_ref,
            &authority,
            &handling,
            secret,
            now(),
        )
        .unwrap();
        assert!(reauthorize_app_workflow_continuation(
            &checkpoint,
            &execution_ref,
            &authority,
            &handling,
            b"exact protected pause bytes: changed",
            now(),
        )
        .is_err());
        assert!(reauthorize_app_workflow_continuation(
            &checkpoint,
            &execution_ref,
            &authority,
            &labels(
                AppDataClassification::Secret,
                AppModelProcessing::LocalOnly,
                "different-policy",
            ),
            secret,
            now(),
        )
        .is_err());

        let mut changed_authority = authority.clone();
        changed_authority.grant_revision = revision(2);
        changed_authority.authority_digest =
            changed_authority.canonical_authority_digest().unwrap();
        assert!(reauthorize_app_workflow_continuation(
            &checkpoint,
            &execution_ref,
            &changed_authority,
            &handling,
            secret,
            now(),
        )
        .is_err());

        let rendered = format!("{checkpoint:?}");
        assert!(!rendered.contains("never-log-this"));
        assert!(rendered.contains("content_digest"));
    }

    #[test]
    fn result_label_and_dispatch_permit_are_not_deserializable_contracts() {
        fn assert_serialize<T: Serialize>() {}
        assert_serialize::<AppToolResultLabel>();
        assert_serialize::<AppToolResultCheckpoint>();

        let source = include_str!("tool_disclosure.rs");
        let permit_definition = source
            .split("pub struct AppToolDisclosurePermit")
            .nth(1)
            .and_then(|tail| tail.split("impl std::fmt::Debug").next())
            .unwrap();
        assert!(!permit_definition.contains("Deserialize"));
        let label_definition = source
            .split("pub struct AppToolResultLabel")
            .nth(1)
            .and_then(|tail| tail.split("impl AppToolResultLabel").next())
            .unwrap();
        assert!(!label_definition.contains("Deserialize"));
    }
}
