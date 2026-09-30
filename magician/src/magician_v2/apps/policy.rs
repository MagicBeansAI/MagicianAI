//! Provider-free app data-handling policy primitives.
//!
//! The types in this module are provider-free policy authority used by the
//! load-bearing model, tool-result, continuation and app-memory adapters. They
//! do not discover providers, dispatch models/tools, capture payloads or
//! persist continuations themselves. Every consequential adapter must obtain
//! server-owned endpoint and consumer capabilities, then call these pure
//! decisions immediately before use.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::{
    models::{
        AppContractLimits, AppDataClassification, AppDataEnvelope, AppDigest, AppHandlingLabels,
        AppInstallationId, AppModelProcessing, AppProtocolVersion, AppReference, AppRevision,
        AppScopeBindingRef, AppSourceRef, AppSourceRefKind,
    },
    records::{AppDataHandlingPolicy, AppExternalEgress},
};
use crate::magician_v2::json_traversal::discard_json_iteratively;

/// Return the monotone intersection of two already-validated data-handling
/// policies. The result can only raise classification or narrow processing,
/// personal-agent access, memory promotion, and external egress.
///
/// Destination allowlists are capabilities, so an approved egress survives
/// only when the exact destination is present in both inputs. Keeping this
/// primitive here gives derived content, brokered workflows, and entity-store
/// joins one definition of "no broader than either source".
pub fn intersect_app_data_handling_policies(
    left: &AppDataHandlingPolicy,
    right: &AppDataHandlingPolicy,
) -> AppDataHandlingPolicy {
    let right_destinations = right
        .approved_destinations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut approved_destinations = left
        .approved_destinations
        .iter()
        .filter(|destination| right_destinations.contains(*destination))
        .cloned()
        .collect::<Vec<_>>();
    approved_destinations.sort();
    approved_destinations.dedup();

    let mut external_egress = left.external_egress.min(right.external_egress);
    // Destination egress with no destination left is denial; "any public
    // host" stands on its own (it still needs the owner's explicit grant).
    if external_egress == AppExternalEgress::Denied
        || (external_egress == AppExternalEgress::ApprovedDestinations
            && approved_destinations.is_empty())
    {
        external_egress = AppExternalEgress::Denied;
        approved_destinations.clear();
    }

    AppDataHandlingPolicy {
        classification_floor: left.classification_floor.max(right.classification_floor),
        model_processing: left.model_processing.min(right.model_processing),
        personal_agent_access: left.personal_agent_access.min(right.personal_agent_access),
        memory_promotion: left.memory_promotion.min(right.memory_promotion),
        external_egress,
        approved_destinations,
    }
}

/// Additional destination-side policy that participates in a derived-content
/// join. It may only raise classification or narrow model processing.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppHandlingConstraint {
    pub classification_floor: AppDataClassification,
    pub model_processing: AppModelProcessing,
    pub policy_digest: AppDigest,
    pub purpose: AppReference,
    pub audience_ref: AppReference,
}

/// Bounded result of joining one or more canonical app envelopes.
///
/// It is intentionally not deserializable: handling labels on derived content
/// are server results, not claims accepted from a package, model or client.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppJoinedContent {
    protocol_version: AppProtocolVersion,
    scope_binding_ref: AppScopeBindingRef,
    value: Value,
    source_refs: Vec<AppSourceRef>,
    handling_labels: ResolvedAppHandlingLabels,
    content_digest: AppDigest,
}

impl AppJoinedContent {
    pub fn protocol_version(&self) -> AppProtocolVersion {
        self.protocol_version
    }

    pub fn scope_binding_ref(&self) -> &AppScopeBindingRef {
        &self.scope_binding_ref
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn source_refs(&self) -> &[AppSourceRef] {
        &self.source_refs
    }

    pub fn handling_labels(&self) -> &AppHandlingLabels {
        self.handling_labels.labels()
    }

    pub fn resolved_handling_labels(&self) -> &ResolvedAppHandlingLabels {
        &self.handling_labels
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }
}

/// Server-resolved handling labels. This type cannot be deserialized from a
/// package, client, model result or bridge message, so consequential policy
/// decisions cannot accidentally trust the labels carried on a wire envelope.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ResolvedAppHandlingLabels {
    labels: AppHandlingLabels,
}

/// Server-resolved effective data policy. Like resolved labels, this cannot be
/// deserialized from an app or client and must be minted only after current
/// grant/schema/authority intersection.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedAppDataHandlingPolicy {
    policy: AppDataHandlingPolicy,
    authority_digest: AppDigest,
    resolved_at: DateTime<Utc>,
}

impl ResolvedAppDataHandlingPolicy {
    pub fn from_resolved_authority(authority: &super::authority::ResolvedAppAuthority) -> Self {
        Self {
            policy: authority.effective_data_handling_policy.clone(),
            authority_digest: authority.authority_digest.clone(),
            resolved_at: authority.resolved_at.to_owned(),
        }
    }

    /// Mint effective policy from the current store-owned record/schema
    /// snapshot. Package, client and model input cannot call this.
    pub fn from_trusted_store_policy(
        policy: AppDataHandlingPolicy,
        authority_digest: AppDigest,
        resolved_at: DateTime<Utc>,
    ) -> Self {
        Self {
            policy,
            authority_digest,
            resolved_at,
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn from_trusted_policy_for_test(
        policy: AppDataHandlingPolicy,
        resolved_at: DateTime<Utc>,
    ) -> Self {
        Self {
            policy,
            authority_digest: AppDigest::blake3(b"test-authority"),
            resolved_at,
        }
    }

    pub fn policy(&self) -> &AppDataHandlingPolicy {
        &self.policy
    }

    pub fn authority_digest(&self) -> &AppDigest {
        &self.authority_digest
    }

    pub fn resolved_at(&self) -> &DateTime<Utc> {
        &self.resolved_at
    }

    fn ensure_current_at(&self, now: &DateTime<Utc>) -> Result<(), AppPolicyError> {
        if &self.resolved_at != now {
            return Err(AppPolicyError::PolicyEvidenceStale);
        }
        Ok(())
    }
}

impl ResolvedAppHandlingLabels {
    /// Trusted policy resolvers mint this after resolving current schema,
    /// installation, grant and field-level policy state.
    pub fn from_trusted_policy(labels: AppHandlingLabels) -> Self {
        Self { labels }
    }

    pub fn labels(&self) -> &AppHandlingLabels {
        &self.labels
    }
}

/// Immutable, validated view over one canonical app envelope. Borrowing the
/// source prevents it from being mutated between validation and policy use;
/// stored metrics avoid a second traversal during joins.
#[derive(Debug)]
pub struct RevalidatedAppEnvelope<'a> {
    envelope: &'a AppDataEnvelope<Value>,
    handling_labels: ResolvedAppHandlingLabels,
    value_nodes: usize,
    value_bytes: usize,
}

impl<'a> RevalidatedAppEnvelope<'a> {
    pub fn from_trusted_resolution(
        envelope: &'a AppDataEnvelope<Value>,
        handling_labels: ResolvedAppHandlingLabels,
        limits: &AppContractLimits,
    ) -> Result<Self, AppPolicyError> {
        let (value_nodes, value_bytes) = super::models::validate_value_envelope(envelope, limits)
            .map_err(|error| {
            let digest_mismatch = matches!(
                &error,
                super::models::AppContractError::InvalidField { field, .. }
                    if *field == "content_digest"
            );
            if digest_mismatch {
                AppPolicyError::InputContentDigestMismatch
            } else {
                AppPolicyError::InvalidEnvelope(error.to_string())
            }
        })?;
        if &envelope.handling_labels != handling_labels.labels() {
            return Err(AppPolicyError::InputHandlingLabelsMismatch);
        }
        Ok(Self {
            envelope,
            handling_labels,
            value_nodes,
            value_bytes,
        })
    }

    pub fn envelope(&self) -> &AppDataEnvelope<Value> {
        self.envelope
    }

    pub fn handling_labels(&self) -> &ResolvedAppHandlingLabels {
        &self.handling_labels
    }

    pub fn value_nodes(&self) -> usize {
        self.value_nodes
    }

    pub fn value_bytes(&self) -> usize {
        self.value_bytes
    }
}

/// Join canonical app envelopes without permitting any source or destination
/// to lower their effective policy.
pub fn join_app_content(
    inputs: &[RevalidatedAppEnvelope<'_>],
    derived_value: Value,
    constraint: &AppHandlingConstraint,
    limits: &AppContractLimits,
) -> Result<AppJoinedContent, AppPolicyError> {
    if let Err(error) = super::models::validate_json_value(&derived_value, limits) {
        discard_json_iteratively(derived_value);
        return Err(AppPolicyError::InvalidDerivedValue(error.to_string()));
    }
    let first = inputs.first().ok_or(AppPolicyError::UnlabeledContent)?;
    if inputs.len() > limits.max_collection_items() {
        return Err(AppPolicyError::InputLimitExceeded {
            limit: limits.max_collection_items(),
        });
    }
    let mut aggregate_source_nodes = 0usize;
    let mut aggregate_source_bytes = 0usize;
    for input in inputs {
        aggregate_source_nodes = aggregate_source_nodes.saturating_add(input.value_nodes);
        aggregate_source_bytes = aggregate_source_bytes.saturating_add(input.value_bytes);
        if aggregate_source_nodes > limits.max_value_nodes()
            || aggregate_source_bytes > limits.max_value_bytes()
        {
            return Err(AppPolicyError::SourcePayloadLimitExceeded {
                max_nodes: limits.max_value_nodes(),
                max_bytes: limits.max_value_bytes(),
            });
        }
        if input.envelope.scope_binding_ref != first.envelope.scope_binding_ref {
            return Err(AppPolicyError::CrossScopeJoin);
        }
    }
    let classification = inputs
        .iter()
        .fold(constraint.classification_floor, |classification, input| {
            classification.max(input.handling_labels.labels.classification)
        });
    let model_processing =
        inputs
            .iter()
            .fold(constraint.model_processing, |model_processing, input| {
                model_processing.min(input.handling_labels.labels.model_processing)
            });

    let source_refs = merge_source_refs(inputs, limits.max_collection_items())?;
    let content_digest = AppDigest::blake3_canonical_json(&derived_value)
        .map_err(|error| AppPolicyError::DigestEncoding(error.to_string()))?;

    let mut input_policy_digests = inputs
        .iter()
        .map(|input| input.handling_labels.labels.policy_digest.clone())
        .collect::<Vec<_>>();
    input_policy_digests.sort();
    input_policy_digests.dedup();
    #[derive(Serialize)]
    struct PolicyDigestMaterial<'a> {
        protocol_version: AppProtocolVersion,
        input_policy_digests: &'a [AppDigest],
        constraint_policy_digest: &'a AppDigest,
        classification: AppDataClassification,
        model_processing: AppModelProcessing,
        purpose: &'a AppReference,
        audience_ref: &'a AppReference,
    }
    let policy_material = PolicyDigestMaterial {
        protocol_version: AppProtocolVersion::V1,
        input_policy_digests: &input_policy_digests,
        constraint_policy_digest: &constraint.policy_digest,
        classification,
        model_processing,
        purpose: &constraint.purpose,
        audience_ref: &constraint.audience_ref,
    };
    let policy_digest = digest_serializable(&policy_material)?;

    let mut input_origins = inputs
        .iter()
        .map(|input| InputOriginMaterial {
            installation_id: input.envelope.installation_id.clone(),
            package_revision_ref: input.envelope.package_revision_ref.clone(),
            schema_revision: input.envelope.schema_revision,
            grant_revision: input.envelope.grant_revision,
            content_digest: input.envelope.content_digest.clone(),
            provenance_digest: input.handling_labels.labels.provenance_digest.clone(),
        })
        .collect::<Vec<_>>();
    input_origins.sort();
    input_origins.dedup();
    #[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
    struct InputOriginMaterial {
        installation_id: AppInstallationId,
        package_revision_ref: AppReference,
        schema_revision: AppRevision,
        grant_revision: AppRevision,
        content_digest: AppDigest,
        provenance_digest: AppDigest,
    }
    #[derive(Serialize)]
    struct ProvenanceDigestMaterial<'a> {
        protocol_version: AppProtocolVersion,
        scope_binding_ref: &'a AppScopeBindingRef,
        input_origins: &'a [InputOriginMaterial],
        source_refs: &'a [AppSourceRef],
    }
    let provenance_material = ProvenanceDigestMaterial {
        protocol_version: AppProtocolVersion::V1,
        scope_binding_ref: &first.envelope.scope_binding_ref,
        input_origins: &input_origins,
        source_refs: &source_refs,
    };
    let provenance_digest = digest_serializable(&provenance_material)?;

    Ok(AppJoinedContent {
        protocol_version: AppProtocolVersion::V1,
        scope_binding_ref: first.envelope.scope_binding_ref.clone(),
        value: derived_value,
        source_refs,
        handling_labels: ResolvedAppHandlingLabels::from_trusted_policy(AppHandlingLabels {
            classification,
            model_processing,
            policy_digest,
            provenance_digest,
        }),
        content_digest,
    })
}

fn merge_source_refs(
    inputs: &[RevalidatedAppEnvelope<'_>],
    max_source_refs: usize,
) -> Result<Vec<AppSourceRef>, AppPolicyError> {
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct SourceIdentity {
        kind: u8,
        reference: String,
        revision: Option<u64>,
    }
    struct SourceAccumulator {
        kind: AppSourceRefKind,
        reference: AppReference,
        revision: Option<AppRevision>,
        fields: BTreeSet<super::models::AppFieldPath>,
    }

    let mut joined = BTreeMap::<SourceIdentity, SourceAccumulator>::new();
    for source in inputs.iter().flat_map(|input| &input.envelope.source_refs) {
        let identity = SourceIdentity {
            kind: source_kind_order(source.kind),
            reference: source.reference.as_str().to_owned(),
            revision: source.revision.map(AppRevision::get),
        };
        let entry = joined.entry(identity).or_insert_with(|| SourceAccumulator {
            kind: source.kind,
            reference: source.reference.clone(),
            revision: source.revision,
            fields: BTreeSet::new(),
        });
        entry.fields.extend(source.fields.iter().cloned());
        if entry.fields.len() > max_source_refs {
            return Err(AppPolicyError::ProvenanceLimitExceeded {
                limit: max_source_refs,
            });
        }
    }
    if joined.len() > max_source_refs {
        return Err(AppPolicyError::ProvenanceLimitExceeded {
            limit: max_source_refs,
        });
    }
    let total_fields = joined.values().fold(0usize, |total, source| {
        total.saturating_add(source.fields.len())
    });
    if total_fields > max_source_refs {
        return Err(AppPolicyError::ProvenanceLimitExceeded {
            limit: max_source_refs,
        });
    }
    Ok(joined
        .into_values()
        .map(|source| AppSourceRef {
            kind: source.kind,
            reference: source.reference,
            revision: source.revision,
            fields: source.fields.into_iter().collect(),
        })
        .collect())
}

const fn source_kind_order(kind: AppSourceRefKind) -> u8 {
    match kind {
        AppSourceRefKind::EntityRecord => 0,
        AppSourceRefKind::EntityField => 1,
        AppSourceRefKind::Artifact => 2,
        AppSourceRefKind::ExternalReceipt => 3,
        AppSourceRefKind::MutationReceipt => 4,
    }
}

fn digest_serializable(value: &impl Serialize) -> Result<AppDigest, AppPolicyError> {
    serde_json::to_vec(value)
        .map(|bytes| AppDigest::blake3(&bytes))
        .map_err(|error| AppPolicyError::DigestEncoding(error.to_string()))
}

/// Server-attested endpoint class. `LocalOnly` eligibility is a separate bit:
/// a provider/model name and even a self-hosted address are insufficient.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppEndpointClass {
    LoopbackManaged,
    TrustedSelfHosted,
    External,
}

/// Endpoint evidence minted by a trusted resolver. It cannot be deserialized
/// from a package, model result, bridge message or API body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttestedAppEndpoint {
    endpoint_ref: AppReference,
    class: AppEndpointClass,
    local_processing_eligible: bool,
    trust_revision: AppRevision,
    configuration_digest: AppDigest,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AttestedAppEndpoint {
    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_resolver(
        endpoint_ref: AppReference,
        class: AppEndpointClass,
        local_processing_eligible: bool,
        trust_revision: AppRevision,
        configuration_digest: AppDigest,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppPolicyError> {
        if expires_at <= issued_at {
            return Err(AppPolicyError::InvalidEndpointAttestation);
        }
        if class == AppEndpointClass::External && local_processing_eligible {
            return Err(AppPolicyError::ExternalEndpointClaimedLocal);
        }
        Ok(Self {
            endpoint_ref,
            class,
            local_processing_eligible,
            trust_revision,
            configuration_digest,
            issued_at,
            expires_at,
        })
    }

    pub fn endpoint_ref(&self) -> &AppReference {
        &self.endpoint_ref
    }

    pub fn class(&self) -> AppEndpointClass {
        self.class
    }

    pub fn local_processing_eligible(&self) -> bool {
        self.local_processing_eligible
    }

    pub fn trust_revision(&self) -> AppRevision {
        self.trust_revision
    }

    pub fn configuration_digest(&self) -> &AppDigest {
        &self.configuration_digest
    }

    pub fn is_live_at(&self, now: &DateTime<Utc>) -> bool {
        now >= &self.issued_at && now < &self.expires_at
    }
}

/// Consequential processing destination. There is intentionally no provider
/// name field: locality is supplied only by [`AttestedAppEndpoint`].
#[derive(Debug, Clone, Copy)]
pub enum AppProcessingTarget<'a> {
    Deterministic,
    Model {
        endpoint: &'a AttestedAppEndpoint,
        model_ref: &'a AppReference,
    },
    ExternalTool {
        endpoint: &'a AttestedAppEndpoint,
        destination: &'a AppReference,
    },
    /// An app OS-jail tool call under the owner's explicit "any public host"
    /// grant (tool disclosure checks the resolved authority). The effective
    /// data-handling policy must itself allow any public host.
    ExternalAnyPublicHost { endpoint: &'a AttestedAppEndpoint },
}

/// Revalidate handling and egress at the final consumer/dispatch boundary.
pub fn authorize_processing_target(
    labels: &ResolvedAppHandlingLabels,
    policy: &ResolvedAppDataHandlingPolicy,
    target: AppProcessingTarget<'_>,
    now: &DateTime<Utc>,
) -> Result<(), AppPolicyError> {
    policy.ensure_current_at(now)?;
    match target {
        AppProcessingTarget::Deterministic => Ok(()),
        AppProcessingTarget::Model { endpoint, .. } => {
            ensure_live_endpoint(endpoint, now)?;
            match labels
                .labels
                .model_processing
                .min(policy.policy.model_processing)
            {
                AppModelProcessing::None => Err(AppPolicyError::ModelProcessingDenied),
                AppModelProcessing::LocalOnly if !endpoint.local_processing_eligible => {
                    Err(AppPolicyError::LocalProcessingRequired)
                },
                AppModelProcessing::LocalOnly | AppModelProcessing::RemoteAllowed => Ok(()),
            }
        },
        AppProcessingTarget::ExternalTool {
            endpoint,
            destination,
        } => {
            ensure_live_endpoint(endpoint, now)?;
            if policy.policy.external_egress < AppExternalEgress::ApprovedDestinations
                || !policy
                    .policy
                    .approved_destinations
                    .iter()
                    .any(|approved| approved == destination)
            {
                return Err(AppPolicyError::ExternalDestinationDenied);
            }
            Ok(())
        },
        AppProcessingTarget::ExternalAnyPublicHost { endpoint } => {
            ensure_live_endpoint(endpoint, now)?;
            if policy.policy.external_egress < AppExternalEgress::AnyPublicHost {
                return Err(AppPolicyError::ExternalDestinationDenied);
            }
            Ok(())
        },
    }
}

fn ensure_live_endpoint(
    endpoint: &AttestedAppEndpoint,
    now: &DateTime<Utc>,
) -> Result<(), AppPolicyError> {
    if endpoint.is_live_at(now) {
        Ok(())
    } else {
        Err(AppPolicyError::EndpointAttestationExpired)
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppHiddenConsumer {
    LocalPreparation,
    Embedding,
    PromptAssembly,
    PromptDebugDump,
    RawTraceCapture,
    /// Canonical protected workflow sidecar and pause/live continuation.
    /// This is intentionally distinct from diagnostics/trace capture.
    WorkflowContinuation,
    EvalArtifact,
    Reflection,
    Synthesis,
    Analytics,
    CrashDiagnostics,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppPayloadCapability {
    MetadataOnly,
    EphemeralLabeledContent,
    ProtectedLabeledRetention,
}

/// Trusted capability description for one hidden consumer. Private fields and
/// lack of `Deserialize` prevent packages from granting this capability.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConsumerCapability {
    consumer: AppHiddenConsumer,
    accepts_labels: bool,
    max_classification: AppDataClassification,
    payload_capability: AppPayloadCapability,
    capability_revision: AppRevision,
}

impl AppConsumerCapability {
    pub fn from_trusted_registry(
        consumer: AppHiddenConsumer,
        accepts_labels: bool,
        max_classification: AppDataClassification,
        payload_capability: AppPayloadCapability,
        capability_revision: AppRevision,
    ) -> Self {
        Self {
            consumer,
            accepts_labels,
            max_classification,
            payload_capability,
            capability_revision,
        }
    }

    pub fn consumer(&self) -> AppHiddenConsumer {
        self.consumer
    }

    pub fn capability_revision(&self) -> AppRevision {
        self.capability_revision
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppConsumerDecision {
    FullContent,
    MetadataOnly,
    Denied,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppConsumerDecisionReason {
    Eligible,
    UnlabeledContent,
    LabelsUnsupported,
    ClassificationUnsupported,
    MetadataConsumer,
    ProtectedRetentionRequired,
    ProcessingTargetMissing,
    ProcessingTargetDenied,
    PolicyEvidenceStale,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConsumerPolicyDecision {
    pub decision: AppConsumerDecision,
    pub reason: AppConsumerDecisionReason,
}

/// Decide whether a hidden consumer receives bytes, metadata, or nothing.
/// Consumers that execute models must also present the exact processing target
/// that will receive the bytes.
pub fn decide_hidden_consumer(
    labels: Option<&ResolvedAppHandlingLabels>,
    policy: &ResolvedAppDataHandlingPolicy,
    capability: &AppConsumerCapability,
    target: Option<AppProcessingTarget<'_>>,
    now: &DateTime<Utc>,
) -> AppConsumerPolicyDecision {
    if policy.ensure_current_at(now).is_err() {
        return consumer_decision(
            AppConsumerDecision::Denied,
            AppConsumerDecisionReason::PolicyEvidenceStale,
        );
    }
    let Some(labels) = labels else {
        return consumer_decision(
            AppConsumerDecision::Denied,
            AppConsumerDecisionReason::UnlabeledContent,
        );
    };
    if !capability.accepts_labels {
        return consumer_decision(
            AppConsumerDecision::MetadataOnly,
            AppConsumerDecisionReason::LabelsUnsupported,
        );
    }
    if labels
        .labels
        .classification
        .max(policy.policy.classification_floor)
        > capability.max_classification
    {
        return consumer_decision(
            AppConsumerDecision::Denied,
            AppConsumerDecisionReason::ClassificationUnsupported,
        );
    }
    if matches!(
        capability.consumer,
        AppHiddenConsumer::Analytics | AppHiddenConsumer::CrashDiagnostics
    ) || capability.payload_capability == AppPayloadCapability::MetadataOnly
    {
        return consumer_decision(
            AppConsumerDecision::MetadataOnly,
            AppConsumerDecisionReason::MetadataConsumer,
        );
    }
    if matches!(
        capability.consumer,
        AppHiddenConsumer::PromptDebugDump
            | AppHiddenConsumer::RawTraceCapture
            | AppHiddenConsumer::WorkflowContinuation
            | AppHiddenConsumer::EvalArtifact
    ) && capability.payload_capability != AppPayloadCapability::ProtectedLabeledRetention
    {
        return consumer_decision(
            AppConsumerDecision::MetadataOnly,
            AppConsumerDecisionReason::ProtectedRetentionRequired,
        );
    }

    let needs_processing_target = matches!(
        capability.consumer,
        AppHiddenConsumer::LocalPreparation
            | AppHiddenConsumer::Embedding
            | AppHiddenConsumer::PromptAssembly
            | AppHiddenConsumer::Reflection
            | AppHiddenConsumer::Synthesis
    );
    if needs_processing_target {
        let Some(target) = target else {
            return consumer_decision(
                AppConsumerDecision::Denied,
                AppConsumerDecisionReason::ProcessingTargetMissing,
            );
        };
        if authorize_processing_target(labels, policy, target, now).is_err() {
            return consumer_decision(
                AppConsumerDecision::Denied,
                AppConsumerDecisionReason::ProcessingTargetDenied,
            );
        }
    }
    consumer_decision(
        AppConsumerDecision::FullContent,
        AppConsumerDecisionReason::Eligible,
    )
}

const fn consumer_decision(
    decision: AppConsumerDecision,
    reason: AppConsumerDecisionReason,
) -> AppConsumerPolicyDecision {
    AppConsumerPolicyDecision { decision, reason }
}

/// Exact identity of reusable provider-side history. A continuation may be
/// reused only when this complete value remains unchanged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppContinuationPartition {
    protocol_version: AppProtocolVersion,
    scope_binding_ref: AppScopeBindingRef,
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    grant_revision: AppRevision,
    schema_revision: AppRevision,
    endpoint_ref: AppReference,
    endpoint_trust_revision: AppRevision,
    endpoint_configuration_digest: AppDigest,
    model_ref: AppReference,
    disclosure_policy_digest: AppDigest,
    authority_digest: AppDigest,
    handling_digest: AppDigest,
}

impl AppContinuationPartition {
    #[allow(clippy::too_many_arguments)]
    pub fn from_resolved_context(
        scope_binding_ref: AppScopeBindingRef,
        installation_id: AppInstallationId,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        endpoint: &AttestedAppEndpoint,
        model_ref: AppReference,
        disclosure_policy_digest: AppDigest,
        authority_digest: AppDigest,
        handling_digest: AppDigest,
    ) -> Self {
        Self {
            protocol_version: AppProtocolVersion::V1,
            scope_binding_ref,
            installation_id,
            package_revision_ref,
            grant_revision,
            schema_revision,
            endpoint_ref: endpoint.endpoint_ref.clone(),
            endpoint_trust_revision: endpoint.trust_revision,
            endpoint_configuration_digest: endpoint.configuration_digest.clone(),
            model_ref,
            disclosure_policy_digest,
            authority_digest,
            handling_digest,
        }
    }

    pub fn partition_id(&self) -> Result<AppDigest, AppPolicyError> {
        digest_serializable(self)
    }

    /// Identity comparison only. A load-bearing adapter must also revalidate
    /// current authority and a live endpoint attestation before actual reuse.
    pub fn has_same_identity_as(&self, previous: &Self) -> bool {
        self == previous
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppPolicyError {
    #[error("derived app content has no labeled source")]
    UnlabeledContent,
    #[error("app content join exceeds the {limit} input ceiling")]
    InputLimitExceeded { limit: usize },
    #[error("joined source payloads exceed {max_nodes} nodes or {max_bytes} bytes")]
    SourcePayloadLimitExceeded { max_nodes: usize, max_bytes: usize },
    #[error("app content join crosses authenticated scope bindings")]
    CrossScopeJoin,
    #[error("invalid source app envelope: {0}")]
    InvalidEnvelope(String),
    #[error("source app envelope content digest does not match its canonical value")]
    InputContentDigestMismatch,
    #[error("source app envelope handling labels do not match trusted policy resolution")]
    InputHandlingLabelsMismatch,
    #[error("invalid derived app value: {0}")]
    InvalidDerivedValue(String),
    #[error("app provenance exceeds the {limit} item ceiling")]
    ProvenanceLimitExceeded { limit: usize },
    #[error("failed to encode canonical policy identity: {0}")]
    DigestEncoding(String),
    #[error("endpoint attestation has an invalid validity window")]
    InvalidEndpointAttestation,
    #[error("an external endpoint cannot be attested as local-processing eligible")]
    ExternalEndpointClaimedLocal,
    #[error("endpoint attestation is not live")]
    EndpointAttestationExpired,
    #[error("resolved app data-handling policy is not current at the consumer boundary")]
    PolicyEvidenceStale,
    #[error("model processing is denied for this content")]
    ModelProcessingDenied,
    #[error("content requires an explicitly local-processing-eligible endpoint")]
    LocalProcessingRequired,
    #[error("external destination is not approved independently of model processing")]
    ExternalDestinationDenied,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::{
        apps::{
            models::{AppDataSource, AppFieldPath, AppSourceRef, AppSourceRefKind},
            records::{AppMemoryPromotion, AppPersonalAgentAccess},
        },
        json_traversal::canonical_json_bytes,
    };

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("valid reference")
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).expect("positive revision")
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0)
            .single()
            .expect("valid time")
    }

    fn labels(
        classification: AppDataClassification,
        processing: AppModelProcessing,
        seed: &str,
    ) -> AppHandlingLabels {
        AppHandlingLabels {
            classification,
            model_processing: processing,
            policy_digest: AppDigest::blake3(format!("policy-{seed}").as_bytes()),
            provenance_digest: AppDigest::blake3(format!("provenance-{seed}").as_bytes()),
        }
    }

    fn resolved_labels(
        classification: AppDataClassification,
        processing: AppModelProcessing,
        seed: &str,
    ) -> ResolvedAppHandlingLabels {
        ResolvedAppHandlingLabels::from_trusted_policy(labels(classification, processing, seed))
    }

    fn join_trusted(
        inputs: &[AppDataEnvelope<Value>],
        derived_value: Value,
        constraint: &AppHandlingConstraint,
        limits: &AppContractLimits,
    ) -> Result<AppJoinedContent, AppPolicyError> {
        let inputs = inputs
            .iter()
            .map(|envelope| {
                RevalidatedAppEnvelope::from_trusted_resolution(
                    envelope,
                    ResolvedAppHandlingLabels::from_trusted_policy(
                        envelope.handling_labels.clone(),
                    ),
                    limits,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        join_app_content(&inputs, derived_value, constraint, limits)
    }

    fn envelope(
        scope: &str,
        source_ref: &str,
        classification: AppDataClassification,
        processing: AppModelProcessing,
        seed: &str,
    ) -> AppDataEnvelope<Value> {
        let value = json!({"source": seed});
        let content_digest =
            AppDigest::blake3(&canonical_json_bytes(&value).expect("canonical value"));
        AppDataEnvelope {
            protocol_version: AppProtocolVersion::V1,
            source: AppDataSource::AppStore,
            scope_binding_ref: AppScopeBindingRef::parse(scope).expect("valid scope"),
            installation_id: AppInstallationId::parse("install_policy_test")
                .expect("valid installation"),
            package_revision_ref: reference("package:policy-test:1.0.0"),
            schema_revision: revision(2),
            grant_revision: revision(3),
            value_schema_ref: reference("schema:policy-test:record"),
            value,
            source_refs: vec![AppSourceRef {
                kind: AppSourceRefKind::EntityField,
                reference: reference(source_ref),
                revision: Some(revision(7)),
                fields: vec![AppFieldPath::parse("private.note").expect("valid field")],
            }],
            handling_labels: labels(classification, processing, seed),
            content_digest,
            produced_at: now(),
            expires_at: None,
        }
    }

    fn constraint() -> AppHandlingConstraint {
        AppHandlingConstraint {
            classification_floor: AppDataClassification::Ordinary,
            model_processing: AppModelProcessing::RemoteAllowed,
            policy_digest: AppDigest::blake3(b"destination-policy"),
            purpose: reference("purpose:compose"),
            audience_ref: reference("audience:owner"),
        }
    }

    fn policy_record(processing: AppModelProcessing) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Public,
            model_processing: processing,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn policy(processing: AppModelProcessing) -> ResolvedAppDataHandlingPolicy {
        ResolvedAppDataHandlingPolicy::from_trusted_policy_for_test(
            policy_record(processing),
            now(),
        )
    }

    fn endpoint(class: AppEndpointClass, local_processing_eligible: bool) -> AttestedAppEndpoint {
        AttestedAppEndpoint::from_trusted_resolver(
            reference("endpoint:primary"),
            class,
            local_processing_eligible,
            revision(9),
            AppDigest::blake3(b"endpoint-configuration"),
            now() - chrono::Duration::minutes(1),
            now() + chrono::Duration::minutes(1),
        )
        .expect("valid endpoint attestation")
    }

    #[test]
    fn trusted_policy_evidence_cannot_be_deserialized_from_wire_values() {
        static_assertions::assert_not_impl_any!(
            ResolvedAppHandlingLabels: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            ResolvedAppDataHandlingPolicy: serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            RevalidatedAppEnvelope<'static>: serde::de::DeserializeOwned, Clone
        );
    }

    #[test]
    fn joins_classification_up_and_model_processing_down() {
        let inputs = vec![
            envelope(
                "scope_owner_default",
                "record:first",
                AppDataClassification::Personal,
                AppModelProcessing::RemoteAllowed,
                "first",
            ),
            envelope(
                "scope_owner_default",
                "record:second",
                AppDataClassification::Secret,
                AppModelProcessing::LocalOnly,
                "second",
            ),
        ];
        let joined = join_trusted(
            &inputs,
            json!({"summary": "derived"}),
            &constraint(),
            &AppContractLimits::default(),
        )
        .expect("join succeeds");
        assert_eq!(
            joined.handling_labels().classification,
            AppDataClassification::Secret
        );
        assert_eq!(
            joined.handling_labels().model_processing,
            AppModelProcessing::LocalOnly
        );
        assert_eq!(joined.source_refs().len(), 2);
    }

    #[test]
    fn join_is_order_independent_and_rejects_unlabeled_or_cross_scope_content() {
        let first = envelope(
            "scope_owner_default",
            "record:first",
            AppDataClassification::Personal,
            AppModelProcessing::RemoteAllowed,
            "first",
        );
        let second = envelope(
            "scope_owner_default",
            "record:second",
            AppDataClassification::Sensitive,
            AppModelProcessing::LocalOnly,
            "second",
        );
        let left = join_trusted(
            &[first.clone(), second.clone()],
            json!({"summary": "derived"}),
            &constraint(),
            &AppContractLimits::default(),
        )
        .expect("first order");
        let right = join_trusted(
            &[second, first],
            json!({"summary": "derived"}),
            &constraint(),
            &AppContractLimits::default(),
        )
        .expect("second order");
        assert_eq!(left, right);
        assert_eq!(
            join_trusted(
                &[],
                json!(null),
                &constraint(),
                &AppContractLimits::default()
            ),
            Err(AppPolicyError::UnlabeledContent)
        );

        let cross_scope = envelope(
            "scope_other_default",
            "record:other",
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            "other",
        );
        assert_eq!(
            join_trusted(
                &[
                    envelope(
                        "scope_owner_default",
                        "record:first",
                        AppDataClassification::Personal,
                        AppModelProcessing::LocalOnly,
                        "first",
                    ),
                    cross_scope,
                ],
                json!({}),
                &constraint(),
                &AppContractLimits::default(),
            ),
            Err(AppPolicyError::CrossScopeJoin)
        );

        let mut forged_digest = envelope(
            "scope_owner_default",
            "record:forged",
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            "forged",
        );
        forged_digest.content_digest = AppDigest::blake3(b"different-content");
        assert_eq!(
            join_trusted(
                &[forged_digest],
                json!({}),
                &constraint(),
                &AppContractLimits::default(),
            ),
            Err(AppPolicyError::InputContentDigestMismatch)
        );

        let mut forged_labels = envelope(
            "scope_owner_default",
            "record:forged-labels",
            AppDataClassification::Sensitive,
            AppModelProcessing::LocalOnly,
            "forged-labels",
        );
        let trusted =
            ResolvedAppHandlingLabels::from_trusted_policy(forged_labels.handling_labels.clone());
        forged_labels.handling_labels.classification = AppDataClassification::Public;
        assert!(matches!(
            RevalidatedAppEnvelope::from_trusted_resolution(
                &forged_labels,
                trusted,
                &AppContractLimits::default(),
            ),
            Err(AppPolicyError::InputHandlingLabelsMismatch)
        ));
    }

    #[test]
    fn provenance_limit_and_derived_value_limits_fail_closed() {
        let inputs = vec![
            envelope(
                "scope_owner_default",
                "record:first",
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "first",
            ),
            envelope(
                "scope_owner_default",
                "record:second",
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "second",
            ),
        ];
        let limits = AppContractLimits::default().with_max_collection_items_for_test(1);
        assert_eq!(
            join_trusted(&inputs, json!({}), &constraint(), &limits),
            Err(AppPolicyError::InputLimitExceeded { limit: 1 })
        );

        let mut provenance_inputs = inputs.clone();
        provenance_inputs[0].source_refs.push(AppSourceRef {
            kind: AppSourceRefKind::Artifact,
            reference: reference("artifact:first"),
            revision: None,
            fields: Vec::new(),
        });
        provenance_inputs[1].source_refs.push(AppSourceRef {
            kind: AppSourceRefKind::Artifact,
            reference: reference("artifact:second"),
            revision: None,
            fields: Vec::new(),
        });
        let limits = AppContractLimits::default().with_max_collection_items_for_test(2);
        assert_eq!(
            join_trusted(&provenance_inputs, json!({}), &constraint(), &limits),
            Err(AppPolicyError::ProvenanceLimitExceeded { limit: 2 })
        );

        let limits = AppContractLimits::default().with_max_value_nodes_for_test(1);
        let mut scalar_input = inputs[0].clone();
        scalar_input.value = json!(null);
        scalar_input.content_digest = AppDigest::blake3(b"null");
        let mut second_scalar = inputs[1].clone();
        second_scalar.value = json!(null);
        second_scalar.content_digest = AppDigest::blake3(b"null");
        assert_eq!(
            join_trusted(
                &[scalar_input.clone(), second_scalar],
                json!(null),
                &constraint(),
                &limits,
            ),
            Err(AppPolicyError::SourcePayloadLimitExceeded {
                max_nodes: 1,
                max_bytes: limits.max_value_bytes(),
            })
        );
        assert!(matches!(
            join_trusted(
                &[scalar_input],
                json!({"nested": {"value": true}}),
                &constraint(),
                &limits
            ),
            Err(AppPolicyError::InvalidDerivedValue(_))
        ));
    }

    #[test]
    fn rejected_deep_derived_value_is_disposed_iteratively_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let mut derived = Value::Null;
                for _ in 0..4_096 {
                    derived = Value::Array(vec![derived]);
                }
                assert!(matches!(
                    join_trusted(
                        &[envelope(
                            "scope_owner_default",
                            "record:deep",
                            AppDataClassification::Personal,
                            AppModelProcessing::LocalOnly,
                            "deep",
                        )],
                        derived,
                        &constraint(),
                        &AppContractLimits::default(),
                    ),
                    Err(AppPolicyError::InvalidDerivedValue(_))
                ));
            })
            .expect("small-stack worker starts")
            .join()
            .expect("deep rejection does not overflow the worker stack");
    }

    #[test]
    fn endpoint_attestation_not_provider_name_controls_locality() {
        let labels = resolved_labels(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            "local",
        );
        let model = reference("model:any-name-including-ollama-is-irrelevant");
        let local = endpoint(AppEndpointClass::TrustedSelfHosted, true);
        assert!(authorize_processing_target(
            &labels,
            &policy(AppModelProcessing::RemoteAllowed),
            AppProcessingTarget::Model {
                endpoint: &local,
                model_ref: &model,
            },
            &now(),
        )
        .is_ok());

        let external = endpoint(AppEndpointClass::External, false);
        assert_eq!(
            authorize_processing_target(
                &labels,
                &policy(AppModelProcessing::RemoteAllowed),
                AppProcessingTarget::Model {
                    endpoint: &external,
                    model_ref: &model,
                },
                &now(),
            ),
            Err(AppPolicyError::LocalProcessingRequired)
        );
        assert!(matches!(
            AttestedAppEndpoint::from_trusted_resolver(
                reference("endpoint:forged-local"),
                AppEndpointClass::External,
                true,
                revision(1),
                AppDigest::blake3(b"configuration"),
                now(),
                now() + chrono::Duration::minutes(1),
            ),
            Err(AppPolicyError::ExternalEndpointClaimedLocal)
        ));
    }

    #[test]
    fn resolved_policy_cannot_cross_a_later_consumer_boundary() {
        let labels = resolved_labels(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            "stale-policy",
        );
        let model = reference("model:local");
        let endpoint = endpoint(AppEndpointClass::TrustedSelfHosted, true);
        let resolved = policy(AppModelProcessing::LocalOnly);
        let later = now() + chrono::Duration::milliseconds(1);
        assert_eq!(
            authorize_processing_target(
                &labels,
                &resolved,
                AppProcessingTarget::Model {
                    endpoint: &endpoint,
                    model_ref: &model,
                },
                &later,
            ),
            Err(AppPolicyError::PolicyEvidenceStale)
        );
    }

    #[test]
    fn model_none_and_external_egress_are_independent_axes() {
        let endpoint = endpoint(AppEndpointClass::External, false);
        let model = reference("model:remote");
        let no_model = resolved_labels(
            AppDataClassification::Ordinary,
            AppModelProcessing::None,
            "none",
        );
        assert_eq!(
            authorize_processing_target(
                &no_model,
                &policy(AppModelProcessing::RemoteAllowed),
                AppProcessingTarget::Model {
                    endpoint: &endpoint,
                    model_ref: &model,
                },
                &now(),
            ),
            Err(AppPolicyError::ModelProcessingDenied)
        );

        let remote = resolved_labels(
            AppDataClassification::Ordinary,
            AppModelProcessing::RemoteAllowed,
            "remote",
        );
        let destination = reference("destination:mail");
        assert_eq!(
            authorize_processing_target(
                &remote,
                &policy(AppModelProcessing::RemoteAllowed),
                AppProcessingTarget::ExternalTool {
                    endpoint: &endpoint,
                    destination: &destination,
                },
                &now(),
            ),
            Err(AppPolicyError::ExternalDestinationDenied)
        );
        let mut egress_policy = policy(AppModelProcessing::None);
        egress_policy.policy.external_egress = AppExternalEgress::ApprovedDestinations;
        egress_policy.policy.approved_destinations = vec![destination.clone()];
        assert!(authorize_processing_target(
            &remote,
            &egress_policy,
            AppProcessingTarget::ExternalTool {
                endpoint: &endpoint,
                destination: &destination,
            },
            &now(),
        )
        .is_ok());
        // "Any public host" needs the data-handling policy to allow it: a
        // named-destination policy does not, and an any-host policy still
        // admits its named destinations.
        assert_eq!(
            authorize_processing_target(
                &remote,
                &egress_policy,
                AppProcessingTarget::ExternalAnyPublicHost {
                    endpoint: &endpoint
                },
                &now(),
            ),
            Err(AppPolicyError::ExternalDestinationDenied)
        );
        egress_policy.policy.external_egress = AppExternalEgress::AnyPublicHost;
        for target in [
            AppProcessingTarget::ExternalAnyPublicHost {
                endpoint: &endpoint,
            },
            AppProcessingTarget::ExternalTool {
                endpoint: &endpoint,
                destination: &destination,
            },
        ] {
            assert!(authorize_processing_target(&remote, &egress_policy, target, &now()).is_ok());
        }
    }

    #[test]
    fn hidden_consumers_receive_only_what_their_capability_can_protect() {
        let sensitive_labels = resolved_labels(
            AppDataClassification::Sensitive,
            AppModelProcessing::LocalOnly,
            "hidden",
        );
        let analytics = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::Analytics,
            true,
            AppDataClassification::Secret,
            AppPayloadCapability::ProtectedLabeledRetention,
            revision(1),
        );
        assert_eq!(
            decide_hidden_consumer(
                Some(&sensitive_labels),
                &policy(AppModelProcessing::LocalOnly),
                &analytics,
                None,
                &now(),
            ),
            consumer_decision(
                AppConsumerDecision::MetadataOnly,
                AppConsumerDecisionReason::MetadataConsumer
            )
        );

        let public_labels = resolved_labels(
            AppDataClassification::Public,
            AppModelProcessing::LocalOnly,
            "policy-floor",
        );
        let public_only = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::Analytics,
            true,
            AppDataClassification::Public,
            AppPayloadCapability::MetadataOnly,
            revision(1),
        );
        let mut sensitive_policy = policy(AppModelProcessing::LocalOnly);
        sensitive_policy.policy.classification_floor = AppDataClassification::Sensitive;
        assert_eq!(
            decide_hidden_consumer(
                Some(&public_labels),
                &sensitive_policy,
                &public_only,
                None,
                &now(),
            )
            .reason,
            AppConsumerDecisionReason::ClassificationUnsupported
        );

        let eval = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::EvalArtifact,
            true,
            AppDataClassification::Secret,
            AppPayloadCapability::EphemeralLabeledContent,
            revision(1),
        );
        assert_eq!(
            decide_hidden_consumer(
                Some(&sensitive_labels),
                &policy(AppModelProcessing::LocalOnly),
                &eval,
                None,
                &now(),
            )
            .decision,
            AppConsumerDecision::MetadataOnly
        );

        let embedding = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::Embedding,
            true,
            AppDataClassification::Sensitive,
            AppPayloadCapability::EphemeralLabeledContent,
            revision(1),
        );
        assert_eq!(
            decide_hidden_consumer(
                Some(&sensitive_labels),
                &policy(AppModelProcessing::LocalOnly),
                &embedding,
                None,
                &now(),
            )
            .reason,
            AppConsumerDecisionReason::ProcessingTargetMissing
        );
        let remote = endpoint(AppEndpointClass::External, false);
        let model = reference("model:remote");
        assert_eq!(
            decide_hidden_consumer(
                Some(&sensitive_labels),
                &policy(AppModelProcessing::RemoteAllowed),
                &embedding,
                Some(AppProcessingTarget::Model {
                    endpoint: &remote,
                    model_ref: &model,
                }),
                &now(),
            )
            .reason,
            AppConsumerDecisionReason::ProcessingTargetDenied
        );
        assert_eq!(
            decide_hidden_consumer(
                None,
                &policy(AppModelProcessing::RemoteAllowed),
                &embedding,
                None,
                &now(),
            )
            .reason,
            AppConsumerDecisionReason::UnlabeledContent
        );

        let label_blind = AppConsumerCapability::from_trusted_registry(
            AppHiddenConsumer::RawTraceCapture,
            false,
            AppDataClassification::Secret,
            AppPayloadCapability::ProtectedLabeledRetention,
            revision(1),
        );
        assert_eq!(
            decide_hidden_consumer(
                Some(&sensitive_labels),
                &policy(AppModelProcessing::LocalOnly),
                &label_blind,
                None,
                &now(),
            ),
            consumer_decision(
                AppConsumerDecision::MetadataOnly,
                AppConsumerDecisionReason::LabelsUnsupported
            )
        );
    }

    fn continuation() -> AppContinuationPartition {
        let endpoint = endpoint(AppEndpointClass::LoopbackManaged, true);
        AppContinuationPartition::from_resolved_context(
            AppScopeBindingRef::parse("scope_owner_default").expect("valid scope"),
            AppInstallationId::parse("install_policy_test").expect("valid installation"),
            reference("package:policy-test:1.0.0"),
            revision(3),
            revision(2),
            &endpoint,
            reference("model:local"),
            AppDigest::blake3(b"disclosure"),
            AppDigest::blake3(b"authority"),
            AppDigest::blake3(b"handling"),
        )
    }

    #[test]
    fn policy_intersection_never_broadens_egress_memory_or_processing() {
        let left = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };
        let right = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Sensitive,
            model_processing: AppModelProcessing::RemoteAllowed,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::CandidateAllowed,
            external_egress: AppExternalEgress::ApprovedDestinations,
            approved_destinations: vec![reference("destination:crm")],
        };

        let joined = intersect_app_data_handling_policies(&left, &right);
        assert_eq!(
            joined.classification_floor,
            AppDataClassification::Sensitive
        );
        assert_eq!(joined.model_processing, AppModelProcessing::LocalOnly);
        assert_eq!(joined.memory_promotion, AppMemoryPromotion::Denied);
        assert_eq!(joined.external_egress, AppExternalEgress::Denied);
        assert!(joined.approved_destinations.is_empty());

        let mut left = left;
        left.external_egress = AppExternalEgress::ApprovedDestinations;
        left.approved_destinations = vec![
            reference("destination:calendar"),
            reference("destination:crm"),
        ];
        let mut right = right;
        right.approved_destinations =
            vec![reference("destination:crm"), reference("destination:mail")];
        let joined = intersect_app_data_handling_policies(&left, &right);
        assert_eq!(
            joined.external_egress,
            AppExternalEgress::ApprovedDestinations
        );
        assert_eq!(
            joined.approved_destinations,
            vec![reference("destination:crm")]
        );
    }

    #[test]
    fn continuation_reuse_requires_exact_policy_authority_endpoint_and_model_identity() {
        let baseline = continuation();
        assert!(baseline.has_same_identity_as(&baseline));
        assert_eq!(
            baseline.partition_id().expect("partition digest"),
            baseline.partition_id().expect("stable partition digest")
        );

        let mut changed = baseline.clone();
        changed.endpoint_ref = reference("endpoint:other");
        assert!(!changed.has_same_identity_as(&baseline));
        let mut changed = baseline.clone();
        changed.endpoint_trust_revision = revision(10);
        assert!(!changed.has_same_identity_as(&baseline));
        let mut changed = baseline.clone();
        changed.endpoint_configuration_digest = AppDigest::blake3(b"new-endpoint-config");
        assert!(!changed.has_same_identity_as(&baseline));
        let mut changed = baseline.clone();
        changed.model_ref = reference("model:other");
        assert!(!changed.has_same_identity_as(&baseline));
        let mut changed = baseline.clone();
        changed.disclosure_policy_digest = AppDigest::blake3(b"new-disclosure");
        assert!(!changed.has_same_identity_as(&baseline));
        let mut changed = baseline.clone();
        changed.authority_digest = AppDigest::blake3(b"new-authority");
        assert!(!changed.has_same_identity_as(&baseline));
        let mut changed = baseline.clone();
        changed.handling_digest = AppDigest::blake3(b"new-handling");
        assert!(!changed.has_same_identity_as(&baseline));
    }
}
