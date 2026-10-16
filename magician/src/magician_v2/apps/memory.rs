//! Source-linked app-memory contracts.
//!
//! App records remain authoritative. This dormant Phase-0 module defines the
//! candidate, lifecycle and current-source fence that future memory writers,
//! retrieval and prompt assembly must share. It does not write memory, query an
//! app store, or enable an app route.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::{AuthenticatedAppScope, ResolvedAppAuthority},
    lifecycle::AppInstallationStatus,
    models::{
        validate_bounded, validate_nonempty_bounded, AppContractError, AppContractLimits,
        AppDigest, AppFieldPath, AppHandlingLabels, AppInstallationId, AppModelProcessing, AppName,
        AppProtocolVersion, AppRecordId, AppReference, AppRevision, AppScopeBindingRef,
        AppSourceRef, AppSourceRefKind, ValidateAppContract,
    },
    policy::{AppJoinedContent, ResolvedAppDataHandlingPolicy, ResolvedAppHandlingLabels},
    records::{
        validate_authority_ceiling_parts, AppInstallation, AppMemoryPromotion, AppRecordRevision,
        AppScope,
    },
};

const STORE_RESOLVER_SCOPE_BINDING: &str = "app_memory_store";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMemoryTierScope {
    Agent {
        agent_id: AppReference,
    },
    AgentGoal {
        agent_id: AppReference,
        goal_id: AppReference,
    },
    User,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemorySemanticDestination {
    TaskProgress,
    Entities,
    Knowledge,
    Archive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemorySourceRef {
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub grant_revision: AppRevision,
    pub schema_revision: AppRevision,
    pub entity_name: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
    pub selected_fields: Vec<AppFieldPath>,
    pub canonical_source_ref: AppSourceRef,
    pub handling_labels: AppHandlingLabels,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryCandidateStatus {
    Proposed,
    Accepted,
    Rejected,
    Stale,
    Tombstoned,
}

/// Durable source-linked candidate. This record contains no bearer authority.
/// Retrieval still requires a fresh [`AppMemoryEligibilityFence`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryCandidate {
    pub protocol_version: AppProtocolVersion,
    pub candidate_id: AppReference,
    pub candidate_revision: AppRevision,
    pub candidate_fingerprint: AppDigest,
    pub scope: AppScope,
    pub intended_tier_scope: AppMemoryTierScope,
    pub semantic_destination: AppMemorySemanticDestination,
    pub source_refs: Vec<AppMemorySourceRef>,
    pub derived_claim_or_summary: String,
    pub claim_content_digest: AppDigest,
    pub handling_labels: AppHandlingLabels,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_and_provenance_refs: Vec<AppSourceRef>,
    pub status: AppMemoryCandidateStatus,
    pub proposed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryCandidateCommand {
    Accept,
    Reject,
    MarkStale,
    Tombstone,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppMemoryCandidateLifecycleError {
    #[error("invalid app-memory candidate: {0}")]
    InvalidCandidate(String),
    #[error("invalid app-memory candidate transition from {from:?} via {command:?}")]
    InvalidTransition {
        from: AppMemoryCandidateStatus,
        command: AppMemoryCandidateCommand,
    },
    #[error("app-memory candidate revision is exhausted")]
    RevisionExhausted,
    #[error("app-memory candidate transition time must be later than updated_at")]
    NonMonotonicTime,
}

impl AppMemoryCandidate {
    pub fn apply(
        &self,
        command: AppMemoryCandidateCommand,
        at: DateTime<Utc>,
    ) -> Result<Self, AppMemoryCandidateLifecycleError> {
        self.validate_app_contract(&AppContractLimits::default())
            .map_err(|error| {
                AppMemoryCandidateLifecycleError::InvalidCandidate(error.to_string())
            })?;
        if at <= self.updated_at {
            return Err(AppMemoryCandidateLifecycleError::NonMonotonicTime);
        }
        let status = match (self.status, command) {
            (AppMemoryCandidateStatus::Proposed, AppMemoryCandidateCommand::Accept) => {
                AppMemoryCandidateStatus::Accepted
            },
            (AppMemoryCandidateStatus::Proposed, AppMemoryCandidateCommand::Reject) => {
                AppMemoryCandidateStatus::Rejected
            },
            (
                AppMemoryCandidateStatus::Proposed | AppMemoryCandidateStatus::Accepted,
                AppMemoryCandidateCommand::MarkStale,
            ) => AppMemoryCandidateStatus::Stale,
            (
                AppMemoryCandidateStatus::Proposed
                | AppMemoryCandidateStatus::Accepted
                | AppMemoryCandidateStatus::Stale,
                AppMemoryCandidateCommand::Tombstone,
            ) => AppMemoryCandidateStatus::Tombstoned,
            (from, command) => {
                return Err(AppMemoryCandidateLifecycleError::InvalidTransition { from, command });
            },
        };
        let next_revision = self
            .candidate_revision
            .get()
            .checked_add(1)
            .ok_or(AppMemoryCandidateLifecycleError::RevisionExhausted)?;
        let mut next = self.clone();
        next.candidate_revision = AppRevision::new(next_revision)
            .map_err(|_| AppMemoryCandidateLifecycleError::RevisionExhausted)?;
        next.status = status;
        next.updated_at = at;
        next.validate_app_contract(&AppContractLimits::default())
            .map_err(|error| {
                AppMemoryCandidateLifecycleError::InvalidCandidate(error.to_string())
            })?;
        Ok(next)
    }

    pub fn seal_fingerprint(&mut self) -> Result<(), AppMemoryEligibilityError> {
        self.candidate_fingerprint = candidate_fingerprint(self)?;
        Ok(())
    }
}

impl ValidateAppContract for AppMemoryCandidate {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_nonempty_bounded(
            "memory.source_refs",
            self.source_refs.len(),
            limits.max_collection_items(),
        )?;
        validate_bounded(
            "memory.evidence_and_provenance_refs",
            self.evidence_and_provenance_refs.len(),
            limits.max_collection_items(),
        )?;
        if self.derived_claim_or_summary.is_empty()
            || self.derived_claim_or_summary.len() > limits.max_value_bytes()
        {
            return Err(AppContractError::invalid(
                "memory.derived_claim_or_summary",
                "must be non-empty and remain within the value byte ceiling",
            ));
        }
        if self.updated_at < self.proposed_at {
            return Err(AppContractError::invalid(
                "memory.updated_at",
                "cannot precede proposed_at",
            ));
        }
        if self.handling_labels.model_processing == AppModelProcessing::None {
            return Err(AppContractError::invalid(
                "memory.handling_labels.model_processing",
                "model_processing none cannot enter memory promotion",
            ));
        }

        let mut source_identities = HashSet::with_capacity(self.source_refs.len());
        let mut aggregate_source_fields = 0usize;
        if !self
            .source_refs
            .windows(2)
            .all(|sources| compare_memory_sources(&sources[0], &sources[1]) == Ordering::Less)
        {
            return Err(AppContractError::invalid(
                "memory.source_refs",
                "must be strictly sorted by canonical source identity",
            ));
        }
        for source in &self.source_refs {
            validate_memory_source_ref(source, limits)?;
            aggregate_source_fields =
                aggregate_source_fields.saturating_add(source.selected_fields.len());
            if aggregate_source_fields > limits.max_collection_items() {
                return Err(AppContractError::invalid(
                    "memory.source_refs.selected_fields",
                    "exceeds the aggregate selected-field ceiling",
                ));
            }
            if !source_identities.insert(memory_source_identity(source)) {
                return Err(AppContractError::invalid(
                    "memory.source_refs",
                    "contains a duplicate source identity",
                ));
            }
            if !self
                .evidence_and_provenance_refs
                .contains(&source.canonical_source_ref)
            {
                return Err(AppContractError::invalid(
                    "memory.evidence_and_provenance_refs",
                    "does not contain every canonical app-record source",
                ));
            }
        }
        validate_provenance_refs(&self.evidence_and_provenance_refs, limits)?;

        let source_classification = self
            .source_refs
            .iter()
            .map(|source| source.handling_labels.classification)
            .max()
            .expect("source_refs is non-empty");
        let source_model_processing = self
            .source_refs
            .iter()
            .map(|source| source.handling_labels.model_processing)
            .min()
            .expect("source_refs is non-empty");
        if self.handling_labels.classification < source_classification
            || self.handling_labels.model_processing > source_model_processing
        {
            return Err(AppContractError::invalid(
                "memory.handling_labels",
                "cannot lower source classification or broaden source model processing",
            ));
        }

        let claim_digest = digest_claim(&self.derived_claim_or_summary)?;
        if claim_digest != self.claim_content_digest {
            return Err(AppContractError::invalid(
                "memory.claim_content_digest",
                "does not match the canonical claim string",
            ));
        }
        let fingerprint = candidate_fingerprint(self).map_err(|error| {
            AppContractError::invalid("memory.candidate_fingerprint", error.to_string())
        })?;
        if fingerprint != self.candidate_fingerprint {
            return Err(AppContractError::invalid(
                "memory.candidate_fingerprint",
                "does not match the canonical candidate material",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryResolvedSourceState {
    Eligible,
    Dormant,
    Deleted,
    Purged,
    PromotionRevoked,
    ModelProcessingDenied,
}

/// Current source evidence minted by the trusted app-store/policy resolver.
/// It is intentionally not deserializable and not cloneable: a package,
/// client, model or cached durable row cannot assert current eligibility.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedAppMemorySource {
    scope: AppScope,
    scope_binding_ref: AppScopeBindingRef,
    source: AppMemorySourceRef,
    installation_generation: u64,
    eligibility_revision: AppRevision,
    state: AppMemoryResolvedSourceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_installation_status: Option<AppInstallationStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_policy: Option<ResolvedAppDataHandlingPolicy>,
    eligibility_digest: AppDigest,
    resolved_at: DateTime<Utc>,
}

impl ResolvedAppMemorySource {
    /// Mint proposal-time evidence from a record projection that the canonical
    /// entity store has just revalidated in one current read snapshot. This is
    /// deliberately crate-private and accepts the store-owned tuple rather
    /// than caller JSON; a projection handle remains evidence, never bearer
    /// authority.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_revalidated_projection(
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        projection: &super::models::AppRecordProjection,
        canonical_source_ref: AppSourceRef,
        policy: super::records::AppDataHandlingPolicy,
        resolved_at: DateTime<Utc>,
    ) -> Result<Self, AppMemoryEligibilityError> {
        authenticated_scope
            .ensure_live_at(&resolved_at)
            .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
        if installation_generation == 0
            || canonical_source_ref.kind != AppSourceRefKind::EntityField
            || canonical_source_ref.revision != Some(projection.record_revision)
        {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "revalidated projection identity is inconsistent".to_owned(),
            ));
        }
        let selected_fields = projection.fields.keys().cloned().collect::<Vec<_>>();
        if selected_fields.is_empty() || canonical_source_ref.fields != selected_fields {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "revalidated projection fields are inconsistent".to_owned(),
            ));
        }
        let policy_value = serde_json::to_value(&policy)
            .map_err(|error| AppMemoryEligibilityError::DigestEncoding(error.to_string()))?;
        let policy_digest = AppDigest::blake3_canonical_json(&policy_value)
            .map_err(|error| AppMemoryEligibilityError::DigestEncoding(error.to_string()))?;
        let source_value = serde_json::to_value(&canonical_source_ref)
            .map_err(|error| AppMemoryEligibilityError::DigestEncoding(error.to_string()))?;
        let provenance_digest = AppDigest::blake3_canonical_json(&source_value)
            .map_err(|error| AppMemoryEligibilityError::DigestEncoding(error.to_string()))?;
        let handling_labels = AppHandlingLabels {
            classification: policy.classification_floor,
            model_processing: policy.model_processing,
            policy_digest: policy_digest.clone(),
            provenance_digest,
        };
        let source = AppMemorySourceRef {
            installation_id,
            package_revision_ref,
            grant_revision,
            schema_revision,
            entity_name: projection.entity.clone(),
            record_id: projection.record_id.clone(),
            record_revision: projection.record_revision,
            selected_fields,
            canonical_source_ref,
            handling_labels,
        };
        let state = if policy.memory_promotion != AppMemoryPromotion::CandidateAllowed {
            AppMemoryResolvedSourceState::PromotionRevoked
        } else if policy.model_processing == AppModelProcessing::None {
            AppMemoryResolvedSourceState::ModelProcessingDenied
        } else {
            AppMemoryResolvedSourceState::Eligible
        };
        let eligibility_revision = AppRevision::new(installation_generation.max(1))
            .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
        let effective_policy = ResolvedAppDataHandlingPolicy::from_trusted_store_policy(
            policy,
            policy_digest,
            resolved_at,
        );
        let eligibility_digest = source_eligibility_digest(
            authenticated_scope.scope(),
            authenticated_scope.scope_binding_ref(),
            &source,
            installation_generation,
            eligibility_revision,
            state,
            None,
            Some(effective_policy.policy()),
            &resolved_at,
        )?;
        Ok(Self {
            scope: authenticated_scope.scope().clone(),
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            source,
            installation_generation,
            eligibility_revision,
            state,
            unavailable_installation_status: None,
            effective_policy: Some(effective_policy),
            eligibility_digest,
            resolved_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_enabled_record(
        authenticated_scope: &AuthenticatedAppScope,
        authority: &ResolvedAppAuthority,
        record: &AppRecordRevision,
        source_reference: AppReference,
        mut selected_fields: Vec<AppFieldPath>,
        handling_labels: ResolvedAppHandlingLabels,
        eligibility_revision: AppRevision,
        resolved_at: DateTime<Utc>,
    ) -> Result<Self, AppMemoryEligibilityError> {
        authenticated_scope
            .ensure_live_at(&resolved_at)
            .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
        if authority.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || authority.actor_ref != *authenticated_scope.actor_ref()
            || authority.session_ref != *authenticated_scope.session_ref()
            || authority.authentication != authenticated_scope.authentication()
            || authority.authentication_revision != authenticated_scope.authentication_revision()
            || authority.installation_id != record.installation_id
            || authority.schema_revision != record.schema_revision
            || authority.installation_generation == 0
            || authority.resolved_at != resolved_at
        {
            return Err(AppMemoryEligibilityError::InvalidAuthority(
                "authority, source record and authenticated scope are not one current app snapshot"
                    .to_owned(),
            ));
        }
        validate_authority_ceiling_parts(
            authority.effective_tools.len(),
            authority.effective_context_reads.len(),
            &authority.effective_data_handling_policy,
            &authority.effective_background_execution,
            &authority.effective_network_policy,
            &authority.effective_resources,
            &AppContractLimits::default(),
        )
        .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
        record
            .validate_app_contract(&AppContractLimits::default())
            .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;
        validate_nonempty_bounded(
            "memory.source.selected_fields",
            selected_fields.len(),
            AppContractLimits::default().max_collection_items(),
        )
        .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;
        selected_fields.sort();
        selected_fields.dedup();
        if selected_fields.is_empty() {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "memory source must select at least one field".to_owned(),
            ));
        }
        if selected_fields
            .iter()
            .any(|field| !field_path_exists(&record.payload, field))
        {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "memory source selects a field absent from the current record revision".to_owned(),
            ));
        }

        let effective_policy = ResolvedAppDataHandlingPolicy::from_resolved_authority(authority);
        let policy = effective_policy.policy();
        let labels = handling_labels.labels();
        if labels.classification < policy.classification_floor
            || labels.model_processing > policy.model_processing
        {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "resolved source labels broaden the effective app policy".to_owned(),
            ));
        }
        let state = if record.deleted_at.is_some() {
            AppMemoryResolvedSourceState::Deleted
        } else if policy.memory_promotion != AppMemoryPromotion::CandidateAllowed {
            AppMemoryResolvedSourceState::PromotionRevoked
        } else if policy.model_processing == AppModelProcessing::None
            || labels.model_processing == AppModelProcessing::None
        {
            AppMemoryResolvedSourceState::ModelProcessingDenied
        } else {
            AppMemoryResolvedSourceState::Eligible
        };
        let canonical_source_ref = AppSourceRef {
            kind: AppSourceRefKind::EntityField,
            reference: source_reference,
            revision: Some(record.record_revision),
            fields: selected_fields.clone(),
        };
        let source = AppMemorySourceRef {
            installation_id: record.installation_id.clone(),
            package_revision_ref: authority.package_revision_ref.clone(),
            grant_revision: authority.grant_revision,
            schema_revision: record.schema_revision,
            entity_name: record.entity_name.clone(),
            record_id: record.record_id.clone(),
            record_revision: record.record_revision,
            selected_fields,
            canonical_source_ref,
            handling_labels: labels.clone(),
        };
        let eligibility_digest = source_eligibility_digest(
            authenticated_scope.scope(),
            authenticated_scope.scope_binding_ref(),
            &source,
            authority.installation_generation,
            eligibility_revision,
            state,
            None,
            Some(effective_policy.policy()),
            &resolved_at,
        )?;
        Ok(Self {
            scope: authenticated_scope.scope().clone(),
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            source,
            installation_generation: authority.installation_generation,
            eligibility_revision,
            state,
            unavailable_installation_status: None,
            effective_policy: Some(effective_policy),
            eligibility_digest,
            resolved_at,
        })
    }

    /// Mint negative current-state evidence for an installation that cannot
    /// supply live memory. Enabled installations must use `from_enabled_record`
    /// so current record and policy revisions cannot be skipped.
    pub fn from_unavailable_installation(
        authenticated_scope: &AuthenticatedAppScope,
        installation: &AppInstallation,
        prior_source: AppMemorySourceRef,
        eligibility_revision: AppRevision,
        resolved_at: DateTime<Utc>,
    ) -> Result<Self, AppMemoryEligibilityError> {
        authenticated_scope
            .ensure_live_at(&resolved_at)
            .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
        installation
            .validate_app_contract(&AppContractLimits::default())
            .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;
        if &installation.scope != authenticated_scope.scope()
            || installation.installation_id != prior_source.installation_id
            || installation.package_revision_ref != prior_source.package_revision_ref
        {
            return Err(AppMemoryEligibilityError::CrossScopeEvidence);
        }
        validate_memory_source_ref(&prior_source, &AppContractLimits::default())
            .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;
        let state = match installation.lifecycle.status {
            AppInstallationStatus::Enabled => {
                return Err(AppMemoryEligibilityError::InvalidSource(
                    "enabled installations require current record and policy evidence".to_owned(),
                ));
            },
            AppInstallationStatus::Purged => AppMemoryResolvedSourceState::Purged,
            AppInstallationStatus::ReadyForReview
            | AppInstallationStatus::Disabled
            | AppInstallationStatus::UpdatePending
            | AppInstallationStatus::Quarantined
            | AppInstallationStatus::UninstalledRetained => AppMemoryResolvedSourceState::Dormant,
        };
        let status = Some(installation.lifecycle.status);
        let eligibility_digest = source_eligibility_digest(
            authenticated_scope.scope(),
            authenticated_scope.scope_binding_ref(),
            &prior_source,
            installation.lifecycle.generation,
            eligibility_revision,
            state,
            status,
            None,
            &resolved_at,
        )?;
        Ok(Self {
            scope: authenticated_scope.scope().clone(),
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            source: prior_source,
            installation_generation: installation.lifecycle.generation,
            eligibility_revision,
            state,
            unavailable_installation_status: status,
            effective_policy: None,
            eligibility_digest,
            resolved_at,
        })
    }

    pub fn state(&self) -> AppMemoryResolvedSourceState {
        self.state
    }

    pub fn source(&self) -> &AppMemorySourceRef {
        &self.source
    }

    /// Mint current-source evidence from the app store's own transaction or
    /// prompt-time read. This is the Phase 5B resolver seam: only the trusted
    /// store may construct it, and the value remains non-deserializable.
    pub fn from_trusted_store_snapshot(
        scope: &AppScope,
        source: AppMemorySourceRef,
        installation_generation: u64,
        eligibility_revision: AppRevision,
        state: AppMemoryResolvedSourceState,
        unavailable_installation_status: Option<AppInstallationStatus>,
        effective_policy: Option<ResolvedAppDataHandlingPolicy>,
        resolved_at: DateTime<Utc>,
    ) -> Result<Self, AppMemoryEligibilityError> {
        if installation_generation == 0 {
            return Err(AppMemoryEligibilityError::InvalidAuthority(
                "store snapshot requires a positive installation generation".to_owned(),
            ));
        }
        validate_memory_source_ref(&source, &AppContractLimits::default())
            .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;
        match state {
            AppMemoryResolvedSourceState::Eligible => {
                if unavailable_installation_status.is_some() || effective_policy.is_none() {
                    return Err(AppMemoryEligibilityError::InvalidSource(
                        "eligible store evidence requires current policy and no unavailable status"
                            .to_owned(),
                    ));
                }
            },
            AppMemoryResolvedSourceState::Dormant | AppMemoryResolvedSourceState::Purged => {
                if unavailable_installation_status.is_none() {
                    return Err(AppMemoryEligibilityError::InvalidSource(
                        "unavailable store evidence must carry the installation status".to_owned(),
                    ));
                }
            },
            AppMemoryResolvedSourceState::Deleted
            | AppMemoryResolvedSourceState::PromotionRevoked
            | AppMemoryResolvedSourceState::ModelProcessingDenied => {},
        }
        let scope_binding_ref = AppScopeBindingRef::parse(STORE_RESOLVER_SCOPE_BINDING)
            .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
        let eligibility_digest = source_eligibility_digest(
            scope,
            &scope_binding_ref,
            &source,
            installation_generation,
            eligibility_revision,
            state,
            unavailable_installation_status,
            effective_policy
                .as_ref()
                .map(ResolvedAppDataHandlingPolicy::policy),
            &resolved_at,
        )?;
        Ok(Self {
            scope: scope.clone(),
            scope_binding_ref,
            source,
            installation_generation,
            eligibility_revision,
            state,
            unavailable_installation_status,
            effective_policy,
            eligibility_digest,
            resolved_at,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMemorySourceRemovalKind {
    Deleted,
    Purged,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppMemoryEligibilityError {
    #[error("invalid app-memory candidate: {0}")]
    InvalidCandidate(String),
    #[error("invalid app-memory source: {0}")]
    InvalidSource(String),
    #[error("invalid app-memory authority: {0}")]
    InvalidAuthority(String),
    #[error("app-memory candidate is not accepted")]
    CandidateNotAccepted,
    #[error("current source evidence is unavailable")]
    SourceEvidenceUnavailable,
    #[error("current source evidence is duplicated")]
    DuplicateSourceEvidence,
    #[error("current source evidence crosses scope")]
    CrossScopeEvidence,
    #[error("source evidence was not resolved at the current decision boundary")]
    EvidenceNotCurrent,
    #[error("app-memory eligibility resolution precedes the accepted candidate revision")]
    ResolutionPrecedesCandidate,
    #[error("app-memory source revision or authority identity changed")]
    SourceStale,
    #[error("app-memory source policy or handling labels changed")]
    SourcePolicyChanged,
    #[error("app-memory source is dormant in installation state {status:?}")]
    SourceDormant { status: AppInstallationStatus },
    #[error("app-memory source was {kind:?}; source_count={source_count}")]
    SourceRemoved {
        kind: AppMemorySourceRemovalKind,
        source_count: usize,
    },
    #[error("app-memory promotion authority was revoked")]
    PromotionRevoked,
    #[error("app-memory source no longer permits model processing")]
    ModelProcessingDenied,
    #[error("failed to compute app-memory identity: {0}")]
    DigestEncoding(String),
}

impl AppMemoryEligibilityError {
    /// Sole-source deletion/purge must tombstone the candidate. Multi-source
    /// removal must create a newly reviewed candidate from independently
    /// supported remaining sources; silently editing the accepted claim is not
    /// safe.
    pub fn required_candidate_command(&self) -> Option<AppMemoryCandidateCommand> {
        match self {
            Self::SourceRemoved {
                source_count: 1, ..
            } => Some(AppMemoryCandidateCommand::Tombstone),
            Self::SourceRemoved { .. } | Self::SourceStale | Self::SourcePolicyChanged => {
                Some(AppMemoryCandidateCommand::MarkStale)
            },
            Self::PromotionRevoked | Self::ModelProcessingDenied => {
                Some(AppMemoryCandidateCommand::MarkStale)
            },
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryEligibilitySourceFence {
    source: AppMemorySourceRef,
    installation_generation: u64,
    eligibility_revision: AppRevision,
    eligibility_digest: AppDigest,
}

impl AppMemoryEligibilitySourceFence {
    pub fn source(&self) -> &AppMemorySourceRef {
        &self.source
    }

    pub fn installation_generation(&self) -> u64 {
        self.installation_generation
    }

    pub fn eligibility_revision(&self) -> AppRevision {
        self.eligibility_revision
    }

    pub fn eligibility_digest(&self) -> &AppDigest {
        &self.eligibility_digest
    }
}

/// Ephemeral, non-deserializable proof that every exact app source was current
/// when the candidate was admitted for retrieval. This is identity evidence,
/// not bearer authority; the adopting store must recheck its eligibility
/// revisions at the load-bearing read/prompt boundary.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryEligibilityFence {
    candidate_id: AppReference,
    candidate_revision: AppRevision,
    candidate_fingerprint: AppDigest,
    scope: AppScope,
    sources: Vec<AppMemoryEligibilitySourceFence>,
    resolved_at: DateTime<Utc>,
    fence_digest: AppDigest,
}

impl AppMemoryEligibilityFence {
    pub fn candidate_id(&self) -> &AppReference {
        &self.candidate_id
    }

    pub fn candidate_revision(&self) -> AppRevision {
        self.candidate_revision
    }

    pub fn fence_digest(&self) -> &AppDigest {
        &self.fence_digest
    }

    pub fn scope(&self) -> &AppScope {
        &self.scope
    }

    pub fn sources(&self) -> &[AppMemoryEligibilitySourceFence] {
        &self.sources
    }

    pub fn resolved_at(&self) -> &DateTime<Utc> {
        &self.resolved_at
    }
}

#[allow(clippy::too_many_arguments)]
pub fn propose_app_memory_candidate(
    authenticated_scope: &AuthenticatedAppScope,
    candidate_id: AppReference,
    intended_tier_scope: AppMemoryTierScope,
    semantic_destination: AppMemorySemanticDestination,
    joined_content: &AppJoinedContent,
    resolved_sources: &[&ResolvedAppMemorySource],
    proposed_at: DateTime<Utc>,
    limits: &AppContractLimits,
) -> Result<AppMemoryCandidate, AppMemoryEligibilityError> {
    authenticated_scope
        .ensure_live_at(&proposed_at)
        .map_err(|error| AppMemoryEligibilityError::InvalidAuthority(error.to_string()))?;
    if joined_content.scope_binding_ref() != authenticated_scope.scope_binding_ref() {
        return Err(AppMemoryEligibilityError::CrossScopeEvidence);
    }
    validate_nonempty_bounded(
        "memory.resolved_sources",
        resolved_sources.len(),
        limits.max_collection_items(),
    )
    .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;
    let claim = joined_content
        .value()
        .as_str()
        .filter(|claim| !claim.is_empty())
        .ok_or_else(|| {
            AppMemoryEligibilityError::InvalidSource(
                "memory candidate joined content must be a non-empty string".to_owned(),
            )
        })?;
    if claim.len() > limits.max_value_bytes() {
        return Err(AppMemoryEligibilityError::InvalidSource(
            "memory candidate claim exceeds the value byte ceiling".to_owned(),
        ));
    }
    if joined_content.handling_labels().model_processing == AppModelProcessing::None {
        return Err(AppMemoryEligibilityError::ModelProcessingDenied);
    }

    let mut sources = Vec::with_capacity(resolved_sources.len());
    let mut identities = HashSet::with_capacity(resolved_sources.len());
    for source in resolved_sources {
        if &source.scope != authenticated_scope.scope()
            || &source.scope_binding_ref != authenticated_scope.scope_binding_ref()
        {
            return Err(AppMemoryEligibilityError::CrossScopeEvidence);
        }
        if source.resolved_at != proposed_at {
            return Err(AppMemoryEligibilityError::EvidenceNotCurrent);
        }
        match source.state {
            AppMemoryResolvedSourceState::Eligible => {},
            AppMemoryResolvedSourceState::PromotionRevoked => {
                return Err(AppMemoryEligibilityError::PromotionRevoked);
            },
            AppMemoryResolvedSourceState::ModelProcessingDenied => {
                return Err(AppMemoryEligibilityError::ModelProcessingDenied);
            },
            AppMemoryResolvedSourceState::Dormant => {
                return Err(AppMemoryEligibilityError::SourceDormant {
                    status: source
                        .unavailable_installation_status
                        .unwrap_or(AppInstallationStatus::Disabled),
                });
            },
            AppMemoryResolvedSourceState::Deleted => {
                return Err(AppMemoryEligibilityError::SourceRemoved {
                    kind: AppMemorySourceRemovalKind::Deleted,
                    source_count: resolved_sources.len(),
                });
            },
            AppMemoryResolvedSourceState::Purged => {
                return Err(AppMemoryEligibilityError::SourceRemoved {
                    kind: AppMemorySourceRemovalKind::Purged,
                    source_count: resolved_sources.len(),
                });
            },
        }
        if !identities.insert(memory_source_identity(&source.source)) {
            return Err(AppMemoryEligibilityError::DuplicateSourceEvidence);
        }
        if !joined_content
            .source_refs()
            .contains(&source.source.canonical_source_ref)
        {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "joined content does not retain the exact app-record source".to_owned(),
            ));
        }
        sources.push(source.source.clone());
    }
    for source_ref in joined_content.source_refs().iter().filter(|source| {
        matches!(
            source.kind,
            AppSourceRefKind::EntityRecord | AppSourceRefKind::EntityField
        )
    }) {
        if !sources
            .iter()
            .any(|source| &source.canonical_source_ref == source_ref)
        {
            return Err(AppMemoryEligibilityError::InvalidSource(
                "joined content contains an app-record source without current eligibility evidence"
                    .to_owned(),
            ));
        }
    }
    sources.sort_by(compare_memory_sources);
    let source_classification = sources
        .iter()
        .map(|source| source.handling_labels.classification)
        .max()
        .expect("resolved_sources is non-empty");
    let source_model_processing = sources
        .iter()
        .map(|source| source.handling_labels.model_processing)
        .min()
        .expect("resolved_sources is non-empty");
    if joined_content.handling_labels().classification < source_classification
        || joined_content.handling_labels().model_processing > source_model_processing
    {
        return Err(AppMemoryEligibilityError::InvalidSource(
            "joined memory content broadens source handling labels".to_owned(),
        ));
    }

    let mut evidence_and_provenance_refs = joined_content.source_refs().to_vec();
    evidence_and_provenance_refs.sort_by(compare_source_refs);
    evidence_and_provenance_refs.dedup();
    let claim_content_digest = joined_content.content_digest().clone();
    let mut candidate = AppMemoryCandidate {
        protocol_version: joined_content.protocol_version(),
        candidate_id,
        candidate_revision: AppRevision::new(1)
            .map_err(|error| AppMemoryEligibilityError::InvalidCandidate(error.to_string()))?,
        candidate_fingerprint: AppDigest::blake3(b"pending-memory-candidate"),
        scope: authenticated_scope.scope().clone(),
        intended_tier_scope,
        semantic_destination,
        source_refs: sources,
        derived_claim_or_summary: claim.to_owned(),
        claim_content_digest,
        handling_labels: joined_content.handling_labels().clone(),
        evidence_and_provenance_refs,
        status: AppMemoryCandidateStatus::Proposed,
        proposed_at,
        updated_at: proposed_at,
    };
    candidate.seal_fingerprint()?;
    candidate
        .validate_app_contract(limits)
        .map_err(|error| AppMemoryEligibilityError::InvalidCandidate(error.to_string()))?;
    Ok(candidate)
}

pub fn resolve_app_memory_eligibility(
    candidate: &AppMemoryCandidate,
    current_sources: &[&ResolvedAppMemorySource],
    resolved_at: DateTime<Utc>,
    limits: &AppContractLimits,
) -> Result<AppMemoryEligibilityFence, AppMemoryEligibilityError> {
    candidate
        .validate_app_contract(limits)
        .map_err(|error| AppMemoryEligibilityError::InvalidCandidate(error.to_string()))?;
    if candidate.status != AppMemoryCandidateStatus::Accepted {
        return Err(AppMemoryEligibilityError::CandidateNotAccepted);
    }
    if resolved_at < candidate.updated_at {
        return Err(AppMemoryEligibilityError::ResolutionPrecedesCandidate);
    }
    validate_bounded(
        "memory.current_sources",
        current_sources.len(),
        limits.max_collection_items(),
    )
    .map_err(|error| AppMemoryEligibilityError::InvalidSource(error.to_string()))?;

    let mut by_identity = BTreeMap::new();
    for evidence in current_sources {
        if evidence.resolved_at != resolved_at {
            return Err(AppMemoryEligibilityError::EvidenceNotCurrent);
        }
        let key = memory_source_identity(&evidence.source);
        if by_identity.insert(key, *evidence).is_some() {
            return Err(AppMemoryEligibilityError::DuplicateSourceEvidence);
        }
    }

    let mut fence_sources = Vec::with_capacity(candidate.source_refs.len());
    for expected in &candidate.source_refs {
        let evidence = by_identity
            .remove(&memory_source_identity(expected))
            .ok_or(AppMemoryEligibilityError::SourceEvidenceUnavailable)?;
        if &evidence.scope != &candidate.scope {
            return Err(AppMemoryEligibilityError::CrossScopeEvidence);
        }
        match evidence.state {
            AppMemoryResolvedSourceState::Eligible => {},
            AppMemoryResolvedSourceState::Dormant => {
                return Err(AppMemoryEligibilityError::SourceDormant {
                    status: evidence
                        .unavailable_installation_status
                        .unwrap_or(AppInstallationStatus::Disabled),
                });
            },
            AppMemoryResolvedSourceState::Deleted => {
                return Err(AppMemoryEligibilityError::SourceRemoved {
                    kind: AppMemorySourceRemovalKind::Deleted,
                    source_count: candidate.source_refs.len(),
                });
            },
            AppMemoryResolvedSourceState::Purged => {
                return Err(AppMemoryEligibilityError::SourceRemoved {
                    kind: AppMemorySourceRemovalKind::Purged,
                    source_count: candidate.source_refs.len(),
                });
            },
            AppMemoryResolvedSourceState::PromotionRevoked => {
                return Err(AppMemoryEligibilityError::PromotionRevoked);
            },
            AppMemoryResolvedSourceState::ModelProcessingDenied => {
                return Err(AppMemoryEligibilityError::ModelProcessingDenied);
            },
        }
        if evidence.source.record_revision != expected.record_revision
            || evidence.source.schema_revision != expected.schema_revision
            || evidence.source.grant_revision != expected.grant_revision
            || evidence.source.package_revision_ref != expected.package_revision_ref
        {
            return Err(AppMemoryEligibilityError::SourceStale);
        }
        if &evidence.source != expected {
            return Err(AppMemoryEligibilityError::SourcePolicyChanged);
        }
        let effective_policy = evidence
            .effective_policy
            .as_ref()
            .ok_or(AppMemoryEligibilityError::SourceEvidenceUnavailable)?;
        if effective_policy.policy().memory_promotion != AppMemoryPromotion::CandidateAllowed {
            return Err(AppMemoryEligibilityError::PromotionRevoked);
        }
        fence_sources.push(AppMemoryEligibilitySourceFence {
            source: evidence.source.clone(),
            installation_generation: evidence.installation_generation,
            eligibility_revision: evidence.eligibility_revision,
            eligibility_digest: evidence.eligibility_digest.clone(),
        });
    }
    if !by_identity.is_empty() {
        return Err(AppMemoryEligibilityError::InvalidSource(
            "current source evidence contains sources outside the candidate".to_owned(),
        ));
    }
    let fence_digest = memory_fence_digest(candidate, &fence_sources, &resolved_at)?;
    Ok(AppMemoryEligibilityFence {
        candidate_id: candidate.candidate_id.clone(),
        candidate_revision: candidate.candidate_revision,
        candidate_fingerprint: candidate.candidate_fingerprint.clone(),
        scope: candidate.scope.clone(),
        sources: fence_sources,
        resolved_at,
        fence_digest,
    })
}

fn validate_memory_source_ref(
    source: &AppMemorySourceRef,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_nonempty_bounded(
        "memory.source.selected_fields",
        source.selected_fields.len(),
        limits.max_collection_items(),
    )?;
    if !source
        .selected_fields
        .windows(2)
        .all(|fields| fields[0] < fields[1])
    {
        return Err(AppContractError::invalid(
            "memory.source.selected_fields",
            "must be strictly sorted and unique",
        ));
    }
    if source.canonical_source_ref.kind != AppSourceRefKind::EntityField
        || source.canonical_source_ref.revision != Some(source.record_revision)
        || source.canonical_source_ref.fields != source.selected_fields
    {
        return Err(AppContractError::invalid(
            "memory.source.canonical_source_ref",
            "must be the exact entity-field record revision and selected fields",
        ));
    }
    if source.handling_labels.model_processing == AppModelProcessing::None {
        return Err(AppContractError::invalid(
            "memory.source.handling_labels.model_processing",
            "model_processing none cannot enter memory promotion",
        ));
    }
    Ok(())
}

fn validate_provenance_refs(
    refs: &[AppSourceRef],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    let mut identities = HashSet::with_capacity(refs.len());
    let mut aggregate_fields = 0usize;
    if !refs
        .windows(2)
        .all(|sources| compare_source_refs(&sources[0], &sources[1]) == Ordering::Less)
    {
        return Err(AppContractError::invalid(
            "memory.evidence_and_provenance_refs",
            "must be strictly sorted and unique",
        ));
    }
    for source in refs {
        validate_bounded(
            "memory.provenance_ref.fields",
            source.fields.len(),
            limits.max_collection_items(),
        )?;
        if !source.fields.windows(2).all(|fields| fields[0] < fields[1]) {
            return Err(AppContractError::invalid(
                "memory.provenance_ref.fields",
                "must be strictly sorted and unique",
            ));
        }
        aggregate_fields = aggregate_fields.saturating_add(source.fields.len());
        if aggregate_fields > limits.max_collection_items() {
            return Err(AppContractError::invalid(
                "memory.provenance_ref.fields",
                "exceeds the aggregate provenance-field ceiling",
            ));
        }
        if matches!(
            source.kind,
            AppSourceRefKind::EntityRecord | AppSourceRefKind::EntityField
        ) && source.revision.is_none()
        {
            return Err(AppContractError::invalid(
                "memory.provenance_ref.revision",
                "entity provenance requires an exact revision",
            ));
        }
        if source.kind == AppSourceRefKind::EntityField && source.fields.is_empty() {
            return Err(AppContractError::invalid(
                "memory.provenance_ref.fields",
                "entity-field provenance requires selected fields",
            ));
        }
        let identity = (
            source_kind_order(source.kind),
            source.reference.as_str().to_owned(),
            source.revision.map(AppRevision::get),
        );
        if !identities.insert(identity) {
            return Err(AppContractError::invalid(
                "memory.evidence_and_provenance_refs",
                "contains a duplicate canonical source identity",
            ));
        }
    }
    Ok(())
}

fn candidate_fingerprint(
    candidate: &AppMemoryCandidate,
) -> Result<AppDigest, AppMemoryEligibilityError> {
    #[derive(Serialize)]
    struct Material<'a> {
        protocol_version: AppProtocolVersion,
        scope: &'a AppScope,
        intended_tier_scope: &'a AppMemoryTierScope,
        semantic_destination: AppMemorySemanticDestination,
        source_refs: &'a [AppMemorySourceRef],
        claim_content_digest: &'a AppDigest,
        handling_labels: &'a AppHandlingLabels,
        evidence_and_provenance_refs: &'a [AppSourceRef],
    }
    digest_serializable(&Material {
        protocol_version: candidate.protocol_version,
        scope: &candidate.scope,
        intended_tier_scope: &candidate.intended_tier_scope,
        semantic_destination: candidate.semantic_destination,
        source_refs: &candidate.source_refs,
        claim_content_digest: &candidate.claim_content_digest,
        handling_labels: &candidate.handling_labels,
        evidence_and_provenance_refs: &candidate.evidence_and_provenance_refs,
    })
}

#[allow(clippy::too_many_arguments)]
fn source_eligibility_digest(
    scope: &AppScope,
    scope_binding_ref: &AppScopeBindingRef,
    source: &AppMemorySourceRef,
    installation_generation: u64,
    eligibility_revision: AppRevision,
    state: AppMemoryResolvedSourceState,
    unavailable_installation_status: Option<AppInstallationStatus>,
    effective_policy: Option<&super::records::AppDataHandlingPolicy>,
    resolved_at: &DateTime<Utc>,
) -> Result<AppDigest, AppMemoryEligibilityError> {
    #[derive(Serialize)]
    struct Material<'a> {
        protocol_version: AppProtocolVersion,
        scope: &'a AppScope,
        scope_binding_ref: &'a AppScopeBindingRef,
        source: &'a AppMemorySourceRef,
        installation_generation: u64,
        eligibility_revision: AppRevision,
        state: AppMemoryResolvedSourceState,
        unavailable_installation_status: Option<AppInstallationStatus>,
        effective_policy: Option<&'a super::records::AppDataHandlingPolicy>,
        resolved_at: &'a DateTime<Utc>,
    }
    digest_serializable(&Material {
        protocol_version: AppProtocolVersion::V1,
        scope,
        scope_binding_ref,
        source,
        installation_generation,
        eligibility_revision,
        state,
        unavailable_installation_status,
        effective_policy,
        resolved_at,
    })
}

fn memory_fence_digest(
    candidate: &AppMemoryCandidate,
    sources: &[AppMemoryEligibilitySourceFence],
    resolved_at: &DateTime<Utc>,
) -> Result<AppDigest, AppMemoryEligibilityError> {
    #[derive(Serialize)]
    struct Material<'a> {
        protocol_version: AppProtocolVersion,
        candidate_id: &'a AppReference,
        candidate_revision: AppRevision,
        candidate_fingerprint: &'a AppDigest,
        scope: &'a AppScope,
        sources: &'a [AppMemoryEligibilitySourceFence],
        resolved_at: &'a DateTime<Utc>,
    }
    digest_serializable(&Material {
        protocol_version: AppProtocolVersion::V1,
        candidate_id: &candidate.candidate_id,
        candidate_revision: candidate.candidate_revision,
        candidate_fingerprint: &candidate.candidate_fingerprint,
        scope: &candidate.scope,
        sources,
        resolved_at,
    })
}

fn digest_claim(claim: &str) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::Value::String(claim.to_owned())).map_err(
        |error| {
            AppContractError::invalid(
                "memory.claim_content_digest",
                format!("cannot encode canonical claim: {error}"),
            )
        },
    )
}

fn digest_serializable<T: Serialize>(value: &T) -> Result<AppDigest, AppMemoryEligibilityError> {
    let value = serde_json::to_value(value)
        .map_err(|error| AppMemoryEligibilityError::DigestEncoding(error.to_string()))?;
    AppDigest::blake3_canonical_json(&value)
        .map_err(|error| AppMemoryEligibilityError::DigestEncoding(error.to_string()))
}

fn field_path_exists(value: &serde_json::Value, field: &AppFieldPath) -> bool {
    let mut current = value;
    for segment in field.as_str().split('.') {
        let Some(next) = current.as_object().and_then(|object| object.get(segment)) else {
            return false;
        };
        current = next;
    }
    true
}

fn memory_source_identity(source: &AppMemorySourceRef) -> (String, String, String, String) {
    (
        source.installation_id.as_str().to_owned(),
        source.entity_name.as_str().to_owned(),
        source.record_id.as_str().to_owned(),
        source.canonical_source_ref.reference.as_str().to_owned(),
    )
}

fn compare_memory_sources(left: &AppMemorySourceRef, right: &AppMemorySourceRef) -> Ordering {
    left.installation_id
        .cmp(&right.installation_id)
        .then_with(|| left.entity_name.cmp(&right.entity_name))
        .then_with(|| left.record_id.cmp(&right.record_id))
        .then_with(|| left.record_revision.cmp(&right.record_revision))
        .then_with(|| {
            left.canonical_source_ref
                .reference
                .cmp(&right.canonical_source_ref.reference)
        })
}

fn source_kind_order(kind: AppSourceRefKind) -> u8 {
    match kind {
        AppSourceRefKind::EntityRecord => 0,
        AppSourceRefKind::EntityField => 1,
        AppSourceRefKind::Artifact => 2,
        AppSourceRefKind::ExternalReceipt => 3,
        AppSourceRefKind::MutationReceipt => 4,
    }
}

fn compare_source_refs(left: &AppSourceRef, right: &AppSourceRef) -> Ordering {
    source_kind_order(left.kind)
        .cmp(&source_kind_order(right.kind))
        .then_with(|| left.reference.cmp(&right.reference))
        .then_with(|| left.revision.cmp(&right.revision))
        .then_with(|| left.fields.cmp(&right.fields))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;
    use serde_json::{json, Value};

    use super::*;
    use crate::magician_v2::apps::{
        authority::{AppScopeAuthentication, ResolvedAppAuthority},
        lifecycle::AppInstallationLifecycle,
        models::{AppDataClassification, AppDataEnvelope, AppDataSource},
        policy::{join_app_content, AppHandlingConstraint, RevalidatedAppEnvelope},
        records::{
            AppBackgroundExecution, AppDataHandlingPolicy, AppExternalEgress, AppNetworkPolicy,
            AppPersonalAgentAccess, AppRecordActorKind, AppRecordProvenance, AppResourceCeiling,
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

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn policy(memory_promotion: AppMemoryPromotion) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 1,
            max_output_tokens: 1,
            max_cost_microusd: 1,
            max_paid_tool_invocations: 1,
            max_active_seconds: 1,
            max_lifetime_seconds: 1,
            max_browser_network_actions: 1,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 0,
            max_records: 10,
            max_payload_bytes: 1_024,
            max_attachment_bytes: 1_024,
            max_monthly_tokens: 2,
            max_monthly_cost_microusd: 1,
        }
    }

    fn scope() -> AppScope {
        AppScope {
            principal: reference("anonymous"),
            workspace: reference("default"),
        }
    }

    fn authenticated_scope() -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            scope(),
            AppScopeBindingRef::parse("scope_1").unwrap(),
            reference("actor:owner"),
            reference("session:1"),
            AppRevision::new(1).unwrap(),
            time(0),
            time(59),
        )
        .unwrap()
    }

    fn authority(
        memory_promotion: AppMemoryPromotion,
        resolved_at: DateTime<Utc>,
    ) -> ResolvedAppAuthority {
        ResolvedAppAuthority {
            scope_binding_ref: AppScopeBindingRef::parse("scope_1").unwrap(),
            actor_ref: reference("actor:owner"),
            session_ref: reference("session:1"),
            authentication: AppScopeAuthentication::AuthenticatedSession,
            authentication_revision: AppRevision::new(1).unwrap(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            installation_generation: 2,
            package_revision_ref: reference("package:1"),
            grant_revision: AppRevision::new(1).unwrap(),
            grant_authority_digest: digest("grant-authority"),
            schema_revision: AppRevision::new(1).unwrap(),
            surface_revision: None,
            authority_digest: digest("authority"),
            effective_tools: BTreeSet::new(),
            effective_context_reads: BTreeSet::new(),
            effective_data_handling_policy: policy(memory_promotion),
            effective_background_execution: AppBackgroundExecution::Denied,
            effective_network_policy: AppNetworkPolicy::Denied,
            effective_resources: resources(),
            effective_any_public_host: false,
            resolved_at,
        }
    }

    fn record(revision: u64) -> AppRecordRevision {
        AppRecordRevision {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity_name: AppName::parse("person").unwrap(),
            record_id: AppRecordId::parse("record_1").unwrap(),
            record_revision: AppRevision::new(revision).unwrap(),
            dataset_generation: revision,
            schema_revision: AppRevision::new(1).unwrap(),
            payload: json!({"name": "Asha", "role": "mentor"}),
            handling_override: None,
            created_at: time(1),
            updated_at: time(revision as u32 + 1),
            deleted_at: None,
            provenance: AppRecordProvenance {
                actor_kind: AppRecordActorKind::User,
                actor_id: reference("actor:owner"),
                execution_id: None,
                output_revision: None,
                mutation_receipt_id: None,
                source_artifact_refs: Vec::new(),
                citation_refs: Vec::new(),
            },
        }
    }

    fn labels() -> AppHandlingLabels {
        AppHandlingLabels {
            classification: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            policy_digest: digest("field-policy"),
            provenance_digest: digest("field-provenance"),
        }
    }

    fn source_evidence(
        auth: &AuthenticatedAppScope,
        authority: &ResolvedAppAuthority,
        record: &AppRecordRevision,
        eligibility_revision: u64,
        resolved_at: DateTime<Utc>,
    ) -> ResolvedAppMemorySource {
        ResolvedAppMemorySource::from_enabled_record(
            auth,
            authority,
            record,
            reference("entity:person/record:1"),
            vec![AppFieldPath::parse("name").unwrap()],
            ResolvedAppHandlingLabels::from_trusted_policy(labels()),
            AppRevision::new(eligibility_revision).unwrap(),
            resolved_at,
        )
        .unwrap()
    }

    fn joined_content(auth: &AuthenticatedAppScope) -> AppJoinedContent {
        let source_ref = AppSourceRef {
            kind: AppSourceRefKind::EntityField,
            reference: reference("entity:person/record:1"),
            revision: Some(AppRevision::new(1).unwrap()),
            fields: vec![AppFieldPath::parse("name").unwrap()],
        };
        let value = json!({"name": "Asha"});
        let envelope = AppDataEnvelope {
            protocol_version: AppProtocolVersion::V1,
            source: AppDataSource::AppStore,
            scope_binding_ref: auth.scope_binding_ref().clone(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: reference("package:1"),
            schema_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            value_schema_ref: reference("schema:person"),
            value: value.clone(),
            source_refs: vec![source_ref],
            handling_labels: labels(),
            content_digest: AppDigest::blake3_canonical_json(&value).unwrap(),
            produced_at: time(5),
            expires_at: None,
        };
        let revalidated = RevalidatedAppEnvelope::from_trusted_resolution(
            &envelope,
            ResolvedAppHandlingLabels::from_trusted_policy(labels()),
            &AppContractLimits::default(),
        )
        .unwrap();
        join_app_content(
            &[revalidated],
            Value::String("Asha is a mentor".to_owned()),
            &AppHandlingConstraint {
                classification_floor: AppDataClassification::Personal,
                model_processing: AppModelProcessing::LocalOnly,
                policy_digest: digest("memory-constraint"),
                purpose: reference("memory:promotion"),
                audience_ref: reference("user:owner"),
            },
            &AppContractLimits::default(),
        )
        .unwrap()
    }

    fn proposed_candidate() -> (
        AuthenticatedAppScope,
        ResolvedAppAuthority,
        AppRecordRevision,
        ResolvedAppMemorySource,
        AppMemoryCandidate,
    ) {
        let auth = authenticated_scope();
        let proposal_authority = authority(AppMemoryPromotion::CandidateAllowed, time(11));
        let record = record(1);
        let proposal_source = source_evidence(&auth, &proposal_authority, &record, 1, time(11));
        let joined = joined_content(&auth);
        let candidate = propose_app_memory_candidate(
            &auth,
            reference("memory:candidate:1"),
            AppMemoryTierScope::User,
            AppMemorySemanticDestination::Entities,
            &joined,
            &[&proposal_source],
            time(11),
            &AppContractLimits::default(),
        )
        .unwrap();
        let authority = authority(AppMemoryPromotion::CandidateAllowed, time(13));
        let source = source_evidence(&auth, &authority, &record, 1, time(13));
        (auth, authority, record, source, candidate)
    }

    #[test]
    fn candidate_requires_acceptance_and_current_source_fence() {
        let (_, _, _, source, candidate) = proposed_candidate();
        assert_eq!(candidate.status, AppMemoryCandidateStatus::Proposed);
        assert_eq!(
            resolve_app_memory_eligibility(
                &candidate,
                &[&source],
                time(12),
                &AppContractLimits::default(),
            )
            .unwrap_err(),
            AppMemoryEligibilityError::CandidateNotAccepted
        );
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();
        assert_eq!(
            resolve_app_memory_eligibility(
                &accepted,
                &[&source],
                time(11),
                &AppContractLimits::default(),
            )
            .unwrap_err(),
            AppMemoryEligibilityError::ResolutionPrecedesCandidate
        );
        let fence = resolve_app_memory_eligibility(
            &accepted,
            &[&source],
            time(13),
            &AppContractLimits::default(),
        )
        .unwrap();
        assert_eq!(fence.candidate_id(), &accepted.candidate_id);
        assert_eq!(fence.candidate_revision(), accepted.candidate_revision);
        assert_eq!(
            resolve_app_memory_eligibility(
                &accepted,
                &[&source],
                time(14),
                &AppContractLimits::default(),
            )
            .unwrap_err(),
            AppMemoryEligibilityError::EvidenceNotCurrent
        );
    }

    #[test]
    fn changed_record_revision_makes_an_accepted_candidate_stale() {
        let (auth, authority, _, _, candidate) = proposed_candidate();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();
        let changed = record(2);
        let source = source_evidence(&auth, &authority, &changed, 2, time(13));
        let error = resolve_app_memory_eligibility(
            &accepted,
            &[&source],
            time(13),
            &AppContractLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error, AppMemoryEligibilityError::SourceStale);
        assert_eq!(
            error.required_candidate_command(),
            Some(AppMemoryCandidateCommand::MarkStale)
        );
    }

    #[test]
    fn missing_or_changed_policy_evidence_cannot_retrieve_memory() {
        let (auth, authority, record, _, candidate) = proposed_candidate();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();
        assert_eq!(
            resolve_app_memory_eligibility(
                &accepted,
                &[],
                time(13),
                &AppContractLimits::default(),
            )
            .unwrap_err(),
            AppMemoryEligibilityError::SourceEvidenceUnavailable
        );

        let changed_labels = AppHandlingLabels {
            policy_digest: digest("changed-field-policy"),
            ..labels()
        };
        let changed_policy = ResolvedAppMemorySource::from_enabled_record(
            &auth,
            &authority,
            &record,
            reference("entity:person/record:1"),
            vec![AppFieldPath::parse("name").unwrap()],
            ResolvedAppHandlingLabels::from_trusted_policy(changed_labels),
            AppRevision::new(2).unwrap(),
            time(13),
        )
        .unwrap();
        assert_eq!(
            resolve_app_memory_eligibility(
                &accepted,
                &[&changed_policy],
                time(13),
                &AppContractLimits::default(),
            )
            .unwrap_err(),
            AppMemoryEligibilityError::SourcePolicyChanged
        );
    }

    #[test]
    fn disabled_and_retained_installations_make_memory_dormant() {
        let (auth, authority, _, source, candidate) = proposed_candidate();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();
        for (status, timestamp) in [
            (AppInstallationStatus::Disabled, Some(time(9))),
            (AppInstallationStatus::UninstalledRetained, None),
        ] {
            let installation = AppInstallation {
                scope: scope(),
                installation_id: authority.installation_id.clone(),
                package_revision_ref: authority.package_revision_ref.clone(),
                lifecycle: AppInstallationLifecycle {
                    status,
                    generation: 3,
                    update_return_status: None,
                },
                grant_revision: Some(authority.grant_revision),
                active_schema_revision: Some(authority.schema_revision),
                active_surface_revision: Some(AppRevision::new(1).unwrap()),
                created_at: time(1),
                updated_at: time(9),
                disabled_at: if status == AppInstallationStatus::Disabled {
                    timestamp
                } else {
                    None
                },
                quarantined_at: None,
                uninstalled_at: if status == AppInstallationStatus::UninstalledRetained {
                    Some(time(9))
                } else {
                    None
                },
                purged_at: None,
            };
            let dormant = ResolvedAppMemorySource::from_unavailable_installation(
                &auth,
                &installation,
                source.source().clone(),
                AppRevision::new(2).unwrap(),
                time(13),
            )
            .unwrap();
            assert_eq!(
                resolve_app_memory_eligibility(
                    &accepted,
                    &[&dormant],
                    time(13),
                    &AppContractLimits::default(),
                )
                .unwrap_err(),
                AppMemoryEligibilityError::SourceDormant { status }
            );
        }
    }

    #[test]
    fn deleted_or_purged_sole_source_requires_tombstone() {
        let (auth, authority, _, source, candidate) = proposed_candidate();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();

        let mut deleted_record = record(1);
        deleted_record.deleted_at = Some(time(4));
        let deleted = source_evidence(&auth, &authority, &deleted_record, 2, time(13));
        let deleted_error = resolve_app_memory_eligibility(
            &accepted,
            &[&deleted],
            time(13),
            &AppContractLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            deleted_error.required_candidate_command(),
            Some(AppMemoryCandidateCommand::Tombstone)
        );

        let installation = AppInstallation {
            scope: scope(),
            installation_id: authority.installation_id.clone(),
            package_revision_ref: authority.package_revision_ref.clone(),
            lifecycle: AppInstallationLifecycle {
                status: AppInstallationStatus::Purged,
                generation: 4,
                update_return_status: None,
            },
            grant_revision: None,
            active_schema_revision: None,
            active_surface_revision: None,
            created_at: time(1),
            updated_at: time(9),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: Some(time(8)),
            purged_at: Some(time(9)),
        };
        let purged = ResolvedAppMemorySource::from_unavailable_installation(
            &auth,
            &installation,
            source.source().clone(),
            AppRevision::new(3).unwrap(),
            time(13),
        )
        .unwrap();
        let purged_error = resolve_app_memory_eligibility(
            &accepted,
            &[&purged],
            time(13),
            &AppContractLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            purged_error.required_candidate_command(),
            Some(AppMemoryCandidateCommand::Tombstone)
        );
    }

    #[test]
    fn multi_source_removal_requires_rederivation_instead_of_silent_edit() {
        let (auth, authority, _, source, mut candidate) = proposed_candidate();
        let mut second = source.source().clone();
        second.record_id = AppRecordId::parse("record_2").unwrap();
        second.canonical_source_ref.reference = reference("entity:person/record:2");
        candidate
            .evidence_and_provenance_refs
            .push(second.canonical_source_ref.clone());
        candidate
            .evidence_and_provenance_refs
            .sort_by(compare_source_refs);
        candidate.source_refs.push(second);
        candidate.source_refs.sort_by(compare_memory_sources);
        candidate.seal_fingerprint().unwrap();
        candidate
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();

        let installation = AppInstallation {
            scope: scope(),
            installation_id: authority.installation_id.clone(),
            package_revision_ref: authority.package_revision_ref.clone(),
            lifecycle: AppInstallationLifecycle {
                status: AppInstallationStatus::Purged,
                generation: 4,
                update_return_status: None,
            },
            grant_revision: None,
            active_schema_revision: None,
            active_surface_revision: None,
            created_at: time(1),
            updated_at: time(9),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: Some(time(8)),
            purged_at: Some(time(9)),
        };
        let purged = ResolvedAppMemorySource::from_unavailable_installation(
            &auth,
            &installation,
            source.source().clone(),
            AppRevision::new(3).unwrap(),
            time(13),
        )
        .unwrap();
        let error = resolve_app_memory_eligibility(
            &accepted,
            &[&purged],
            time(13),
            &AppContractLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            AppMemoryEligibilityError::SourceRemoved {
                kind: AppMemorySourceRemovalKind::Purged,
                source_count: 2,
            }
        );
        assert_eq!(
            error.required_candidate_command(),
            Some(AppMemoryCandidateCommand::MarkStale)
        );
    }

    #[test]
    fn revoked_promotion_and_none_processing_fail_closed() {
        let auth = authenticated_scope();
        let record = record(1);
        let revoked_authority = authority(AppMemoryPromotion::Denied, time(13));
        let revoked = source_evidence(&auth, &revoked_authority, &record, 2, time(13));
        assert_eq!(
            revoked.state(),
            AppMemoryResolvedSourceState::PromotionRevoked
        );
        let (_, _, _, _, candidate) = proposed_candidate();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();
        let revoked_error = resolve_app_memory_eligibility(
            &accepted,
            &[&revoked],
            time(13),
            &AppContractLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            revoked_error.required_candidate_command(),
            Some(AppMemoryCandidateCommand::MarkStale)
        );

        let mut denied_authority = authority(AppMemoryPromotion::CandidateAllowed, time(13));
        denied_authority
            .effective_data_handling_policy
            .model_processing = AppModelProcessing::None;
        let denied_labels = AppHandlingLabels {
            model_processing: AppModelProcessing::None,
            ..labels()
        };
        let denied = ResolvedAppMemorySource::from_enabled_record(
            &auth,
            &denied_authority,
            &record,
            reference("entity:person/record:1"),
            vec![AppFieldPath::parse("name").unwrap()],
            ResolvedAppHandlingLabels::from_trusted_policy(denied_labels),
            AppRevision::new(3).unwrap(),
            time(13),
        )
        .unwrap();
        assert_eq!(
            denied.state(),
            AppMemoryResolvedSourceState::ModelProcessingDenied
        );
    }

    #[test]
    fn candidate_fingerprint_and_source_evidence_are_not_forgeable_shortcuts() {
        static_assertions::assert_not_impl_any!(
            ResolvedAppMemorySource: serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppMemoryEligibilityFence: serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppMemoryEligibilitySourceFence: serde::de::DeserializeOwned, Clone
        );

        let (_, _, _, _, mut candidate) = proposed_candidate();
        candidate.derived_claim_or_summary.push('!');
        assert!(candidate
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("claim_content_digest"));

        let (_, _, _, _, candidate) = proposed_candidate();
        let mut wire = serde_json::to_value(candidate).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("temperature".to_owned(), json!("hot"));
        assert!(serde_json::from_value::<AppMemoryCandidate>(wire).is_err());
    }

    #[test]
    fn lifecycle_never_resurrects_stale_or_tombstoned_source_identity() {
        let (_, _, _, _, candidate) = proposed_candidate();
        let accepted = candidate
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();
        let stale = accepted
            .apply(AppMemoryCandidateCommand::MarkStale, time(13))
            .unwrap();
        assert!(matches!(
            stale.apply(AppMemoryCandidateCommand::Accept, time(14)),
            Err(AppMemoryCandidateLifecycleError::InvalidTransition { .. })
        ));
        let tombstoned = stale
            .apply(AppMemoryCandidateCommand::Tombstone, time(14))
            .unwrap();
        assert!(matches!(
            tombstoned.apply(AppMemoryCandidateCommand::Accept, time(15)),
            Err(AppMemoryCandidateLifecycleError::InvalidTransition { .. })
        ));
    }
}
