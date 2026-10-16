//! Phase 5C source-to-destination broker.
//!
//! A personal agent may compose eligible app results into another app's
//! declared action. This owner rechecks both grants, schemas and policies,
//! refuses incompatible fields, and mints a transfer envelope. Direct
//! installation-to-installation access is never granted. Delegated, outward
//! and app-workflow callers cannot mint this path.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[cfg(any(test, feature = "test-fixtures"))]
use super::models::AppDataClassification;
use super::{
    authority::AuthenticatedAppScope,
    boundary::{AppAgentProcessingClass, AppBoundaryError, AppStoreReadAudience},
    models::{
        AppContractLimits, AppDataEnvelope, AppDataSource, AppDigest, AppFieldPath,
        AppHandlingLabels, AppInstallationId, AppModelProcessing, AppName, AppProtocolVersion,
        AppRecordProjection, AppReference, AppRevision, AppSourceRecordFence, AppSourceRef,
        AppSourceRefKind, ValidateAppContract,
    },
    policy::{
        intersect_app_data_handling_policies, join_app_content, AppHandlingConstraint,
        AppJoinedContent, AppPolicyError, ResolvedAppHandlingLabels, RevalidatedAppEnvelope,
    },
    records::{validate_policy, AppDataHandlingPolicy, AppPersonalAgentAccess},
    value_mapping::{
        apply_compiled_value_mapping, compile_value_mapping, AppCompiledValueMapping,
        AppValueMappingError, AppValueMappingOperation, AppValueSchemaContract,
    },
};

pub(crate) const TRANSFER_ENVELOPE_SCHEMA_VERSION: u8 = 7;
pub(crate) const LEGACY_ACTION_TRANSFER_SCHEMA_VERSION: u8 = 6;
pub(crate) const LEGACY_ENTITY_TRANSFER_SCHEMA_VERSION: u8 = 5;

/// Payload-free, independently verifiable identity of one source action in a
/// bounded composition chain. It is durable audit evidence only. The workflow
/// owner reconstructs a move-only live authority chain from these identities
/// before each publication/effect boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompositionAuthorityHopReceipt {
    pub run_ref: AppReference,
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub schema_revision: AppRevision,
    pub grant_revision: AppRevision,
    pub action_id: AppName,
    pub action_revision: AppRevision,
    pub result_schema_ref: AppReference,
    pub idempotency_key: AppReference,
    pub task_binding_digest: AppDigest,
    pub authority_digest: AppDigest,
    pub result_digest: AppDigest,
    pub output_revision: AppRevision,
    pub handling_policy_digest: AppDigest,
    pub provenance_digest: AppDigest,
    pub content_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompositionAuthorityChainReceipt {
    pub chain_request_digest: AppDigest,
    pub hop_index: u8,
    pub hop_count: u8,
    pub source_hops: Vec<AppCompositionAuthorityHopReceipt>,
}

/// Immutable server-sealed identity of an app action result used as the source
/// of another brokered action. The receipt is evidence, not bearer authority:
/// workflow admission reopens the binding and result sidecars and then checks
/// the source and destination registry tuples again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSourceActionResultFence {
    pub source_run_ref: AppReference,
    pub source_task_binding_digest: AppDigest,
    /// Digest of the source app's accepted grant policy. This is distinct
    /// from the possibly narrower result policy carried by the envelope and
    /// lets the registry snapshot detect a changed or revoked source grant.
    pub source_grant_policy_digest: AppDigest,
    pub output_revision: AppRevision,
    pub result_digest: AppDigest,
    pub fields: Vec<AppFieldPath>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppCompositionFieldRefusal {
    DestinationPersonalAgentDenied,
    SourceClassificationExceedsDestination,
    DestinationWouldDeclassify,
    DestinationWouldBroadenProcessing,
    DestinationWouldBroadenEgress,
    PersonalAgentProcessingIncompatible,
    UnmappedRequiredTarget,
    MappingFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompositionRefusedField {
    pub field: AppFieldPath,
    pub reason: AppCompositionFieldRefusal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompositionTransferReceipt {
    pub schema_version: u8,
    pub transfer_id: AppReference,
    pub source_installation_id: AppInstallationId,
    pub source_installation_generation: u64,
    pub source_package_revision_ref: AppReference,
    pub source_schema_revision: AppRevision,
    pub source_grant_revision: AppRevision,
    pub source_handling_policy_digest: AppDigest,
    pub source_provenance_digest: AppDigest,
    pub source_content_digest: AppDigest,
    /// Exact canonical upstream provenance references before the broker adds
    /// its own transfer reference. V5 entity receipts omit this field; every
    /// V6 receipt must bind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_refs_digest: Option<AppDigest>,
    #[serde(default)]
    pub source_records: Vec<AppSourceRecordFence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_action_result: Option<AppSourceActionResultFence>,
    /// Full payload-free source lineage for V7 action-result transfers. The
    /// receipt is deserializable for recovery; it is never executable
    /// authority and every hop is reopened against live sidecars and registry
    /// state immediately before launch/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_chain: Option<AppCompositionAuthorityChainReceipt>,
    pub destination_installation_id: AppInstallationId,
    pub destination_package_revision_ref: AppReference,
    pub destination_schema_revision: AppRevision,
    pub destination_grant_revision: AppRevision,
    pub destination_action_id: AppName,
    pub destination_action_revision: AppRevision,
    pub destination_input_schema_ref: AppReference,
    pub destination_result_schema_ref: AppReference,
    pub destination_handling_policy_digest: AppDigest,
    /// Digest of the complete monotone A ∩ B policy carried into the
    /// destination workflow. This is distinct from the derived-content label
    /// digest, which also binds purpose, audience and source provenance.
    pub effective_policy_digest: AppDigest,
    pub joined_handling_policy_digest: AppDigest,
    pub joined_provenance_digest: AppDigest,
    pub destination_content_digest: AppDigest,
    /// Digest of the caller's canonical mapping operations. The compiled
    /// mapping digest below also binds both schemas; this separate request
    /// digest lets durable same-key recovery reject changed arguments before
    /// an ephemeral source projection can be reopened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_request_digest: Option<AppDigest>,
    /// Digest of the exact caller-held source locator and selector used to mint
    /// this transfer. For record composition this binds the opaque projection
    /// handle plus optional record id; for action-result composition it binds
    /// the canonical source run ref. The digest is replay identity, not bearer
    /// authority: every live source fence is still revalidated independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_request_digest: Option<AppDigest>,
    pub mapping_digest: AppDigest,
    pub accepted_fields: Vec<AppFieldPath>,
    pub refused_fields: Vec<AppCompositionRefusedField>,
    pub transferred_at: DateTime<Utc>,
}

#[derive(PartialEq)]
pub(crate) struct AppBrokeredTransfer {
    pub(crate) envelope: AppDataEnvelope<Value>,
    pub(crate) admission: AppBrokeredTransferAdmission,
    pub(crate) joined: AppJoinedContent,
}

impl std::fmt::Debug for AppBrokeredTransfer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppBrokeredTransfer")
            .field("transfer_id", &self.admission.receipt.transfer_id)
            .field("source", &self.envelope.source)
            .field(
                "destination_installation_id",
                &self.envelope.installation_id,
            )
            .field("destination_content_digest", &self.envelope.content_digest)
            .field("source_ref_count", &self.envelope.source_refs.len())
            .field(
                "accepted_field_count",
                &self.admission.receipt.accepted_fields.len(),
            )
            .field(
                "refused_field_count",
                &self.admission.receipt.refused_fields.len(),
            )
            .finish_non_exhaustive()
    }
}

impl AppBrokeredTransfer {
    /// Seal the exact current source row after the store has revalidated it.
    /// The pure broker cannot invent this evidence because it intentionally
    /// owns no entity-store access.
    pub(crate) fn seal_source_record(
        mut self,
        projection: &AppRecordProjection,
        mut policy_influence_fields: Vec<AppFieldPath>,
        source_request_digest: AppDigest,
    ) -> Result<Self, AppCompositionError> {
        if !self.admission.receipt.source_records.is_empty()
            || self.admission.receipt.source_action_result.is_some()
        {
            return Err(AppCompositionError::StaleSourceEnvelope);
        }
        let fields = projection.fields.keys().cloned().collect::<Vec<_>>();
        policy_influence_fields.sort();
        policy_influence_fields.dedup();
        if policy_influence_fields.is_empty()
            || !fields
                .iter()
                .all(|field| policy_influence_fields.contains(field))
        {
            return Err(AppCompositionError::StaleSourceEnvelope);
        }
        let selected_values_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&projection.fields)?)?;
        self.admission.receipt.source_records = vec![AppSourceRecordFence {
            entity: projection.entity.clone(),
            record_id: projection.record_id.clone(),
            record_revision: projection.record_revision,
            fields,
            policy_influence_fields,
            selected_values_digest,
        }];
        self.admission.receipt.source_request_digest = Some(source_request_digest);
        self.envelope.handling_labels.provenance_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&self.admission.receipt)?)?;
        Ok(self)
    }

    /// Seal a workflow result source after the workflow owner has reopened the
    /// exact immutable task/result pair. Nested result composition is refused
    /// by that owner in V1; the optional fence keeps the receipt format
    /// extensible without pretending an entity-row fence can describe an
    /// action result.
    pub(crate) fn seal_source_action_result(
        mut self,
        fence: AppSourceActionResultFence,
        source_request_digest: AppDigest,
    ) -> Result<Self, AppCompositionError> {
        if !self.admission.receipt.source_records.is_empty()
            || self.admission.receipt.source_action_result.is_some()
            || fence.fields.len() > AppContractLimits::default().max_collection_items()
            || !fence.fields.windows(2).all(|fields| fields[0] < fields[1])
        {
            return Err(AppCompositionError::StaleSourceEnvelope);
        }
        self.admission.receipt.source_action_result = Some(fence);
        self.admission.receipt.source_request_digest = Some(source_request_digest);
        // Internal one-hop composition remains readable as the V6 action
        // receipt shape. The supported-public chain owner upgrades it to V7
        // only after attaching its independently revalidated source lineage.
        self.admission.receipt.schema_version = LEGACY_ACTION_TRANSFER_SCHEMA_VERSION;
        self.envelope.handling_labels.provenance_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&self.admission.receipt)?)?;
        Ok(self)
    }

    pub(crate) fn seal_authority_chain(
        mut self,
        authority_chain: AppCompositionAuthorityChainReceipt,
    ) -> Result<Self, AppCompositionError> {
        let limit = super::composition_service::MAX_APP_ACTION_COMPOSITION_HOPS;
        if self.admission.receipt.source_action_result.is_none()
            || self.admission.receipt.authority_chain.is_some()
            || authority_chain.source_hops.is_empty()
            || authority_chain.source_hops.len() > limit
            || usize::from(authority_chain.hop_count) > limit
            || authority_chain.hop_count == 0
            || authority_chain.hop_index >= authority_chain.hop_count
            || authority_chain.source_hops.len() != usize::from(authority_chain.hop_index) + 1
        {
            return Err(AppCompositionError::StaleSourceEnvelope);
        }
        self.admission.receipt.schema_version = TRANSFER_ENVELOPE_SCHEMA_VERSION;
        self.admission.receipt.authority_chain = Some(authority_chain);
        self.envelope.handling_labels.provenance_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&self.admission.receipt)?)?;
        Ok(self)
    }
}

/// Move-only server admission for one brokered workflow launch. The durable
/// receipt remains serializable for replay and audit, but cannot by itself
/// mint this non-deserializable execution authority.
#[derive(Debug, PartialEq)]
pub(crate) struct AppBrokeredTransferAdmission {
    receipt: AppCompositionTransferReceipt,
    effective_policy: AppDataHandlingPolicy,
}

impl AppBrokeredTransferAdmission {
    /// Reconstitute move-only launch authority from an already authenticated
    /// durable task binding. The caller must have loaded the receipt and policy
    /// from the same sealed sidecar; the canonical workflow admission path
    /// revalidates their digests plus live source/destination authority before
    /// it may start an execution.
    pub(crate) fn from_sealed_binding(
        receipt: AppCompositionTransferReceipt,
        effective_policy: AppDataHandlingPolicy,
    ) -> Self {
        Self {
            receipt,
            effective_policy,
        }
    }

    pub(crate) fn receipt(&self) -> &AppCompositionTransferReceipt {
        &self.receipt
    }

    pub(crate) fn effective_policy(&self) -> &AppDataHandlingPolicy {
        &self.effective_policy
    }
}

/// Destination-owned contract supplied by the server-side composition owner.
/// It is deliberately not deserializable and keeps the destination execution
/// tuple separate from source provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppCompositionDestination {
    pub(crate) installation_id: AppInstallationId,
    pub(crate) package_revision_ref: AppReference,
    pub(crate) schema_revision: AppRevision,
    pub(crate) grant_revision: AppRevision,
    pub(crate) action_id: AppName,
    pub(crate) action_revision: AppRevision,
    pub(crate) input_schema_ref: AppReference,
    pub(crate) input_schema: AppValueSchemaContract,
    pub(crate) result_schema_ref: AppReference,
    pub(crate) policy: AppDataHandlingPolicy,
}

#[derive(Debug, Error)]
pub enum AppCompositionError {
    #[error(transparent)]
    Authority(#[from] super::authority::AppAuthorityError),
    #[error(transparent)]
    Boundary(#[from] AppBoundaryError),
    #[error(transparent)]
    Policy(#[from] AppPolicyError),
    #[error(transparent)]
    Mapping(#[from] AppValueMappingError),
    #[error(transparent)]
    Contract(#[from] super::models::AppContractError),
    #[error("app composition encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("app composition requires a direct owner-facing personal agent")]
    IndirectAudience,
    #[error("destination personal-agent access is denied")]
    DestinationPersonalAgentDenied,
    #[error("source personal-agent access is denied")]
    SourcePersonalAgentDenied,
    #[error("source and destination scopes do not match")]
    CrossScopeTransfer,
    #[error("source envelope is not current for this transfer")]
    StaleSourceEnvelope,
    #[error("no eligible source fields remain after destination policy intersection")]
    NoEligibleFields,
    #[error("destination required field `{0}` cannot be filled from eligible sources")]
    MissingRequiredTarget(String),
}

/// Broker one source envelope into a destination action input.
///
/// The caller supplies already-compiled source and destination value schemas
/// plus the destination grant policy. This function does not open another
/// app's store. A copied envelope is not authority.
pub(crate) fn broker_source_to_destination(
    authenticated_scope: &AuthenticatedAppScope,
    read_audience: &AppStoreReadAudience,
    source: &AppDataEnvelope<Value>,
    source_policy: &AppDataHandlingPolicy,
    source_schema: &AppValueSchemaContract,
    source_installation_generation: u64,
    destination: &AppCompositionDestination,
    operations: Vec<AppValueMappingOperation>,
    transfer_id: AppReference,
    transferred_at: DateTime<Utc>,
) -> Result<AppBrokeredTransfer, AppCompositionError> {
    broker_source_to_destination_inner(
        authenticated_scope,
        Some(read_audience),
        source,
        source_policy,
        source_schema,
        source_installation_generation,
        destination,
        operations,
        transfer_id,
        transferred_at,
    )
}

/// Broker a server-reopened app action result directly into another app action.
/// The result bytes are never projected into the requesting model, so a remote
/// model's processing class does not reject an otherwise local-only transfer.
/// Personal-agent projection must still be approved by both source and
/// destination policies because this path is initiated by that owner-facing
/// agent rather than by either app directly.
pub(crate) fn broker_action_result_to_destination(
    authenticated_scope: &AuthenticatedAppScope,
    source: &AppDataEnvelope<Value>,
    source_policy: &AppDataHandlingPolicy,
    source_schema: &AppValueSchemaContract,
    source_installation_generation: u64,
    destination: &AppCompositionDestination,
    operations: Vec<AppValueMappingOperation>,
    transfer_id: AppReference,
    transferred_at: DateTime<Utc>,
) -> Result<AppBrokeredTransfer, AppCompositionError> {
    broker_source_to_destination_inner(
        authenticated_scope,
        None,
        source,
        source_policy,
        source_schema,
        source_installation_generation,
        destination,
        operations,
        transfer_id,
        transferred_at,
    )
}

#[allow(clippy::too_many_arguments)]
fn broker_source_to_destination_inner(
    authenticated_scope: &AuthenticatedAppScope,
    read_audience: Option<&AppStoreReadAudience>,
    source: &AppDataEnvelope<Value>,
    source_policy: &AppDataHandlingPolicy,
    source_schema: &AppValueSchemaContract,
    source_installation_generation: u64,
    destination: &AppCompositionDestination,
    operations: Vec<AppValueMappingOperation>,
    transfer_id: AppReference,
    transferred_at: DateTime<Utc>,
) -> Result<AppBrokeredTransfer, AppCompositionError> {
    authenticated_scope.ensure_live_at(&transferred_at)?;
    if source.scope_binding_ref != *authenticated_scope.scope_binding_ref() {
        return Err(AppCompositionError::CrossScopeTransfer);
    }
    source.validate_app_contract(&AppContractLimits::default())?;
    AppRevision::new(source_installation_generation)?;
    if transferred_at < source.produced_at {
        return Err(AppCompositionError::StaleSourceEnvelope);
    }
    validate_policy(source_policy, &AppContractLimits::default())?;
    let source_policy_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(source_policy)?)?;
    if source.handling_labels.policy_digest != source_policy_digest
        || source.handling_labels.classification < source_policy.classification_floor
        || source.handling_labels.model_processing > source_policy.model_processing
    {
        return Err(AppCompositionError::StaleSourceEnvelope);
    }
    if source_policy.personal_agent_access != AppPersonalAgentAccess::ApprovedProjection {
        return Err(AppCompositionError::SourcePersonalAgentDenied);
    }
    if destination.policy.personal_agent_access != AppPersonalAgentAccess::ApprovedProjection {
        return Err(AppCompositionError::DestinationPersonalAgentDenied);
    }
    validate_policy(&destination.policy, &AppContractLimits::default())?;
    let mut effective_policy =
        intersect_app_data_handling_policies(source_policy, &destination.policy);
    validate_policy(&effective_policy, &AppContractLimits::default())?;

    let processing_class = match read_audience {
        Some(AppStoreReadAudience::PersonalAgent {
            processing_class, ..
        }) => Some(*processing_class),
        Some(_) => return Err(AppCompositionError::IndirectAudience),
        None => None,
    };
    if read_audience.is_some_and(|audience| {
        !audience.permits_policy(
            source.handling_labels.classification,
            source.handling_labels.model_processing,
        )
    }) {
        return Err(AppCompositionError::IndirectAudience);
    }

    let mut refused = Vec::new();
    if let Some(processing_class) = processing_class {
        refuse_policy_fields(source, processing_class, &mut refused);
    }

    let mapping_request_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&operations)?)?;
    let mapping = compile_value_mapping(source_schema, &destination.input_schema, operations)?;
    let mapped = apply_compiled_value_mapping(
        &mapping,
        source_schema,
        &destination.input_schema,
        &source.value,
    )?;
    let eligible = accepted_target_fields(&mapping, &refused);
    let mapped = retain_accepted_mapping_paths(&mapped, &eligible)?;
    let accepted = eligible
        .into_iter()
        .filter(|path| read_mapping_path(&mapped, path).is_some())
        .collect::<Vec<_>>();

    if accepted.is_empty() {
        return Err(AppCompositionError::NoEligibleFields);
    }
    for (path, contract) in destination_schema_fields(&destination.input_schema) {
        if contract.required && !accepted.iter().any(|field| field == path) {
            return Err(AppCompositionError::MissingRequiredTarget(path.to_string()));
        }
    }

    let constraint = AppHandlingConstraint {
        classification_floor: effective_policy
            .classification_floor
            .max(source.handling_labels.classification),
        model_processing: effective_policy
            .model_processing
            .min(source.handling_labels.model_processing),
        policy_digest: destination_policy_digest(&destination.policy)?,
        purpose: AppReference::parse("purpose:app-composition")?,
        audience_ref: AppReference::parse(format!(
            "installation:{}",
            destination.installation_id.as_str()
        ))?,
    };
    let resolved_labels =
        ResolvedAppHandlingLabels::from_trusted_policy(source.handling_labels.clone());
    let revalidated = RevalidatedAppEnvelope::from_trusted_resolution(
        source,
        resolved_labels,
        &AppContractLimits::default(),
    )?;
    let joined = join_app_content(
        &[revalidated],
        mapped,
        &constraint,
        &AppContractLimits::default(),
    )?;
    effective_policy.classification_floor = effective_policy
        .classification_floor
        .max(joined.handling_labels().classification);
    effective_policy.model_processing = effective_policy
        .model_processing
        .min(joined.handling_labels().model_processing);
    validate_policy(&effective_policy, &AppContractLimits::default())?;
    let effective_policy_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&effective_policy)?)?;

    let mut source_refs = joined.source_refs().to_vec();
    let source_refs_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&source_refs)?)?;
    source_refs.push(AppSourceRef {
        kind: AppSourceRefKind::ExternalReceipt,
        reference: transfer_id.clone(),
        revision: Some(AppRevision::new(1)?),
        fields: accepted.clone(),
    });
    let receipt = AppCompositionTransferReceipt {
        schema_version: TRANSFER_ENVELOPE_SCHEMA_VERSION,
        transfer_id: transfer_id.clone(),
        source_installation_id: source.installation_id.clone(),
        source_installation_generation,
        source_package_revision_ref: source.package_revision_ref.clone(),
        source_schema_revision: source.schema_revision,
        source_grant_revision: source.grant_revision,
        source_handling_policy_digest: source.handling_labels.policy_digest.clone(),
        source_provenance_digest: source.handling_labels.provenance_digest.clone(),
        source_content_digest: source.content_digest.clone(),
        source_refs_digest: Some(source_refs_digest),
        source_records: Vec::new(),
        source_action_result: None,
        authority_chain: None,
        destination_installation_id: destination.installation_id.clone(),
        destination_package_revision_ref: destination.package_revision_ref.clone(),
        destination_schema_revision: destination.schema_revision,
        destination_grant_revision: destination.grant_revision,
        destination_action_id: destination.action_id.clone(),
        destination_action_revision: destination.action_revision,
        destination_input_schema_ref: destination.input_schema_ref.clone(),
        destination_result_schema_ref: destination.result_schema_ref.clone(),
        destination_handling_policy_digest: constraint.policy_digest.clone(),
        effective_policy_digest,
        joined_handling_policy_digest: joined.handling_labels().policy_digest.clone(),
        joined_provenance_digest: joined.handling_labels().provenance_digest.clone(),
        destination_content_digest: joined.content_digest().clone(),
        mapping_request_digest: Some(mapping_request_digest),
        source_request_digest: None,
        mapping_digest: mapping.mapping_digest().clone(),
        accepted_fields: accepted,
        refused_fields: refused,
        transferred_at,
    };
    let handling_labels = AppHandlingLabels {
        classification: joined.handling_labels().classification,
        model_processing: joined.handling_labels().model_processing,
        policy_digest: joined.handling_labels().policy_digest.clone(),
        provenance_digest: AppDigest::blake3_canonical_json(&serde_json::to_value(&receipt)?)?,
    };
    let envelope = AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source: AppDataSource::BrokeredTransfer,
        scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
        installation_id: destination.installation_id.clone(),
        package_revision_ref: destination.package_revision_ref.clone(),
        schema_revision: destination.schema_revision,
        grant_revision: destination.grant_revision,
        value_schema_ref: destination.input_schema_ref.clone(),
        value: joined.value().clone(),
        source_refs,
        handling_labels: handling_labels.clone(),
        content_digest: joined.content_digest().clone(),
        produced_at: transferred_at,
        expires_at: source.expires_at,
    };
    envelope.validate_app_contract(&AppContractLimits::default())?;
    Ok(AppBrokeredTransfer {
        envelope,
        admission: AppBrokeredTransferAdmission {
            receipt,
            effective_policy,
        },
        joined,
    })
}

/// Delegated, handover and public/envoy executions cannot mint personal-agent
/// composition authority. The boundary type already refuses them; this helper
/// is the explicit 5C gate used by compiled tools.
pub fn personal_agent_composition_is_direct(
    source_kind: Option<&str>,
    source_agent_id: Option<&str>,
    surface: Option<&str>,
) -> bool {
    if source_agent_id.is_some_and(|value| !value.trim().is_empty()) {
        return false;
    }
    let Some(kind) = source_kind.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if !matches!(kind, "direct" | "chat_inline" | "product_feature") {
        return false;
    }
    match surface.map(str::trim).filter(|value| !value.is_empty()) {
        Some(
            "chat" | "realtime_voice" | "task" | "tutor" | "app_copilot" | "contextual_assist",
        ) => true,
        Some(_) | None => false,
    }
}

fn refuse_policy_fields(
    source: &AppDataEnvelope<Value>,
    processing_class: AppAgentProcessingClass,
    refused: &mut Vec<AppCompositionRefusedField>,
) {
    let fields = source
        .source_refs
        .iter()
        .flat_map(|source| source.fields.iter().cloned())
        .collect::<Vec<_>>();
    let fields = if fields.is_empty() {
        source
            .value
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .filter_map(|key| AppFieldPath::parse(key.clone()).ok())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        fields
    };
    for field in fields {
        if matches!(processing_class, AppAgentProcessingClass::RemoteModel)
            && source.handling_labels.model_processing != AppModelProcessing::RemoteAllowed
        {
            refused.push(AppCompositionRefusedField {
                field,
                reason: AppCompositionFieldRefusal::PersonalAgentProcessingIncompatible,
            });
        }
    }
}

fn accepted_target_fields(
    mapping: &AppCompiledValueMapping,
    refused: &[AppCompositionRefusedField],
) -> Vec<AppFieldPath> {
    let refused_sources = refused
        .iter()
        .map(|field| field.field.as_str().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    mapping
        .operations()
        .iter()
        .filter_map(|operation| match operation {
            AppValueMappingOperation::Select { source, target }
            | AppValueMappingOperation::Convert { source, target, .. }
            | AppValueMappingOperation::MapEnum { source, target, .. } => {
                if refused_sources.contains(source.as_str()) {
                    None
                } else {
                    Some(target.clone())
                }
            },
            AppValueMappingOperation::Constant { target, .. } => Some(target.clone()),
        })
        .collect()
}

fn retain_accepted_mapping_paths(
    mapped: &Value,
    accepted: &[AppFieldPath],
) -> Result<Value, AppCompositionError> {
    let mut output = serde_json::Map::new();
    for path in accepted {
        let Some(value) = read_mapping_path(mapped, path) else {
            continue;
        };
        write_mapping_path(&mut output, path, value.clone())?;
    }
    Ok(Value::Object(output))
}

fn read_mapping_path<'a>(mut value: &'a Value, path: &AppFieldPath) -> Option<&'a Value> {
    for segment in path.as_str().split('.') {
        value = value.as_object()?.get(segment)?;
    }
    Some(value)
}

fn write_mapping_path(
    root: &mut serde_json::Map<String, Value>,
    path: &AppFieldPath,
    value: Value,
) -> Result<(), AppCompositionError> {
    let mut segments = path.as_str().split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_owned(), value);
            return Ok(());
        }
        let child = current
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        current = child.as_object_mut().ok_or_else(|| {
            AppCompositionError::Mapping(AppValueMappingError::TargetPathConflict(path.to_string()))
        })?;
    }
    Err(AppCompositionError::Mapping(
        AppValueMappingError::TargetPathConflict(path.to_string()),
    ))
}

fn destination_policy_digest(
    policy: &AppDataHandlingPolicy,
) -> Result<AppDigest, AppCompositionError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::to_value(
        policy,
    )?)?)
}

fn destination_schema_fields(
    schema: &AppValueSchemaContract,
) -> impl Iterator<Item = (&AppFieldPath, &super::value_mapping::AppValueFieldContract)> {
    schema.fields()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::magician_v2::{
        agents::{AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface},
        apps::{
            authority::AuthenticatedAppScope,
            boundary::{AppDirectOwnerExecutionEvidence, AppPersonalAgentProviderGrant},
            query_semantics::AppQueryScalarKind,
            records::{AppExternalEgress, AppMemoryPromotion},
            registry::tests::{authenticated_scope, time},
            value_mapping::AppValueFieldContract,
        },
    };

    fn labels(
        classification: AppDataClassification,
        model_processing: AppModelProcessing,
    ) -> AppHandlingLabels {
        let source_policy = policy(
            AppPersonalAgentAccess::ApprovedProjection,
            classification,
            model_processing,
            AppExternalEgress::Denied,
        );
        AppHandlingLabels {
            classification,
            model_processing,
            policy_digest: AppDigest::blake3_canonical_json(
                &serde_json::to_value(source_policy).unwrap(),
            )
            .unwrap(),
            provenance_digest: AppDigest::blake3(b"source-provenance"),
        }
    }

    fn source_policy(source: &AppDataEnvelope<Value>) -> AppDataHandlingPolicy {
        policy(
            AppPersonalAgentAccess::ApprovedProjection,
            source.handling_labels.classification,
            source.handling_labels.model_processing,
            AppExternalEgress::Denied,
        )
    }

    fn policy(
        personal_agent: AppPersonalAgentAccess,
        classification: AppDataClassification,
        model_processing: AppModelProcessing,
        egress: AppExternalEgress,
    ) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: classification,
            model_processing,
            personal_agent_access: personal_agent,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: egress,
            approved_destinations: Vec::new(),
        }
    }

    fn schema(fields: &[(&str, bool)]) -> AppValueSchemaContract {
        let mut map = BTreeMap::new();
        for (name, required) in fields {
            map.insert(
                AppFieldPath::parse(*name).unwrap(),
                AppValueFieldContract {
                    kind: AppQueryScalarKind::Text,
                    required: *required,
                    nullable: false,
                    enum_values: Default::default(),
                },
            );
        }
        AppValueSchemaContract::from_compiled_fields(map).unwrap()
    }

    fn destination(
        input_schema: AppValueSchemaContract,
        policy: AppDataHandlingPolicy,
    ) -> AppCompositionDestination {
        AppCompositionDestination {
            installation_id: AppInstallationId::parse("install_b").unwrap(),
            package_revision_ref: AppReference::parse("package:b").unwrap(),
            schema_revision: AppRevision::new(3).unwrap(),
            grant_revision: AppRevision::new(4).unwrap(),
            action_id: AppName::parse("capture_insight").unwrap(),
            action_revision: AppRevision::new(5).unwrap(),
            input_schema_ref: AppReference::parse("capture.input").unwrap(),
            input_schema,
            result_schema_ref: AppReference::parse("capture.result").unwrap(),
            policy,
        }
    }

    fn envelope(
        auth: &AuthenticatedAppScope,
        value: Value,
        handling: AppHandlingLabels,
    ) -> AppDataEnvelope<Value> {
        let content_digest = AppDigest::blake3_canonical_json(&value).unwrap();
        AppDataEnvelope {
            protocol_version: AppProtocolVersion::V1,
            source: AppDataSource::AppStore,
            scope_binding_ref: auth.scope_binding_ref().clone(),
            installation_id: AppInstallationId::parse("install_a").unwrap(),
            package_revision_ref: AppReference::parse("package:a").unwrap(),
            schema_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            value_schema_ref: AppReference::parse("schema:source").unwrap(),
            value,
            source_refs: vec![AppSourceRef {
                kind: AppSourceRefKind::EntityField,
                reference: AppReference::parse("entity:person/record:1").unwrap(),
                revision: Some(AppRevision::new(1).unwrap()),
                fields: vec![
                    AppFieldPath::parse("name").unwrap(),
                    AppFieldPath::parse("note").unwrap(),
                ],
            }],
            handling_labels: handling,
            content_digest,
            produced_at: time(4),
            expires_at: None,
        }
    }

    fn audience(
        authenticated: &AuthenticatedAppScope,
        class: AppAgentProcessingClass,
    ) -> AppStoreReadAudience {
        let grant = AppPersonalAgentProviderGrant::from_trusted_provider_registry(
            class,
            AppDataClassification::Secret,
            AppRevision::new(1).unwrap(),
            AppDigest::blake3(b"provider"),
            time(3),
            time(20),
        )
        .unwrap();
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: None,
            target_agent_id: "primary".to_owned(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::ChatInline,
            chat_session_id: Some("session:1".to_owned()),
            chat_turn_id: Some("turn:1".to_owned()),
        };
        let evidence = AppDirectOwnerExecutionEvidence::from_resolved_execution(
            authenticated,
            &invocation,
            AppReference::parse("execution:compose").unwrap(),
            grant,
            time(4),
        )
        .unwrap();
        let authority =
            super::super::boundary::AppPersonalAgentReadAuthority::from_current_execution(
                authenticated,
                evidence,
                time(4),
            )
            .unwrap();
        AppStoreReadAudience::PersonalAgent {
            execution_ref: authority.execution_ref().clone(),
            processing_class: authority.processing_class(),
            maximum_classification: authority.maximum_classification(),
        }
    }

    fn mapping() -> Vec<AppValueMappingOperation> {
        vec![
            AppValueMappingOperation::Select {
                source: AppFieldPath::parse("name").unwrap(),
                target: AppFieldPath::parse("title").unwrap(),
            },
            AppValueMappingOperation::Select {
                source: AppFieldPath::parse("note").unwrap(),
                target: AppFieldPath::parse("summary").unwrap(),
            },
        ]
    }

    #[test]
    fn direct_owner_can_broker_compatible_fields() {
        let auth = authenticated_scope("anonymous", "default");
        let source = envelope(
            &auth,
            serde_json::json!({"name": "Asha", "note": "mentor"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let dest = policy(
            AppPersonalAgentAccess::ApprovedProjection,
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            AppExternalEgress::Denied,
        );
        let destination = destination(schema(&[("title", true), ("summary", false)]), dest);
        let transfer = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("name", true), ("note", false)]),
            1,
            &destination,
            mapping(),
            AppReference::parse("transfer:1").unwrap(),
            time(5),
        )
        .unwrap();
        assert_eq!(transfer.envelope.source, AppDataSource::BrokeredTransfer);
        assert_eq!(
            transfer.envelope.package_revision_ref,
            AppReference::parse("package:b").unwrap()
        );
        assert_eq!(
            transfer.envelope.schema_revision,
            AppRevision::new(3).unwrap()
        );
        assert_eq!(
            transfer.envelope.grant_revision,
            AppRevision::new(4).unwrap()
        );
        assert_eq!(transfer.envelope.value["title"], "Asha");
        assert_eq!(transfer.admission.receipt().accepted_fields.len(), 2);
        assert!(transfer.admission.receipt().refused_fields.is_empty());
        assert!(transfer
            .envelope
            .source_refs
            .iter()
            .any(|source| source.kind == AppSourceRefKind::EntityField));
    }

    #[test]
    fn accepted_nested_target_paths_survive_policy_filtering() {
        let auth = authenticated_scope("anonymous", "default");
        let mut source = envelope(
            &auth,
            serde_json::json!({"profile": {"name": "Asha"}}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        source.source_refs[0].fields = vec![AppFieldPath::parse("profile.name").unwrap()];
        let destination = destination(
            schema(&[("customer.name", true)]),
            policy(
                AppPersonalAgentAccess::ApprovedProjection,
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                AppExternalEgress::Denied,
            ),
        );
        let transfer = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("profile.name", true)]),
            1,
            &destination,
            vec![AppValueMappingOperation::Select {
                source: AppFieldPath::parse("profile.name").unwrap(),
                target: AppFieldPath::parse("customer.name").unwrap(),
            }],
            AppReference::parse("transfer:nested").unwrap(),
            time(5),
        )
        .unwrap();
        assert_eq!(transfer.envelope.value["customer"]["name"], "Asha");
    }

    #[test]
    fn absent_optional_source_does_not_count_as_an_accepted_transfer() {
        let auth = authenticated_scope("anonymous", "default");
        let mut source = envelope(
            &auth,
            serde_json::json!({}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        source.source_refs[0].fields = vec![AppFieldPath::parse("note").unwrap()];
        let destination = destination(
            schema(&[("summary", false)]),
            policy(
                AppPersonalAgentAccess::ApprovedProjection,
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                AppExternalEgress::Denied,
            ),
        );
        let error = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("note", false)]),
            1,
            &destination,
            vec![AppValueMappingOperation::Select {
                source: AppFieldPath::parse("note").unwrap(),
                target: AppFieldPath::parse("summary").unwrap(),
            }],
            AppReference::parse("transfer:missing-optional").unwrap(),
            time(5),
        )
        .unwrap_err();
        assert!(matches!(error, AppCompositionError::NoEligibleFields));
    }

    #[test]
    fn destination_processing_capability_is_narrowed_by_source_policy() {
        let auth = authenticated_scope("anonymous", "default");
        let source = envelope(
            &auth,
            serde_json::json!({"name": "Asha", "note": "mentor"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let dest = policy(
            AppPersonalAgentAccess::ApprovedProjection,
            AppDataClassification::Personal,
            AppModelProcessing::RemoteAllowed,
            AppExternalEgress::Denied,
        );
        let destination = destination(schema(&[("title", false), ("summary", false)]), dest);
        let transfer = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("name", true), ("note", false)]),
            1,
            &destination,
            mapping(),
            AppReference::parse("transfer:2").unwrap(),
            time(5),
        )
        .unwrap();
        assert_eq!(
            transfer.admission.effective_policy().model_processing,
            AppModelProcessing::LocalOnly
        );
        assert_eq!(transfer.admission.receipt().accepted_fields.len(), 2);
    }

    #[test]
    fn carried_policy_is_tightened_to_the_actual_joined_labels() {
        let auth = authenticated_scope("anonymous", "default");
        let mut source = envelope(
            &auth,
            serde_json::json!({"name": "Asha"}),
            labels(AppDataClassification::Secret, AppModelProcessing::LocalOnly),
        );
        source.source_refs[0].fields = vec![AppFieldPath::parse("name").unwrap()];
        let source_policy = policy(
            AppPersonalAgentAccess::ApprovedProjection,
            AppDataClassification::Personal,
            AppModelProcessing::RemoteAllowed,
            AppExternalEgress::Denied,
        );
        source.handling_labels.policy_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&source_policy).unwrap())
                .unwrap();
        let destination = destination(
            schema(&[("title", true)]),
            policy(
                AppPersonalAgentAccess::ApprovedProjection,
                AppDataClassification::Personal,
                AppModelProcessing::RemoteAllowed,
                AppExternalEgress::Denied,
            ),
        );
        let transfer = broker_action_result_to_destination(
            &auth,
            &source,
            &source_policy,
            &schema(&[("name", true)]),
            1,
            &destination,
            vec![AppValueMappingOperation::Select {
                source: AppFieldPath::parse("name").unwrap(),
                target: AppFieldPath::parse("title").unwrap(),
            }],
            AppReference::parse("transfer:joined-labels").unwrap(),
            time(5),
        )
        .unwrap();

        assert_eq!(
            transfer.admission.effective_policy().classification_floor,
            AppDataClassification::Secret
        );
        assert_eq!(
            transfer.admission.effective_policy().model_processing,
            AppModelProcessing::LocalOnly
        );
    }

    #[test]
    fn action_result_can_flow_without_disclosure_to_a_remote_model() {
        let auth = authenticated_scope("anonymous", "default");
        let source = envelope(
            &auth,
            serde_json::json!({"name": "Asha", "note": "mentor"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let source_run_ref = AppReference::parse("run:app-action:task_app_source").unwrap();
        let source_fields = vec![
            AppFieldPath::parse("name").unwrap(),
            AppFieldPath::parse("note").unwrap(),
        ];
        let destination = destination(
            schema(&[("title", true), ("summary", false)]),
            policy(
                AppPersonalAgentAccess::ApprovedProjection,
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                AppExternalEgress::Denied,
            ),
        );
        let transfer = broker_action_result_to_destination(
            &auth,
            &source,
            &source_policy(&source),
            &schema(&[("name", true), ("note", false)]),
            1,
            &destination,
            mapping(),
            AppReference::parse("transfer:action-result").unwrap(),
            time(5),
        )
        .unwrap()
        .seal_source_action_result(
            AppSourceActionResultFence {
                source_run_ref,
                source_task_binding_digest: AppDigest::blake3(b"task"),
                source_grant_policy_digest: AppDigest::blake3(b"grant-policy"),
                output_revision: AppRevision::new(1).unwrap(),
                result_digest: AppDigest::blake3(b"result"),
                fields: source_fields,
            },
            AppDigest::blake3(b"source-request"),
        )
        .unwrap();

        assert_eq!(transfer.envelope.value["title"], "Asha");
        assert!(transfer.admission.receipt().source_records.is_empty());
        assert!(transfer.admission.receipt().source_action_result.is_some());
    }

    #[test]
    fn empty_record_fields_can_drive_a_constant_mapping() {
        let auth = authenticated_scope("anonymous", "default");
        let projection = AppRecordProjection {
            entity: AppName::parse("person").unwrap(),
            record_id: super::super::models::AppRecordId::parse("record_1").unwrap(),
            record_revision: AppRevision::new(1).unwrap(),
            fields: BTreeMap::new(),
        };
        let identity = serde_json::json!({
            "entity": &projection.entity,
            "record_id": &projection.record_id,
        });
        let mut source = envelope(
            &auth,
            serde_json::json!({}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let influence_field = AppFieldPath::parse("note").unwrap();
        source.source_refs = vec![AppSourceRef {
            kind: AppSourceRefKind::EntityField,
            reference: AppReference::parse(format!(
                "record:{}",
                AppDigest::blake3_canonical_json(&identity)
                    .unwrap()
                    .as_str()
            ))
            .unwrap(),
            revision: Some(projection.record_revision),
            fields: vec![influence_field.clone()],
        }];
        source.handling_labels.provenance_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&source.source_refs).unwrap())
                .unwrap();
        let transfer = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("note", false)]),
            1,
            &destination(
                schema(&[("title", true)]),
                policy(
                    AppPersonalAgentAccess::ApprovedProjection,
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                    AppExternalEgress::Denied,
                ),
            ),
            vec![AppValueMappingOperation::Constant {
                target: AppFieldPath::parse("title").unwrap(),
                value: serde_json::json!("Continue"),
            }],
            AppReference::parse("transfer:empty-record").unwrap(),
            time(5),
        )
        .unwrap()
        .seal_source_record(
            &projection,
            vec![influence_field.clone()],
            AppDigest::blake3(b"source-request"),
        )
        .unwrap();

        let fence = &transfer.admission.receipt().source_records[0];
        assert!(fence.fields.is_empty());
        assert_eq!(fence.policy_influence_fields, vec![influence_field.clone()]);
        assert_eq!(transfer.admission.receipt().accepted_fields.len(), 1);
        assert_eq!(
            transfer.envelope.source_refs[0].fields,
            vec![influence_field]
        );
    }

    #[test]
    fn empty_action_result_fields_can_drive_a_constant_mapping() {
        let auth = authenticated_scope("anonymous", "default");
        let mut source = envelope(
            &auth,
            serde_json::json!({}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        source.source_refs.clear();
        source.handling_labels.provenance_digest =
            AppDigest::blake3_canonical_json(&serde_json::json!([])).unwrap();
        let source_run_ref = AppReference::parse("run:app-action:task_app_empty").unwrap();
        let transfer = broker_action_result_to_destination(
            &auth,
            &source,
            &source_policy(&source),
            &schema(&[("note", false)]),
            1,
            &destination(
                schema(&[("title", true)]),
                policy(
                    AppPersonalAgentAccess::ApprovedProjection,
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                    AppExternalEgress::Denied,
                ),
            ),
            vec![AppValueMappingOperation::Constant {
                target: AppFieldPath::parse("title").unwrap(),
                value: serde_json::json!("Continue"),
            }],
            AppReference::parse("transfer:empty-action-result").unwrap(),
            time(5),
        )
        .unwrap()
        .seal_source_action_result(
            AppSourceActionResultFence {
                source_run_ref,
                source_task_binding_digest: AppDigest::blake3(b"task"),
                source_grant_policy_digest: AppDigest::blake3(b"grant-policy"),
                output_revision: AppRevision::new(1).unwrap(),
                result_digest: AppDigest::blake3(b"result"),
                fields: Vec::new(),
            },
            AppDigest::blake3(b"source-request"),
        )
        .unwrap();

        assert_eq!(transfer.envelope.value["title"], "Continue");
        assert_eq!(transfer.admission.receipt().accepted_fields.len(), 1);
        assert_eq!(transfer.envelope.source_refs.len(), 1);
        assert_eq!(
            transfer.envelope.source_refs[0].kind,
            AppSourceRefKind::ExternalReceipt
        );
    }

    #[test]
    fn action_result_still_requires_source_personal_agent_approval() {
        let auth = authenticated_scope("anonymous", "default");
        let mut source = envelope(
            &auth,
            serde_json::json!({"name": "Asha"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let mut source_policy = source_policy(&source);
        source_policy.personal_agent_access = AppPersonalAgentAccess::Denied;
        source.handling_labels.policy_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&source_policy).unwrap())
                .unwrap();
        let destination = destination(
            schema(&[("title", true)]),
            policy(
                AppPersonalAgentAccess::ApprovedProjection,
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                AppExternalEgress::Denied,
            ),
        );

        let error = broker_action_result_to_destination(
            &auth,
            &source,
            &source_policy,
            &schema(&[("name", true)]),
            1,
            &destination,
            vec![AppValueMappingOperation::Select {
                source: AppFieldPath::parse("name").unwrap(),
                target: AppFieldPath::parse("title").unwrap(),
            }],
            AppReference::parse("transfer:denied-action-result").unwrap(),
            time(5),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AppCompositionError::SourcePersonalAgentDenied
        ));
    }

    #[test]
    fn source_egress_denial_survives_destination_egress_capability() {
        let auth = authenticated_scope("anonymous", "default");
        let source = envelope(
            &auth,
            serde_json::json!({"name": "Asha", "note": "mentor"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let mut dest = policy(
            AppPersonalAgentAccess::ApprovedProjection,
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            AppExternalEgress::ApprovedDestinations,
        );
        dest.approved_destinations = vec![AppReference::parse("destination:crm").unwrap()];
        let destination = destination(schema(&[("title", false), ("summary", false)]), dest);

        let transfer = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("name", true), ("note", false)]),
            1,
            &destination,
            mapping(),
            AppReference::parse("transfer:egress").unwrap(),
            time(5),
        )
        .unwrap();

        assert_eq!(
            transfer.admission.effective_policy().external_egress,
            AppExternalEgress::Denied
        );
        assert!(transfer
            .admission
            .effective_policy()
            .approved_destinations
            .is_empty());
    }

    #[test]
    fn destination_without_personal_agent_access_fails_closed() {
        let auth = authenticated_scope("anonymous", "default");
        let source = envelope(
            &auth,
            serde_json::json!({"name": "Asha", "note": "mentor"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let dest = policy(
            AppPersonalAgentAccess::Denied,
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
            AppExternalEgress::Denied,
        );
        let destination = destination(schema(&[("title", true), ("summary", false)]), dest);
        let error = broker_source_to_destination(
            &auth,
            &audience(&auth, AppAgentProcessingClass::Deterministic),
            &source,
            &source_policy(&source),
            &schema(&[("name", true), ("note", false)]),
            1,
            &destination,
            mapping(),
            AppReference::parse("transfer:3").unwrap(),
            time(5),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            AppCompositionError::DestinationPersonalAgentDenied
        ));
    }

    #[test]
    fn delegated_and_outward_audiences_cannot_compose() {
        assert!(!personal_agent_composition_is_direct(
            None,
            None,
            Some("chat")
        ));
        assert!(!personal_agent_composition_is_direct(
            Some("chat_inline"),
            None,
            None
        ));
        assert!(!personal_agent_composition_is_direct(
            Some("delegated"),
            None,
            Some("chat")
        ));
        assert!(!personal_agent_composition_is_direct(
            Some("direct"),
            Some("worker"),
            Some("chat")
        ));
        assert!(!personal_agent_composition_is_direct(
            Some("direct"),
            None,
            Some("delegation")
        ));
        assert!(!personal_agent_composition_is_direct(
            Some("direct"),
            None,
            Some("handover")
        ));
        assert!(!personal_agent_composition_is_direct(
            Some("direct"),
            None,
            Some("public_envoy")
        ));
        assert!(personal_agent_composition_is_direct(
            Some("chat_inline"),
            None,
            Some("chat")
        ));
    }
}
