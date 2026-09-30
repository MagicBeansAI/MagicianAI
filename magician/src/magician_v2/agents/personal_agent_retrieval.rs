//! Durable owner for app-contributed personal-agent retrieval projections.
//!
//! This is intentionally not an [`AgentMemoryService`](super::memory::AgentMemoryService)
//! adapter. It stores the closed retrieval proposal DTO in a distinct private
//! receipt/head/projection chain and exposes a typed, read-only query projection
//! for later prompt/search integration. Apps cannot write this store directly.
//!
//! Durable mutation order is immutable receipt -> head -> projection. Recovery
//! can replay a bounded suffix or adopt one exact orphan receipt. Lock order is
//! in-process owner mutex -> private cross-process head lock -> bounded no-follow
//! file I/O; no app-registry transaction may be held by this owner.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use magician_app_contract::contribution::{
    content_digest, AppContributionContractError, AppContributionRetractionPolicy,
    AppContributionUpdatePolicy, AppMemoryInvalidationDispositionV1, AppMemoryInvalidationV1,
    AppPersonalAgentRetrievalProjectionProposalV1, APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES,
    APP_RETRIEVAL_PROJECTION_CONTRACT_ID,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::magician_v2::apps::{
    authority::AuthenticatedAppScope, contribution::invalidation_advances_exact_source,
};

use super::storage::{validate_agent_identifier, AgentStorage, AgentStorageError};

const RETRIEVAL_OWNER_SCHEMA_VERSION: u16 = 1;
const MAX_ACTIVE_PROJECTIONS: usize = 1_024;
const MAX_SOURCE_HEADS: usize = 4_096;
const MAX_RECENT_RECEIPTS: u64 = 64;
const MAX_QUERY_RESULTS: usize = 128;
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_PROJECTION_BYTES: usize = 32 * 1024 * 1024;
const MAX_CHECKPOINT_BYTES: usize = MAX_PROJECTION_BYTES + 64 * 1024;

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[derive(Debug, Error)]
pub enum PersonalAgentRetrievalError {
    #[error("invalid personal-agent retrieval contribution: {0}")]
    Validation(String),
    #[error("personal-agent retrieval contribution conflicts with an exact durable identity: {0}")]
    Conflict(&'static str),
    #[error("personal-agent retrieval exact replay predates the retained receipt window")]
    HistoryCompacted,
    #[error("personal-agent retrieval storage error: {0}")]
    Storage(#[from] AgentStorageError),
    #[error("personal-agent retrieval JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("personal-agent retrieval contract error: {0}")]
    Contract(#[from] AppContributionContractError),
}

/// Exact server-owned identity of the retrieval implementation that owns the
/// projection. The type has no `Deserialize` implementation, so serialized app
/// or model output cannot mint a provider identity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalProviderIdentityV1 {
    provider_id: String,
    provider_revision: u64,
    provider_authority_digest: String,
    projection_schema_digest: String,
}

impl PersonalAgentRetrievalProviderIdentityV1 {
    pub(crate) fn from_trusted_provider(
        provider_id: impl Into<String>,
        provider_revision: u64,
        provider_authority_digest: impl Into<String>,
        projection_schema_digest: impl Into<String>,
    ) -> Result<Self, PersonalAgentRetrievalError> {
        let value = Self {
            provider_id: provider_id.into(),
            provider_revision,
            provider_authority_digest: provider_authority_digest.into(),
            projection_schema_digest: projection_schema_digest.into(),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), PersonalAgentRetrievalError> {
        validate_token("provider_id", &self.provider_id)?;
        if self.provider_revision == 0 {
            return validation("provider revision must be positive");
        }
        validate_digest(&self.provider_authority_digest)?;
        validate_digest(&self.projection_schema_digest)
    }
}

/// A non-deserializable snapshot of the reviewed grant that authorized one
/// contribution. Every field is compared with the sealed proposal and later
/// retained beside its exact source head.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalGrantFenceV1 {
    installation_id: String,
    installation_generation: u64,
    package_content_digest: String,
    grant_revision: u64,
    grant_authority_digest: String,
    contribution_port_digest: String,
    target_agent_id: String,
    target_goal_id: Option<String>,
    provider: PersonalAgentRetrievalProviderIdentityV1,
}

impl PersonalAgentRetrievalGrantFenceV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_reviewed_grant(
        installation_id: impl Into<String>,
        installation_generation: u64,
        package_content_digest: impl Into<String>,
        grant_revision: u64,
        grant_authority_digest: impl Into<String>,
        contribution_port_digest: impl Into<String>,
        target_agent_id: impl Into<String>,
        target_goal_id: Option<String>,
        provider: PersonalAgentRetrievalProviderIdentityV1,
    ) -> Result<Self, PersonalAgentRetrievalError> {
        let value = Self {
            installation_id: installation_id.into(),
            installation_generation,
            package_content_digest: package_content_digest.into(),
            grant_revision,
            grant_authority_digest: grant_authority_digest.into(),
            contribution_port_digest: contribution_port_digest.into(),
            target_agent_id: target_agent_id.into(),
            target_goal_id,
            provider,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), PersonalAgentRetrievalError> {
        validate_token("installation_id", &self.installation_id)?;
        if self.installation_generation == 0 || self.grant_revision == 0 {
            return validation("grant and installation generations must be positive");
        }
        validate_digest(&self.package_content_digest)?;
        validate_digest(&self.grant_authority_digest)?;
        validate_digest(&self.contribution_port_digest)?;
        validate_agent_identifier(&self.target_agent_id)?;
        if let Some(goal_id) = self.target_goal_id.as_deref() {
            validate_token("target_goal_id", goal_id)?;
        }
        self.provider.validate()
    }

    fn matches_proposal(&self, proposal: &AppPersonalAgentRetrievalProjectionProposalV1) -> bool {
        let header = &proposal.header;
        self.installation_id == header.installation_id
            && self.installation_generation == header.installation_generation
            && self.package_content_digest == header.package_content_digest
            && self.grant_revision == header.grant_revision
            && self.grant_authority_digest == header.grant_authority_digest
            && self.contribution_port_digest == header.contribution_port_digest
            && self.target_agent_id == proposal.target_agent_id
            && self.target_goal_id == proposal.target_goal_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RetrievalOwnerIdentityV1 {
    principal: String,
    workspace: String,
    scope_binding_ref: String,
    provider_id: String,
    provider_revision: u64,
    provider_authority_digest: String,
    projection_schema_digest: String,
}

impl RetrievalOwnerIdentityV1 {
    fn new(
        principal: String,
        workspace: String,
        scope_binding_ref: String,
        provider: &PersonalAgentRetrievalProviderIdentityV1,
    ) -> Result<Self, PersonalAgentRetrievalError> {
        let value = Self {
            principal,
            workspace,
            scope_binding_ref,
            provider_id: provider.provider_id.clone(),
            provider_revision: provider.provider_revision,
            provider_authority_digest: provider.provider_authority_digest.clone(),
            projection_schema_digest: provider.projection_schema_digest.clone(),
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), PersonalAgentRetrievalError> {
        validate_token("principal", &self.principal)?;
        validate_token("workspace", &self.workspace)?;
        validate_token("scope_binding_ref", &self.scope_binding_ref)?;
        validate_token("provider_id", &self.provider_id)?;
        if self.provider_revision == 0 {
            return validation("owner provider revision must be positive");
        }
        validate_digest(&self.provider_authority_digest)?;
        validate_digest(&self.projection_schema_digest)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RetrievalDestinationOperationV1 {
    Stage {
        proposal: AppPersonalAgentRetrievalProjectionProposalV1,
        grant_fence: PersistedGrantFenceV1,
    },
    Invalidate {
        invalidation: AppMemoryInvalidationV1,
        grant_fence: PersistedGrantFenceV1,
    },
    Expire {
        source_head_key: String,
        proposal_id: String,
        proposal_digest: String,
        expires_at_ms: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersistedGrantFenceV1 {
    installation_id: String,
    installation_generation: u64,
    package_content_digest: String,
    grant_revision: u64,
    grant_authority_digest: String,
    contribution_port_digest: String,
    target_agent_id: String,
    target_goal_id: Option<String>,
    provider_id: String,
    provider_revision: u64,
    provider_authority_digest: String,
    projection_schema_digest: String,
}

impl From<&PersonalAgentRetrievalGrantFenceV1> for PersistedGrantFenceV1 {
    fn from(value: &PersonalAgentRetrievalGrantFenceV1) -> Self {
        Self {
            installation_id: value.installation_id.clone(),
            installation_generation: value.installation_generation,
            package_content_digest: value.package_content_digest.clone(),
            grant_revision: value.grant_revision,
            grant_authority_digest: value.grant_authority_digest.clone(),
            contribution_port_digest: value.contribution_port_digest.clone(),
            target_agent_id: value.target_agent_id.clone(),
            target_goal_id: value.target_goal_id.clone(),
            provider_id: value.provider.provider_id.clone(),
            provider_revision: value.provider.provider_revision,
            provider_authority_digest: value.provider.provider_authority_digest.clone(),
            projection_schema_digest: value.provider.projection_schema_digest.clone(),
        }
    }
}

impl PersistedGrantFenceV1 {
    fn validate(&self) -> Result<(), PersonalAgentRetrievalError> {
        validate_token("installation_id", &self.installation_id)?;
        validate_token("provider_id", &self.provider_id)?;
        validate_agent_identifier(&self.target_agent_id)?;
        if let Some(goal_id) = self.target_goal_id.as_deref() {
            validate_token("target_goal_id", goal_id)?;
        }
        if self.installation_generation == 0
            || self.grant_revision == 0
            || self.provider_revision == 0
        {
            return validation("persisted authority revisions must be positive");
        }
        for digest in [
            &self.package_content_digest,
            &self.grant_authority_digest,
            &self.contribution_port_digest,
            &self.provider_authority_digest,
            &self.projection_schema_digest,
        ] {
            validate_digest(digest)?;
        }
        Ok(())
    }

    fn matches_owner(&self, identity: &RetrievalOwnerIdentityV1) -> bool {
        self.provider_id == identity.provider_id
            && self.provider_revision == identity.provider_revision
            && self.provider_authority_digest == identity.provider_authority_digest
            && self.projection_schema_digest == identity.projection_schema_digest
    }

    fn matches_proposal(&self, proposal: &AppPersonalAgentRetrievalProjectionProposalV1) -> bool {
        let header = &proposal.header;
        self.installation_id == header.installation_id
            && self.installation_generation == header.installation_generation
            && self.package_content_digest == header.package_content_digest
            && self.grant_revision == header.grant_revision
            && self.grant_authority_digest == header.grant_authority_digest
            && self.contribution_port_digest == header.contribution_port_digest
            && self.target_agent_id == proposal.target_agent_id
            && self.target_goal_id == proposal.target_goal_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalReceiptV1 {
    pub schema_version: u16,
    pub receipt_id: String,
    pub owner_identity_digest: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_digest: Option<String>,
    operation: RetrievalDestinationOperationV1,
    pub operation_digest: String,
    pub invalidation_disposition: Option<AppMemoryInvalidationDispositionV1>,
    pub resulting_projection_digest: String,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RetrievalSourceStateV1 {
    Live,
    Invalidated,
    Expired,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersonalAgentRetrievalInspectionStateV1 {
    Accepted,
    Stale,
    Tombstoned,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalInspectionItemV1 {
    pub proposal_id: String,
    pub proposal_digest: String,
    pub installation_id: String,
    pub source_event_ref: String,
    pub source_event_revision: u64,
    pub state: PersonalAgentRetrievalInspectionStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_invalidation_reason:
        Option<magician_app_contract::contribution::AppMemoryInvalidationReasonV1>,
    pub state_changed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub target_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_goal_id: Option<String>,
    pub details_compacted: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalInspectionSnapshotV1 {
    pub destination_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_receipt_digest: Option<String>,
    pub items: Vec<PersonalAgentRetrievalInspectionItemV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RetrievalSourceHeadV1 {
    installation_id: String,
    installation_generation: u64,
    package_content_digest: String,
    scope_binding_ref: String,
    canonical_source_ref: String,
    canonical_source_digest: String,
    dedupe_key: String,
    proposal_id: String,
    proposal_revision: u64,
    proposal_digest: String,
    source_record_revision: u64,
    proposal_expires_at_ms: i64,
    grant_revision: u64,
    grant_authority_digest: String,
    contribution_port_digest: String,
    target_agent_id: String,
    target_goal_id: Option<String>,
    state: RetrievalSourceStateV1,
    generation: u64,
    operation_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_evidence_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_reason: Option<magician_app_contract::contribution::AppMemoryInvalidationReasonV1>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    state_changed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredRetrievalProjectionV1 {
    schema_version: u16,
    owner: RetrievalOwnerIdentityV1,
    generation: u64,
    last_recorded_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_receipt_digest: Option<String>,
    replay_floor_generation: u64,
    entries: BTreeMap<String, AppPersonalAgentRetrievalProjectionProposalV1>,
    source_heads: BTreeMap<String, RetrievalSourceHeadV1>,
    applied_operations: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RetrievalDestinationHeadV1 {
    schema_version: u16,
    owner_identity_digest: String,
    generation: u64,
    last_recorded_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_receipt_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RetrievalDestinationCheckpointV1 {
    schema_version: u16,
    owner_identity_digest: String,
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_receipt_digest: Option<String>,
    projection: StoredRetrievalProjectionV1,
    projection_digest: String,
    checkpoint_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalAgentRetrievalExpectedHeadV1 {
    pub generation: u64,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonalAgentRetrievalApplyOutcomeV1 {
    Applied(PersonalAgentRetrievalReceiptV1),
    ExactReplay(PersonalAgentRetrievalReceiptV1),
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalAgentRetrievalQueryV1 {
    pub target_agent_id: String,
    pub target_goal_id: Option<String>,
    pub installation_id: String,
    pub installation_generation: u64,
    pub package_content_digest: String,
    pub grant_revision: u64,
    pub grant_authority_digest: String,
    pub provider: PersonalAgentRetrievalProviderIdentityV1,
    pub maximum_results: usize,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalQueryEntryV1 {
    pub proposal: AppPersonalAgentRetrievalProjectionProposalV1,
    pub source_head_key: String,
    pub admitted_generation: u64,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PersonalAgentRetrievalQueryProjectionV1 {
    pub principal: String,
    pub workspace: String,
    pub scope_binding_ref: String,
    pub provider: PersonalAgentRetrievalProviderIdentityV1,
    pub destination_generation: u64,
    pub destination_receipt_digest: Option<String>,
    pub entries: Vec<PersonalAgentRetrievalQueryEntryV1>,
}

/// Distinct retrieval destination. It owns no memory-tier files and performs no
/// embedding, ranking, prompt rendering, or app-registry mutation.
#[derive(Debug, Clone)]
pub struct PersonalAgentRetrievalOwner {
    storage: AgentStorage,
    owner: RetrievalOwnerIdentityV1,
    provider: PersonalAgentRetrievalProviderIdentityV1,
    mutation_lock: Arc<Mutex<()>>,
}

impl PersonalAgentRetrievalOwner {
    pub(crate) fn new(
        storage: AgentStorage,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        scope_binding_ref: impl Into<String>,
        provider: PersonalAgentRetrievalProviderIdentityV1,
    ) -> Result<Self, PersonalAgentRetrievalError> {
        let principal = principal.into();
        let workspace = workspace.into();
        if storage.scope_segments().as_ref() != Some(&(principal.clone(), workspace.clone())) {
            return validation("retrieval owner scope does not match its scoped storage root");
        }
        let owner = RetrievalOwnerIdentityV1::new(
            principal,
            workspace,
            scope_binding_ref.into(),
            &provider,
        )?;
        Ok(Self {
            storage,
            owner,
            provider,
            mutation_lock: Arc::new(Mutex::new(())),
        })
    }

    pub async fn stage_projection(
        &self,
        authenticated: &AuthenticatedAppScope,
        grant_fence: PersonalAgentRetrievalGrantFenceV1,
        proposal: AppPersonalAgentRetrievalProjectionProposalV1,
        expected_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
        recorded_at_ms: i64,
    ) -> Result<PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError> {
        self.validate_authenticated_scope(authenticated)?;
        grant_fence.validate()?;
        proposal.validate()?;
        if grant_fence.provider != self.provider || !grant_fence.matches_proposal(&proposal) {
            return Err(PersonalAgentRetrievalError::Conflict(
                "reviewed grant/provider fence",
            ));
        }
        let operation = RetrievalDestinationOperationV1::Stage {
            proposal,
            grant_fence: PersistedGrantFenceV1::from(&grant_fence),
        };
        self.apply_operation(operation, expected_head, recorded_at_ms)
            .await
    }

    pub async fn invalidate_projection(
        &self,
        authenticated: &AuthenticatedAppScope,
        grant_fence: PersonalAgentRetrievalGrantFenceV1,
        invalidation: AppMemoryInvalidationV1,
        expected_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
        recorded_at_ms: i64,
    ) -> Result<PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError> {
        self.validate_authenticated_scope(authenticated)?;
        grant_fence.validate()?;
        invalidation.validate()?;
        if grant_fence.provider != self.provider
            || grant_fence.installation_id != invalidation.installation_id
        {
            return Err(PersonalAgentRetrievalError::Conflict(
                "invalidation grant/provider fence",
            ));
        }
        let operation = RetrievalDestinationOperationV1::Invalidate {
            invalidation,
            grant_fence: PersistedGrantFenceV1::from(&grant_fence),
        };
        self.apply_operation(operation, expected_head, recorded_at_ms)
            .await
    }

    /// Durably expires one exact live proposal. Queries independently refuse
    /// expired rows, so delivery lag cannot make expired text prompt-visible.
    pub async fn expire_projection(
        &self,
        authenticated: &AuthenticatedAppScope,
        proposal_id: &str,
        proposal_digest: &str,
        expected_head: PersonalAgentRetrievalExpectedHeadV1,
        recorded_at_ms: i64,
    ) -> Result<PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError> {
        self.validate_authenticated_scope(authenticated)?;
        validate_token("proposal_id", proposal_id)?;
        validate_digest(proposal_digest)?;
        let _guard = self.mutation_lock.lock().await;
        let paths = RetrievalOwnerPaths::new(self.storage.root());
        self.ensure_layout(&paths).await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let recovered = recover_owner(&self.storage, &paths, &self.owner).await?;
        let (source_head_key, source_head) = recovered
            .projection
            .source_heads
            .iter()
            .find(|(_, source_head)| {
                source_head.proposal_id == proposal_id
                    && source_head.proposal_digest == proposal_digest
            })
            .ok_or(PersonalAgentRetrievalError::Conflict(
                "expiration proposal head",
            ))?;
        if Utc::now().timestamp_millis() < source_head.proposal_expires_at_ms
            || recorded_at_ms < source_head.proposal_expires_at_ms
        {
            return validation("retrieval projection cannot expire before its sealed deadline");
        }
        let operation = RetrievalDestinationOperationV1::Expire {
            source_head_key: source_head_key.clone(),
            proposal_id: proposal_id.to_owned(),
            proposal_digest: proposal_digest.to_owned(),
            expires_at_ms: source_head.proposal_expires_at_ms,
        };
        apply_operation_locked(
            &self.storage,
            &paths,
            &self.owner,
            recovered,
            operation,
            Some(expected_head),
            recorded_at_ms,
        )
        .await
    }

    pub async fn recover(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<Option<PersonalAgentRetrievalExpectedHeadV1>, PersonalAgentRetrievalError> {
        self.validate_authenticated_scope(authenticated)?;
        let _guard = self.mutation_lock.lock().await;
        let paths = RetrievalOwnerPaths::new(self.storage.root());
        self.ensure_layout(&paths).await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let recovered = recover_owner(&self.storage, &paths, &self.owner).await?;
        expected_head_from(&recovered.head)
    }

    pub(crate) async fn inspect_source_state(
        &self,
        authenticated: &AuthenticatedAppScope,
        maximum_results: usize,
    ) -> Result<PersonalAgentRetrievalInspectionSnapshotV1, PersonalAgentRetrievalError> {
        self.validate_authenticated_scope(authenticated)?;
        if maximum_results == 0 || maximum_results > 32 {
            return validation("retrieval inspection limit is outside the closed ceiling");
        }
        let _guard = self.mutation_lock.lock().await;
        let paths = RetrievalOwnerPaths::new(self.storage.root());
        self.ensure_layout(&paths).await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let recovered = recover_owner(&self.storage, &paths, &self.owner).await?;
        let mut items = recovered
            .projection
            .source_heads
            .values()
            .map(|head| {
                let state = match (head.state.clone(), head.terminal_reason) {
                    (RetrievalSourceStateV1::Live, _) => {
                        PersonalAgentRetrievalInspectionStateV1::Accepted
                    },
                    (
                        RetrievalSourceStateV1::Invalidated,
                        Some(
                            magician_app_contract::contribution::AppMemoryInvalidationReasonV1::SourceUpdated
                            | magician_app_contract::contribution::AppMemoryInvalidationReasonV1::SourceRestored
                            | magician_app_contract::contribution::AppMemoryInvalidationReasonV1::PolicyChanged,
                        ),
                    ) => PersonalAgentRetrievalInspectionStateV1::Stale,
                    _ => PersonalAgentRetrievalInspectionStateV1::Tombstoned,
                };
                PersonalAgentRetrievalInspectionItemV1 {
                    proposal_id: head.proposal_id.clone(),
                    proposal_digest: head.proposal_digest.clone(),
                    installation_id: head.installation_id.clone(),
                    source_event_ref: head.canonical_source_ref.clone(),
                    source_event_revision: head.source_record_revision,
                    state,
                    source_invalidation_reason: head.terminal_reason,
                    state_changed_at_ms: head.state_changed_at_ms,
                    expires_at_ms: (head.proposal_expires_at_ms > 0)
                        .then_some(head.proposal_expires_at_ms),
                    target_agent_id: head.target_agent_id.clone(),
                    target_goal_id: head.target_goal_id.clone(),
                    details_compacted: head.state != RetrievalSourceStateV1::Live,
                }
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| {
            right
                .state_changed_at_ms
                .cmp(&left.state_changed_at_ms)
                .then_with(|| left.proposal_digest.cmp(&right.proposal_digest))
        });
        items.truncate(maximum_results);
        let snapshot = PersonalAgentRetrievalInspectionSnapshotV1 {
            destination_generation: recovered.head.generation,
            destination_receipt_digest: recovered.head.latest_receipt_digest,
            items,
        };
        if serde_json::to_vec(&snapshot)?.len() > 256 * 1024 {
            return validation("retrieval inspection exceeds its disclosure ceiling");
        }
        Ok(snapshot)
    }

    #[cfg(test)]
    pub async fn query_projection(
        &self,
        authenticated: &AuthenticatedAppScope,
        query: PersonalAgentRetrievalQueryV1,
        now_ms: i64,
    ) -> Result<PersonalAgentRetrievalQueryProjectionV1, PersonalAgentRetrievalError> {
        self.validate_authenticated_scope(authenticated)?;
        validate_query(&query, &self.provider, now_ms)?;
        let effective_now_ms = now_ms.max(Utc::now().timestamp_millis());
        let _guard = self.mutation_lock.lock().await;
        let paths = RetrievalOwnerPaths::new(self.storage.root());
        self.ensure_layout(&paths).await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let recovered = recover_owner(&self.storage, &paths, &self.owner).await?;
        let mut entries = Vec::with_capacity(query.maximum_results);
        for (source_head_key, proposal) in &recovered.projection.entries {
            if proposal.target_agent_id != query.target_agent_id
                || proposal.target_goal_id != query.target_goal_id
                || proposal.header.installation_id != query.installation_id
                || proposal.header.installation_generation != query.installation_generation
                || proposal.header.package_content_digest != query.package_content_digest
                || proposal.header.grant_revision != query.grant_revision
                || proposal.header.grant_authority_digest != query.grant_authority_digest
                || proposal.header.expires_at_ms <= effective_now_ms
            {
                continue;
            }
            let source_head = recovered
                .projection
                .source_heads
                .get(source_head_key)
                .ok_or(PersonalAgentRetrievalError::Conflict("query source head"))?;
            if source_head.state != RetrievalSourceStateV1::Live {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "query-visible source state",
                ));
            }
            entries.push(PersonalAgentRetrievalQueryEntryV1 {
                proposal: proposal.clone(),
                source_head_key: source_head_key.clone(),
                admitted_generation: source_head.generation,
            });
            if entries.len() == query.maximum_results {
                break;
            }
        }
        Ok(PersonalAgentRetrievalQueryProjectionV1 {
            principal: self.owner.principal.clone(),
            workspace: self.owner.workspace.clone(),
            scope_binding_ref: self.owner.scope_binding_ref.clone(),
            provider: self.provider.clone(),
            destination_generation: recovered.head.generation,
            destination_receipt_digest: recovered.head.latest_receipt_digest,
            entries,
        })
    }

    /// Return a bounded, private candidate snapshot for the core projection
    /// adapter. This is deliberately not a public query surface: proposal
    /// bytes remain inside the core until the adapter independently reopens
    /// the current installation, grant, package and source authority for each
    /// candidate immediately before disclosure.
    pub(crate) async fn projected_candidates_for_target(
        &self,
        authenticated: &AuthenticatedAppScope,
        target_agent_id: &str,
        target_goal_id: Option<&str>,
        maximum_results: usize,
        now_ms: i64,
    ) -> Result<Vec<AppPersonalAgentRetrievalProjectionProposalV1>, PersonalAgentRetrievalError>
    {
        self.validate_authenticated_scope(authenticated)?;
        validate_agent_identifier(target_agent_id)?;
        if let Some(goal_id) = target_goal_id {
            validate_token("target_goal_id", goal_id)?;
        }
        if maximum_results == 0 || maximum_results > MAX_QUERY_RESULTS {
            return validation("retrieval query result limit is outside the closed ceiling");
        }
        let effective_now_ms = now_ms.max(Utc::now().timestamp_millis());
        let _guard = self.mutation_lock.lock().await;
        let paths = RetrievalOwnerPaths::new(self.storage.root());
        self.ensure_layout(&paths).await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let recovered = recover_owner(&self.storage, &paths, &self.owner).await?;
        let mut proposals = Vec::with_capacity(maximum_results);
        for (source_head_key, proposal) in &recovered.projection.entries {
            if proposal.target_agent_id != target_agent_id
                || proposal.target_goal_id.as_deref() != target_goal_id
                || proposal.header.expires_at_ms <= effective_now_ms
            {
                continue;
            }
            let source_head = recovered
                .projection
                .source_heads
                .get(source_head_key)
                .ok_or(PersonalAgentRetrievalError::Conflict(
                    "prompt query source head",
                ))?;
            if source_head.state != RetrievalSourceStateV1::Live {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "prompt query-visible source state",
                ));
            }
            proposals.push(proposal.clone());
            if proposals.len() == maximum_results {
                break;
            }
        }
        Ok(proposals)
    }

    async fn apply_operation(
        &self,
        operation: RetrievalDestinationOperationV1,
        expected_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
        recorded_at_ms: i64,
    ) -> Result<PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError> {
        let _guard = self.mutation_lock.lock().await;
        let paths = RetrievalOwnerPaths::new(self.storage.root());
        self.ensure_layout(&paths).await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let recovered = recover_owner(&self.storage, &paths, &self.owner).await?;
        apply_operation_locked(
            &self.storage,
            &paths,
            &self.owner,
            recovered,
            operation,
            expected_head,
            recorded_at_ms,
        )
        .await
    }

    fn validate_authenticated_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<(), PersonalAgentRetrievalError> {
        authenticated.ensure_live_at(&Utc::now()).map_err(|error| {
            PersonalAgentRetrievalError::Validation(format!(
                "authenticated retrieval scope is not live: {error}"
            ))
        })?;
        if authenticated.scope().principal.as_str() != self.owner.principal
            || authenticated.scope().workspace.as_str() != self.owner.workspace
            || authenticated.scope_binding_ref().as_str() != self.owner.scope_binding_ref
        {
            return Err(PersonalAgentRetrievalError::Conflict(
                "authenticated scope binding",
            ));
        }
        Ok(())
    }

    async fn ensure_layout(
        &self,
        paths: &RetrievalOwnerPaths,
    ) -> Result<(), PersonalAgentRetrievalError> {
        self.storage
            .ensure_private_directory(&paths.authority_root)
            .await?;
        self.storage.ensure_private_directory(&paths.root).await?;
        self.storage
            .ensure_private_directory(&paths.receipts)
            .await?;
        Ok(())
    }
}

#[derive(Debug)]
struct RecoveredRetrievalOwner {
    head: RetrievalDestinationHeadV1,
    projection: StoredRetrievalProjectionV1,
}

struct RetrievalOwnerPaths {
    authority_root: PathBuf,
    root: PathBuf,
    receipts: PathBuf,
    head: PathBuf,
    projection: PathBuf,
    checkpoint: PathBuf,
}

impl RetrievalOwnerPaths {
    fn new(storage_root: &Path) -> Self {
        let authority_root = storage_root.join("app-contributions");
        let root = authority_root.join("personal-agent-retrieval-v1");
        Self {
            authority_root,
            receipts: root.join("receipts"),
            head: root.join("head.json"),
            projection: root.join("projection.json"),
            checkpoint: root.join("checkpoint.json"),
            root,
        }
    }

    fn receipt(&self, generation: u64) -> PathBuf {
        self.receipts.join(format!("{generation:020}.json"))
    }
}

async fn apply_operation_locked(
    storage: &AgentStorage,
    paths: &RetrievalOwnerPaths,
    owner: &RetrievalOwnerIdentityV1,
    mut recovered: RecoveredRetrievalOwner,
    operation: RetrievalDestinationOperationV1,
    expected_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
    recorded_at_ms: i64,
) -> Result<PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError> {
    if recorded_at_ms > Utc::now().timestamp_millis().saturating_add(60_000) {
        return validation("retrieval receipt timestamp is implausibly far in the future");
    }
    validate_operation(&operation, owner, recorded_at_ms)?;
    let operation_digest =
        digest_serialized("magician.personal-agent-retrieval.operation.v1", &operation)?;
    if let Some(generation) = recovered
        .projection
        .applied_operations
        .get(&operation_digest)
        .copied()
    {
        let receipt = read_receipt(storage, paths, owner, generation).await?;
        if receipt.operation != operation {
            return Err(PersonalAgentRetrievalError::Conflict(
                "operation replay bytes",
            ));
        }
        validate_replay_predecessor(expected_head.as_ref(), &receipt)?;
        return Ok(PersonalAgentRetrievalApplyOutcomeV1::ExactReplay(receipt));
    }
    if operation_matches_compacted_head(&recovered.projection, &operation, &operation_digest)? {
        return Err(PersonalAgentRetrievalError::HistoryCompacted);
    }
    if let RetrievalDestinationOperationV1::Stage { proposal, .. } = &operation {
        let live_now_ms = Utc::now().timestamp_millis();
        if proposal.header.issued_at_ms > live_now_ms
            || proposal.header.expires_at_ms <= live_now_ms
        {
            return validation("retrieval proposal is not live at destination admission");
        }
    }
    validate_current_expected_head(&recovered.head, expected_head.as_ref())?;
    if recorded_at_ms < recovered.head.last_recorded_at_ms {
        return validation("retrieval receipt time regressed behind the durable head");
    }
    let generation =
        recovered.head.generation.checked_add(1).ok_or_else(|| {
            PersonalAgentRetrievalError::Validation("generation overflow".to_owned())
        })?;
    let invalidation_disposition = apply_projection_operation(
        &mut recovered.projection,
        &operation,
        &operation_digest,
        generation,
        recorded_at_ms,
    )?;
    if serde_json::to_vec(&recovered.projection)?.len() > MAX_PROJECTION_BYTES {
        return validation("retrieval projection exceeds its durable byte ceiling");
    }
    let resulting_projection_digest = projection_digest(&recovered.projection)?;
    let owner_identity_digest = owner_digest(owner)?;
    let receipt_id = format!(
        "personal-agent-retrieval-receipt:{}",
        digest_serialized(
            "magician.personal-agent-retrieval.receipt-id.v1",
            &(generation, &operation_digest),
        )?
        .trim_start_matches("blake3:")
    );
    let mut receipt = PersonalAgentRetrievalReceiptV1 {
        schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
        receipt_id,
        owner_identity_digest: owner_identity_digest.clone(),
        generation,
        previous_receipt_digest: recovered.head.latest_receipt_digest.clone(),
        operation,
        operation_digest,
        invalidation_disposition,
        resulting_projection_digest,
        recorded_at_ms,
        receipt_digest: String::new(),
    };
    receipt.receipt_digest = receipt_digest(&receipt)?;
    validate_receipt(&receipt, owner)?;
    write_immutable_receipt(storage, paths, &receipt).await?;
    recovered.head = RetrievalDestinationHeadV1 {
        schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
        owner_identity_digest,
        generation,
        last_recorded_at_ms: recorded_at_ms,
        latest_receipt_digest: Some(receipt.receipt_digest.clone()),
    };
    recovered.projection.latest_receipt_digest = Some(receipt.receipt_digest.clone());
    validate_projection(&recovered.projection)?;
    write_private_json(storage, &paths.head, &recovered.head, MAX_HEAD_BYTES).await?;
    write_private_json(
        storage,
        &paths.projection,
        &recovered.projection,
        MAX_PROJECTION_BYTES,
    )
    .await?;
    maybe_checkpoint(storage, paths, owner, &recovered).await?;
    Ok(PersonalAgentRetrievalApplyOutcomeV1::Applied(receipt))
}

fn apply_projection_operation(
    projection: &mut StoredRetrievalProjectionV1,
    operation: &RetrievalDestinationOperationV1,
    operation_digest: &str,
    generation: u64,
    recorded_at_ms: i64,
) -> Result<Option<AppMemoryInvalidationDispositionV1>, PersonalAgentRetrievalError> {
    if recorded_at_ms < projection.last_recorded_at_ms {
        return validation("receipt timestamp regressed inside the projection chain");
    }
    let invalidation_disposition = match operation {
        RetrievalDestinationOperationV1::Stage {
            proposal,
            grant_fence,
        } => {
            let source = proposal.header.sources.first().ok_or_else(|| {
                PersonalAgentRetrievalError::Validation("proposal lost exact source".to_owned())
            })?;
            let key = source_head_key(
                &proposal.header.installation_id,
                &proposal.header.scope_binding_ref,
                &source.canonical_source_ref,
                &proposal.header.dedupe_key,
            )?;
            if let Some(previous) = projection.source_heads.get(&key) {
                if previous.proposal_digest == proposal.proposal_digest {
                    return Err(PersonalAgentRetrievalError::HistoryCompacted);
                }
                let replacement_at_invalidation_revision = previous.state
                    == RetrievalSourceStateV1::Invalidated
                    && source.record_revision == previous.source_record_revision
                    && source.canonical_source_digest != previous.canonical_source_digest;
                let authority_rollback_or_substitution = grant_fence.installation_generation
                    < previous.installation_generation
                    || (grant_fence.installation_generation == previous.installation_generation
                        && grant_fence.package_content_digest != previous.package_content_digest)
                    || grant_fence.grant_revision < previous.grant_revision
                    || (grant_fence.grant_revision == previous.grant_revision
                        && grant_fence.grant_authority_digest != previous.grant_authority_digest)
                    || (grant_fence.installation_generation == previous.installation_generation
                        && grant_fence.contribution_port_digest
                            != previous.contribution_port_digest)
                    || (grant_fence.grant_revision == previous.grant_revision
                        && (grant_fence.target_agent_id != previous.target_agent_id
                            || grant_fence.target_goal_id != previous.target_goal_id));
                if source.record_revision < previous.source_record_revision
                    || (source.record_revision == previous.source_record_revision
                        && !replacement_at_invalidation_revision)
                    || proposal.header.proposal_revision <= previous.proposal_revision
                    || authority_rollback_or_substitution
                {
                    return Err(PersonalAgentRetrievalError::Conflict(
                        "source/proposal revision rollback or substitution",
                    ));
                }
            } else if projection.source_heads.len() >= MAX_SOURCE_HEADS {
                return validation("retrieval source-head quota is exhausted");
            }
            if !projection.entries.contains_key(&key)
                && projection.entries.len() >= MAX_ACTIVE_PROJECTIONS
            {
                return validation("active retrieval projection quota is exhausted");
            }
            projection.entries.insert(key.clone(), proposal.clone());
            projection.source_heads.insert(
                key,
                RetrievalSourceHeadV1 {
                    installation_id: proposal.header.installation_id.clone(),
                    installation_generation: proposal.header.installation_generation,
                    package_content_digest: proposal.header.package_content_digest.clone(),
                    scope_binding_ref: proposal.header.scope_binding_ref.clone(),
                    canonical_source_ref: source.canonical_source_ref.clone(),
                    canonical_source_digest: source.canonical_source_digest.clone(),
                    dedupe_key: proposal.header.dedupe_key.clone(),
                    proposal_id: proposal.header.proposal_id.clone(),
                    proposal_revision: proposal.header.proposal_revision,
                    proposal_digest: proposal.proposal_digest.clone(),
                    source_record_revision: source.record_revision,
                    proposal_expires_at_ms: proposal.header.expires_at_ms,
                    grant_revision: grant_fence.grant_revision,
                    grant_authority_digest: grant_fence.grant_authority_digest.clone(),
                    contribution_port_digest: grant_fence.contribution_port_digest.clone(),
                    target_agent_id: proposal.target_agent_id.clone(),
                    target_goal_id: proposal.target_goal_id.clone(),
                    state: RetrievalSourceStateV1::Live,
                    generation,
                    operation_digest: operation_digest.to_owned(),
                    terminal_evidence_digest: None,
                    terminal_reason: None,
                    state_changed_at_ms: recorded_at_ms,
                },
            );
            None
        },
        RetrievalDestinationOperationV1::Invalidate {
            invalidation,
            grant_fence,
        } => {
            let key = source_head_key(
                &invalidation.installation_id,
                &invalidation.scope_binding_ref,
                &invalidation.source_event_ref,
                &invalidation.dedupe_key,
            )?;
            Some(match projection.source_heads.get_mut(&key) {
                Some(head) => {
                    if head.proposal_id != invalidation.proposal_id
                        || head.proposal_digest != invalidation.proposal_digest
                        || head.canonical_source_digest != invalidation.source_identity_digest
                        || head.grant_revision != grant_fence.grant_revision
                        || head.grant_authority_digest != grant_fence.grant_authority_digest
                        || head.installation_generation != grant_fence.installation_generation
                        || head.package_content_digest != grant_fence.package_content_digest
                        || head.contribution_port_digest != grant_fence.contribution_port_digest
                        || head.target_agent_id != grant_fence.target_agent_id
                        || head.target_goal_id != grant_fence.target_goal_id
                        || !invalidation_advances_exact_source(
                            invalidation.reason,
                            invalidation.source_event_revision,
                            head.source_record_revision,
                        )
                    {
                        return Err(PersonalAgentRetrievalError::Conflict(
                            "invalidation exact source/grant head",
                        ));
                    }
                    let disposition = if head.state == RetrievalSourceStateV1::Live {
                        AppMemoryInvalidationDispositionV1::Tombstoned
                    } else {
                        AppMemoryInvalidationDispositionV1::AlreadyTombstoned
                    };
                    projection.entries.remove(&key);
                    head.source_record_revision = invalidation.source_event_revision;
                    head.state = RetrievalSourceStateV1::Invalidated;
                    head.generation = generation;
                    head.operation_digest = operation_digest.to_owned();
                    head.terminal_evidence_digest = Some(invalidation.invalidation_digest.clone());
                    head.terminal_reason = Some(invalidation.reason);
                    head.state_changed_at_ms = recorded_at_ms;
                    disposition
                },
                None => {
                    if projection.source_heads.len() >= MAX_SOURCE_HEADS {
                        return validation("retrieval source-head quota is exhausted");
                    }
                    projection.source_heads.insert(
                        key,
                        RetrievalSourceHeadV1 {
                            installation_id: invalidation.installation_id.clone(),
                            installation_generation: grant_fence.installation_generation,
                            package_content_digest: grant_fence.package_content_digest.clone(),
                            scope_binding_ref: invalidation.scope_binding_ref.clone(),
                            canonical_source_ref: invalidation.source_event_ref.clone(),
                            canonical_source_digest: invalidation.source_identity_digest.clone(),
                            dedupe_key: invalidation.dedupe_key.clone(),
                            proposal_id: invalidation.proposal_id.clone(),
                            proposal_revision: 0,
                            proposal_digest: invalidation.proposal_digest.clone(),
                            source_record_revision: invalidation.source_event_revision,
                            proposal_expires_at_ms: 0,
                            grant_revision: grant_fence.grant_revision,
                            grant_authority_digest: grant_fence.grant_authority_digest.clone(),
                            contribution_port_digest: grant_fence.contribution_port_digest.clone(),
                            target_agent_id: grant_fence.target_agent_id.clone(),
                            target_goal_id: grant_fence.target_goal_id.clone(),
                            state: RetrievalSourceStateV1::Invalidated,
                            generation,
                            operation_digest: operation_digest.to_owned(),
                            terminal_evidence_digest: Some(
                                invalidation.invalidation_digest.clone(),
                            ),
                            terminal_reason: Some(invalidation.reason),
                            state_changed_at_ms: recorded_at_ms,
                        },
                    );
                    AppMemoryInvalidationDispositionV1::SupersededBeforeAdmission
                },
            })
        },
        RetrievalDestinationOperationV1::Expire {
            source_head_key,
            proposal_id,
            proposal_digest,
            ..
        } => {
            let head = projection.source_heads.get_mut(source_head_key).ok_or(
                PersonalAgentRetrievalError::Conflict("expiration source head"),
            )?;
            if head.state != RetrievalSourceStateV1::Live
                || head.proposal_id != *proposal_id
                || head.proposal_digest != *proposal_digest
            {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "expiration exact proposal head",
                ));
            }
            projection.entries.remove(source_head_key);
            head.state = RetrievalSourceStateV1::Expired;
            head.generation = generation;
            head.operation_digest = operation_digest.to_owned();
            head.terminal_evidence_digest = Some(content_digest(b"contribution_expired"));
            head.terminal_reason = Some(
                magician_app_contract::contribution::AppMemoryInvalidationReasonV1::ContributionExpired,
            );
            head.state_changed_at_ms = recorded_at_ms;
            None
        },
    };
    projection.generation = generation;
    projection.last_recorded_at_ms = recorded_at_ms;
    projection
        .applied_operations
        .insert(operation_digest.to_owned(), generation);
    projection.replay_floor_generation = generation.saturating_sub(MAX_RECENT_RECEIPTS);
    projection
        .applied_operations
        .retain(|_, applied_generation| *applied_generation > projection.replay_floor_generation);
    Ok(invalidation_disposition)
}

fn validate_operation(
    operation: &RetrievalDestinationOperationV1,
    owner: &RetrievalOwnerIdentityV1,
    recorded_at_ms: i64,
) -> Result<(), PersonalAgentRetrievalError> {
    if recorded_at_ms <= 0 {
        return validation("retrieval receipt timestamp must be positive");
    }
    match operation {
        RetrievalDestinationOperationV1::Stage {
            proposal,
            grant_fence,
        } => {
            proposal.validate()?;
            grant_fence.validate()?;
            let source = proposal.header.sources.first().ok_or_else(|| {
                PersonalAgentRetrievalError::Validation("proposal has no exact source".to_owned())
            })?;
            if proposal.header.sources.len() != 1
                || proposal.header.update_policy
                    != AppContributionUpdatePolicy::ReplaceExactSourceHead
                || proposal.header.retraction_policy
                    != AppContributionRetractionPolicy::TombstoneOnAnySourceDrift
                || proposal.header.destination_contract_id != APP_RETRIEVAL_PROJECTION_CONTRACT_ID
                || proposal.header.destination_schema_digest != owner.projection_schema_digest
                || proposal.header.scope_binding_ref != owner.scope_binding_ref
                || source.installation_id != proposal.header.installation_id
                || proposal.header.issued_at_ms > recorded_at_ms
                || proposal.header.expires_at_ms <= recorded_at_ms
                || !grant_fence.matches_owner(owner)
                || !grant_fence.matches_proposal(proposal)
            {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "sealed retrieval source/scope/grant/provider identity",
                ));
            }
            validate_agent_identifier(&proposal.target_agent_id)?;
            if let Some(goal_id) = proposal.target_goal_id.as_deref() {
                validate_token("target_goal_id", goal_id)?;
            }
        },
        RetrievalDestinationOperationV1::Invalidate {
            invalidation,
            grant_fence,
        } => {
            invalidation.validate()?;
            grant_fence.validate()?;
            if invalidation.scope_binding_ref != owner.scope_binding_ref
                || invalidation.installation_id != grant_fence.installation_id
                || invalidation.issued_at_ms > recorded_at_ms
                || !grant_fence.matches_owner(owner)
            {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "invalidation scope/grant/provider identity",
                ));
            }
        },
        RetrievalDestinationOperationV1::Expire {
            source_head_key,
            proposal_id,
            proposal_digest,
            expires_at_ms,
        } => {
            validate_digest(source_head_key)?;
            validate_token("proposal_id", proposal_id)?;
            validate_digest(proposal_digest)?;
            if *expires_at_ms < 0 || recorded_at_ms < *expires_at_ms {
                return validation("invalid retrieval expiration boundary");
            }
        },
    }
    Ok(())
}

fn operation_matches_compacted_head(
    projection: &StoredRetrievalProjectionV1,
    operation: &RetrievalDestinationOperationV1,
    operation_digest: &str,
) -> Result<bool, PersonalAgentRetrievalError> {
    let key = match operation {
        RetrievalDestinationOperationV1::Stage { proposal, .. } => {
            let source = proposal.header.sources.first().ok_or_else(|| {
                PersonalAgentRetrievalError::Validation("proposal lost exact source".to_owned())
            })?;
            source_head_key(
                &proposal.header.installation_id,
                &proposal.header.scope_binding_ref,
                &source.canonical_source_ref,
                &proposal.header.dedupe_key,
            )?
        },
        RetrievalDestinationOperationV1::Invalidate { invalidation, .. } => source_head_key(
            &invalidation.installation_id,
            &invalidation.scope_binding_ref,
            &invalidation.source_event_ref,
            &invalidation.dedupe_key,
        )?,
        RetrievalDestinationOperationV1::Expire {
            source_head_key, ..
        } => source_head_key.clone(),
    };
    Ok(projection
        .source_heads
        .get(&key)
        .is_some_and(|head| head.operation_digest == operation_digest))
}

async fn recover_owner(
    storage: &AgentStorage,
    paths: &RetrievalOwnerPaths,
    owner: &RetrievalOwnerIdentityV1,
) -> Result<RecoveredRetrievalOwner, PersonalAgentRetrievalError> {
    let owner_identity_digest = owner_digest(owner)?;
    let checkpoint = read_optional_private_json::<RetrievalDestinationCheckpointV1>(
        storage,
        &paths.checkpoint,
        MAX_CHECKPOINT_BYTES,
    )
    .await?;
    if let Some(checkpoint) = &checkpoint {
        validate_checkpoint(checkpoint, owner)?;
    }
    let stored_head = read_optional_private_json::<RetrievalDestinationHeadV1>(
        storage,
        &paths.head,
        MAX_HEAD_BYTES,
    )
    .await?;
    if stored_head.is_none()
        && checkpoint
            .as_ref()
            .is_some_and(|value| value.generation > 0)
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "destination head rollback behind checkpoint",
        ));
    }
    let mut head = match stored_head {
        Some(stored)
            if stored.owner_identity_digest != owner_identity_digest
                && stored.generation == 0
                && stored.last_recorded_at_ms == 0
                && stored.latest_receipt_digest.is_none()
                && checkpoint.is_none() =>
        {
            // Nothing was ever recorded under the previous owner: the head is
            // an empty marker, so adopting the current owner loses no data.
            // A sealed set (generation > 0) still fails closed below; that is
            // the substitution guard, and this branch never reaches it.
            tracing::warn!(
                previous_owner = %stored.owner_identity_digest,
                current_owner = %owner_identity_digest,
                "personal-agent retrieval owner changed; re-seeding the empty destination head"
            );
            let reseeded = RetrievalDestinationHeadV1 {
                schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
                owner_identity_digest: owner_identity_digest.clone(),
                generation: 0,
                last_recorded_at_ms: 0,
                latest_receipt_digest: None,
            };
            write_private_json(storage, &paths.head, &reseeded, MAX_HEAD_BYTES).await?;
            // A generation-0 projection written under the old owner would
            // fail the owner check below; drop it so recovery rebuilds it.
            match storage.remove_file(&paths.projection).await {
                Ok(()) => {},
                Err(AgentStorageError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error.into()),
            }
            reseeded
        },
        Some(stored) => stored,
        None => RetrievalDestinationHeadV1 {
            schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
            owner_identity_digest: owner_identity_digest.clone(),
            generation: 0,
            last_recorded_at_ms: 0,
            latest_receipt_digest: None,
        },
    };
    validate_head(&head, owner)?;
    if checkpoint
        .as_ref()
        .is_some_and(|value| value.generation > head.generation)
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "destination generation rollback behind checkpoint",
        ));
    }
    let stored_projection = read_optional_private_json::<StoredRetrievalProjectionV1>(
        storage,
        &paths.projection,
        MAX_PROJECTION_BYTES,
    )
    .await?;
    let mut projection = match stored_projection {
        Some(value) => value,
        None => checkpoint
            .as_ref()
            .map(|value| value.projection.clone())
            .unwrap_or_else(|| empty_projection(owner.clone())),
    };
    validate_projection(&projection)?;
    if projection.owner != *owner
        || projection.generation > head.generation
        || head.generation.saturating_sub(projection.generation) > MAX_RECENT_RECEIPTS
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "projection/head owner or bounded prefix",
        ));
    }
    while projection.generation < head.generation {
        let generation = projection.generation + 1;
        let receipt = read_receipt(storage, paths, owner, generation).await?;
        replay_receipt(&mut projection, &receipt, owner)?;
    }
    if projection.latest_receipt_digest != head.latest_receipt_digest {
        return Err(PersonalAgentRetrievalError::Conflict(
            "projection/head receipt digest",
        ));
    }
    if head.generation > 0 {
        let current = read_receipt(storage, paths, owner, head.generation).await?;
        if Some(current.receipt_digest) != head.latest_receipt_digest
            || current.resulting_projection_digest != projection_digest(&projection)?
            || current.recorded_at_ms != head.last_recorded_at_ms
        {
            return Err(PersonalAgentRetrievalError::Conflict(
                "current receipt/head/projection seal",
            ));
        }
    }
    let orphan_generation = head.generation.checked_add(1);
    if let Some(orphan) = match orphan_generation {
        Some(generation) => read_optional_receipt(storage, paths, owner, generation).await?,
        None => None,
    } {
        // Two exact no-follow probes distinguish one legitimate orphan from a
        // longer rolled-back prefix without enumerating an attacker-sized dir.
        if let Some(generation) = orphan.generation.checked_add(1) {
            if read_optional_receipt(storage, paths, owner, generation)
                .await?
                .is_some()
            {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "destination head rolled back by more than one receipt",
                ));
            }
        }
        replay_receipt(&mut projection, &orphan, owner)?;
        head.generation = orphan.generation;
        head.last_recorded_at_ms = orphan.recorded_at_ms;
        head.latest_receipt_digest = Some(orphan.receipt_digest);
    }
    if projection.last_recorded_at_ms != head.last_recorded_at_ms {
        return Err(PersonalAgentRetrievalError::Conflict(
            "projection/head receipt timestamp",
        ));
    }
    validate_projection(&projection)?;
    write_private_json(storage, &paths.head, &head, MAX_HEAD_BYTES).await?;
    write_private_json(
        storage,
        &paths.projection,
        &projection,
        MAX_PROJECTION_BYTES,
    )
    .await?;
    Ok(RecoveredRetrievalOwner { head, projection })
}

fn replay_receipt(
    projection: &mut StoredRetrievalProjectionV1,
    receipt: &PersonalAgentRetrievalReceiptV1,
    owner: &RetrievalOwnerIdentityV1,
) -> Result<(), PersonalAgentRetrievalError> {
    validate_receipt(receipt, owner)?;
    if receipt.generation != projection.generation + 1
        || receipt.previous_receipt_digest != projection.latest_receipt_digest
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "receipt chain predecessor",
        ));
    }
    let invalidation_disposition = apply_projection_operation(
        projection,
        &receipt.operation,
        &receipt.operation_digest,
        receipt.generation,
        receipt.recorded_at_ms,
    )?;
    if invalidation_disposition != receipt.invalidation_disposition {
        return Err(PersonalAgentRetrievalError::Conflict(
            "receipt invalidation disposition",
        ));
    }
    if projection_digest(projection)? != receipt.resulting_projection_digest {
        let mut legacy_projection = projection.clone();
        strip_retrieval_inspection_metadata(&mut legacy_projection);
        if projection_digest(&legacy_projection)? != receipt.resulting_projection_digest {
            return Err(PersonalAgentRetrievalError::Conflict(
                "receipt resulting projection digest",
            ));
        }
        *projection = legacy_projection;
    }
    projection.latest_receipt_digest = Some(receipt.receipt_digest.clone());
    validate_projection(projection)?;
    Ok(())
}

fn strip_retrieval_inspection_metadata(projection: &mut StoredRetrievalProjectionV1) {
    for head in projection.source_heads.values_mut() {
        head.terminal_reason = None;
        head.state_changed_at_ms = 0;
    }
}

fn empty_projection(owner: RetrievalOwnerIdentityV1) -> StoredRetrievalProjectionV1 {
    StoredRetrievalProjectionV1 {
        schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
        owner,
        generation: 0,
        last_recorded_at_ms: 0,
        latest_receipt_digest: None,
        replay_floor_generation: 0,
        entries: BTreeMap::new(),
        source_heads: BTreeMap::new(),
        applied_operations: BTreeMap::new(),
    }
}

fn validate_projection(
    projection: &StoredRetrievalProjectionV1,
) -> Result<(), PersonalAgentRetrievalError> {
    projection.owner.validate()?;
    if projection.schema_version != RETRIEVAL_OWNER_SCHEMA_VERSION
        || projection.entries.len() > MAX_ACTIVE_PROJECTIONS
        || projection.source_heads.len() > MAX_SOURCE_HEADS
        || projection.replay_floor_generation > projection.generation
        || projection.last_recorded_at_ms < 0
        || (projection.generation == 0) != (projection.last_recorded_at_ms == 0)
        || projection
            .generation
            .saturating_sub(projection.replay_floor_generation)
            > MAX_RECENT_RECEIPTS
        || (projection.generation == 0) != projection.latest_receipt_digest.is_none()
        || serde_json::to_vec(projection)?.len() > MAX_PROJECTION_BYTES
    {
        return validation("retrieval projection has an invalid bounded shape");
    }
    if let Some(digest) = &projection.latest_receipt_digest {
        validate_digest(digest)?;
    }
    let expected_operations = usize::try_from(
        projection
            .generation
            .saturating_sub(projection.replay_floor_generation),
    )
    .map_err(|_| PersonalAgentRetrievalError::Validation("operation window overflow".to_owned()))?;
    if projection.applied_operations.len() != expected_operations {
        return Err(PersonalAgentRetrievalError::Conflict(
            "contiguous operation replay window",
        ));
    }
    let mut generations = projection
        .applied_operations
        .values()
        .copied()
        .collect::<Vec<_>>();
    generations.sort_unstable();
    if generations
        .iter()
        .copied()
        .ne(projection.replay_floor_generation + 1..=projection.generation)
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "operation replay generations",
        ));
    }
    for (digest, generation) in &projection.applied_operations {
        validate_digest(digest)?;
        if *generation == 0 || *generation > projection.generation {
            return Err(PersonalAgentRetrievalError::Conflict(
                "operation replay generation bound",
            ));
        }
    }
    let mut live_heads = 0usize;
    for (key, head) in &projection.source_heads {
        validate_source_head(key, head, &projection.owner)?;
        if head.state == RetrievalSourceStateV1::Live {
            live_heads += 1;
            let proposal =
                projection
                    .entries
                    .get(key)
                    .ok_or(PersonalAgentRetrievalError::Conflict(
                        "live source projection",
                    ))?;
            if proposal.header.proposal_id != head.proposal_id
                || proposal.proposal_digest != head.proposal_digest
                || proposal.header.proposal_revision != head.proposal_revision
                || proposal.header.expires_at_ms != head.proposal_expires_at_ms
                || proposal.header.grant_revision != head.grant_revision
                || proposal.header.grant_authority_digest != head.grant_authority_digest
                || proposal.header.installation_generation != head.installation_generation
                || proposal.header.package_content_digest != head.package_content_digest
                || proposal.header.contribution_port_digest != head.contribution_port_digest
                || proposal.target_agent_id != head.target_agent_id
                || proposal.target_goal_id != head.target_goal_id
            {
                return Err(PersonalAgentRetrievalError::Conflict(
                    "live source proposal identity",
                ));
            }
        } else if projection.entries.contains_key(key) {
            return Err(PersonalAgentRetrievalError::Conflict(
                "terminal source remained query-visible",
            ));
        }
    }
    if live_heads != projection.entries.len() {
        return Err(PersonalAgentRetrievalError::Conflict(
            "live source/projection cardinality",
        ));
    }
    for (key, proposal) in &projection.entries {
        proposal.validate()?;
        let source = proposal.header.sources.first().ok_or_else(|| {
            PersonalAgentRetrievalError::Validation("projection lost exact source".to_owned())
        })?;
        if proposal.header.sources.len() != 1
            || proposal.header.scope_binding_ref != projection.owner.scope_binding_ref
            || proposal.header.destination_contract_id != APP_RETRIEVAL_PROJECTION_CONTRACT_ID
            || proposal.header.destination_schema_digest
                != projection.owner.projection_schema_digest
            || source_head_key(
                &proposal.header.installation_id,
                &proposal.header.scope_binding_ref,
                &source.canonical_source_ref,
                &proposal.header.dedupe_key,
            )? != *key
        {
            return Err(PersonalAgentRetrievalError::Conflict(
                "projection exact source/scope identity",
            ));
        }
    }
    Ok(())
}

fn validate_source_head(
    key: &str,
    head: &RetrievalSourceHeadV1,
    owner: &RetrievalOwnerIdentityV1,
) -> Result<(), PersonalAgentRetrievalError> {
    validate_digest(key)?;
    validate_token("installation_id", &head.installation_id)?;
    validate_digest(&head.package_content_digest)?;
    validate_token("scope_binding_ref", &head.scope_binding_ref)?;
    validate_token("canonical_source_ref", &head.canonical_source_ref)?;
    validate_digest(&head.canonical_source_digest)?;
    validate_token("dedupe_key", &head.dedupe_key)?;
    validate_token("proposal_id", &head.proposal_id)?;
    validate_digest(&head.proposal_digest)?;
    validate_digest(&head.grant_authority_digest)?;
    validate_digest(&head.contribution_port_digest)?;
    validate_agent_identifier(&head.target_agent_id)?;
    if let Some(goal_id) = head.target_goal_id.as_deref() {
        validate_token("target_goal_id", goal_id)?;
    }
    validate_digest(&head.operation_digest)?;
    if head.scope_binding_ref != owner.scope_binding_ref
        || head.source_record_revision == 0
        || head.installation_generation == 0
        || head.grant_revision == 0
        || head.generation == 0
        || head.state_changed_at_ms < 0
        || source_head_key(
            &head.installation_id,
            &head.scope_binding_ref,
            &head.canonical_source_ref,
            &head.dedupe_key,
        )? != key
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "source high-water identity",
        ));
    }
    match head.state {
        RetrievalSourceStateV1::Live
            if head.proposal_revision > 0
                && head.proposal_expires_at_ms > 0
                && head.terminal_evidence_digest.is_none()
                && head.terminal_reason.is_none() => {},
        RetrievalSourceStateV1::Invalidated => {
            validate_digest(head.terminal_evidence_digest.as_deref().ok_or(
                PersonalAgentRetrievalError::Conflict("terminal source evidence"),
            )?)?;
        },
        RetrievalSourceStateV1::Expired
            if head.proposal_revision > 0 && head.proposal_expires_at_ms > 0 =>
        {
            validate_digest(head.terminal_evidence_digest.as_deref().ok_or(
                PersonalAgentRetrievalError::Conflict("expired source evidence"),
            )?)?;
        },
        _ => {
            return Err(PersonalAgentRetrievalError::Conflict(
                "source high-water state",
            ));
        },
    }
    Ok(())
}

fn validate_head(
    head: &RetrievalDestinationHeadV1,
    owner: &RetrievalOwnerIdentityV1,
) -> Result<(), PersonalAgentRetrievalError> {
    if head.schema_version != RETRIEVAL_OWNER_SCHEMA_VERSION
        || head.owner_identity_digest != owner_digest(owner)?
        || head.last_recorded_at_ms < 0
        || (head.generation == 0) != (head.last_recorded_at_ms == 0)
        || (head.generation == 0) != head.latest_receipt_digest.is_none()
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "destination head identity",
        ));
    }
    if let Some(digest) = &head.latest_receipt_digest {
        validate_digest(digest)?;
    }
    Ok(())
}

fn validate_receipt(
    receipt: &PersonalAgentRetrievalReceiptV1,
    owner: &RetrievalOwnerIdentityV1,
) -> Result<(), PersonalAgentRetrievalError> {
    if receipt.schema_version != RETRIEVAL_OWNER_SCHEMA_VERSION
        || receipt.receipt_id
            != format!(
                "personal-agent-retrieval-receipt:{}",
                digest_serialized(
                    "magician.personal-agent-retrieval.receipt-id.v1",
                    &(receipt.generation, &receipt.operation_digest),
                )?
                .trim_start_matches("blake3:")
            )
        || receipt.owner_identity_digest != owner_digest(owner)?
        || receipt.generation == 0
        || receipt.recorded_at_ms < 0
        || receipt.operation_digest
            != digest_serialized(
                "magician.personal-agent-retrieval.operation.v1",
                &receipt.operation,
            )?
        || receipt.receipt_digest != receipt_digest(receipt)?
        || serde_json::to_vec(receipt)?.len() > APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES
    {
        return Err(PersonalAgentRetrievalError::Conflict(
            "destination receipt seal",
        ));
    }
    validate_token("receipt_id", &receipt.receipt_id)?;
    match (&receipt.operation, receipt.invalidation_disposition) {
        (RetrievalDestinationOperationV1::Invalidate { .. }, Some(_))
        | (RetrievalDestinationOperationV1::Stage { .. }, None)
        | (RetrievalDestinationOperationV1::Expire { .. }, None) => {},
        _ => {
            return Err(PersonalAgentRetrievalError::Conflict(
                "destination receipt disposition shape",
            ));
        },
    }
    match (
        receipt.generation,
        receipt.previous_receipt_digest.as_deref(),
    ) {
        (1, None) => {},
        (1, Some(_)) | (_, None) => {
            return Err(PersonalAgentRetrievalError::Conflict(
                "destination receipt predecessor",
            ));
        },
        (_, Some(digest)) => validate_digest(digest)?,
    }
    validate_digest(&receipt.operation_digest)?;
    validate_digest(&receipt.resulting_projection_digest)?;
    validate_digest(&receipt.receipt_digest)?;
    validate_operation(&receipt.operation, owner, receipt.recorded_at_ms)
}

fn validate_checkpoint(
    checkpoint: &RetrievalDestinationCheckpointV1,
    owner: &RetrievalOwnerIdentityV1,
) -> Result<(), PersonalAgentRetrievalError> {
    let mut unsigned = checkpoint.clone();
    unsigned.checkpoint_digest.clear();
    if checkpoint.schema_version != RETRIEVAL_OWNER_SCHEMA_VERSION
        || checkpoint.owner_identity_digest != owner_digest(owner)?
        || checkpoint.generation == 0
        || checkpoint.generation != checkpoint.projection.generation
        || checkpoint.latest_receipt_digest != checkpoint.projection.latest_receipt_digest
        || checkpoint.projection.owner != *owner
        || checkpoint.projection_digest != projection_digest(&checkpoint.projection)?
        || checkpoint.checkpoint_digest
            != digest_serialized("magician.personal-agent-retrieval.checkpoint.v1", &unsigned)?
    {
        return Err(PersonalAgentRetrievalError::Conflict("checkpoint seal"));
    }
    validate_projection(&checkpoint.projection)
}

async fn maybe_checkpoint(
    storage: &AgentStorage,
    paths: &RetrievalOwnerPaths,
    owner: &RetrievalOwnerIdentityV1,
    recovered: &RecoveredRetrievalOwner,
) -> Result<(), PersonalAgentRetrievalError> {
    if recovered.head.generation == 0 || recovered.head.generation % MAX_RECENT_RECEIPTS != 0 {
        return Ok(());
    }
    let mut checkpoint = RetrievalDestinationCheckpointV1 {
        schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
        owner_identity_digest: owner_digest(owner)?,
        generation: recovered.head.generation,
        latest_receipt_digest: recovered.head.latest_receipt_digest.clone(),
        projection: recovered.projection.clone(),
        projection_digest: projection_digest(&recovered.projection)?,
        checkpoint_digest: String::new(),
    };
    checkpoint.checkpoint_digest = digest_serialized(
        "magician.personal-agent-retrieval.checkpoint.v1",
        &checkpoint,
    )?;
    validate_checkpoint(&checkpoint, owner)?;
    write_private_json(
        storage,
        &paths.checkpoint,
        &checkpoint,
        MAX_CHECKPOINT_BYTES,
    )
    .await?;
    let prune_through = recovered
        .head
        .generation
        .saturating_sub(MAX_RECENT_RECEIPTS);
    let previous_checkpoint_floor = prune_through.saturating_sub(MAX_RECENT_RECEIPTS);
    for generation in previous_checkpoint_floor.saturating_add(1)..=prune_through {
        storage
            .remove_private_file_and_sync_parent(paths.receipt(generation))
            .await?;
    }
    Ok(())
}

fn validate_current_expected_head(
    head: &RetrievalDestinationHeadV1,
    expected: Option<&PersonalAgentRetrievalExpectedHeadV1>,
) -> Result<(), PersonalAgentRetrievalError> {
    match (head.generation, &head.latest_receipt_digest, expected) {
        (0, None, None) => Ok(()),
        (generation, Some(digest), Some(expected))
            if generation == expected.generation && digest == &expected.receipt_digest =>
        {
            Ok(())
        },
        _ => Err(PersonalAgentRetrievalError::Conflict(
            "expected destination head",
        )),
    }
}

fn validate_replay_predecessor(
    expected: Option<&PersonalAgentRetrievalExpectedHeadV1>,
    receipt: &PersonalAgentRetrievalReceiptV1,
) -> Result<(), PersonalAgentRetrievalError> {
    match (
        receipt.generation,
        &receipt.previous_receipt_digest,
        expected,
    ) {
        (1, None, None) => Ok(()),
        (generation, Some(digest), Some(expected))
            if expected.generation.checked_add(1) == Some(generation)
                && digest == &expected.receipt_digest =>
        {
            Ok(())
        },
        _ => Err(PersonalAgentRetrievalError::Conflict(
            "replayed operation predecessor",
        )),
    }
}

fn expected_head_from(
    head: &RetrievalDestinationHeadV1,
) -> Result<Option<PersonalAgentRetrievalExpectedHeadV1>, PersonalAgentRetrievalError> {
    if head.generation == 0 && head.latest_receipt_digest.is_none() {
        return Ok(None);
    }
    let receipt_digest =
        head.latest_receipt_digest
            .clone()
            .ok_or(PersonalAgentRetrievalError::Conflict(
                "non-empty destination head digest",
            ))?;
    Ok(Some(PersonalAgentRetrievalExpectedHeadV1 {
        generation: head.generation,
        receipt_digest,
    }))
}

#[cfg(test)]
fn validate_query(
    query: &PersonalAgentRetrievalQueryV1,
    provider: &PersonalAgentRetrievalProviderIdentityV1,
    now_ms: i64,
) -> Result<(), PersonalAgentRetrievalError> {
    validate_agent_identifier(&query.target_agent_id)?;
    validate_token("installation_id", &query.installation_id)?;
    if let Some(goal_id) = query.target_goal_id.as_deref() {
        validate_token("target_goal_id", goal_id)?;
    }
    if now_ms <= 0
        || query.grant_revision == 0
        || query.installation_generation == 0
        || query.maximum_results == 0
        || query.maximum_results > MAX_QUERY_RESULTS
        || &query.provider != provider
    {
        return validation("retrieval query has an invalid bound or provider identity");
    }
    validate_digest(&query.package_content_digest)?;
    validate_digest(&query.grant_authority_digest)
}

fn source_head_key(
    installation_id: &str,
    scope_binding_ref: &str,
    canonical_source_ref: &str,
    dedupe_key: &str,
) -> Result<String, PersonalAgentRetrievalError> {
    digest_serialized(
        "magician.personal-agent-retrieval.source-head.v1",
        &(
            installation_id,
            scope_binding_ref,
            canonical_source_ref,
            dedupe_key,
        ),
    )
}

fn projection_digest(
    projection: &StoredRetrievalProjectionV1,
) -> Result<String, PersonalAgentRetrievalError> {
    let mut logical = projection.clone();
    logical.latest_receipt_digest = None;
    digest_serialized("magician.personal-agent-retrieval.projection.v1", &logical)
}

fn owner_digest(owner: &RetrievalOwnerIdentityV1) -> Result<String, PersonalAgentRetrievalError> {
    digest_serialized("magician.personal-agent-retrieval.owner.v1", owner)
}

fn receipt_digest(
    receipt: &PersonalAgentRetrievalReceiptV1,
) -> Result<String, PersonalAgentRetrievalError> {
    let mut unsigned = receipt.clone();
    unsigned.receipt_digest.clear();
    digest_serialized("magician.personal-agent-retrieval.receipt.v1", &unsigned)
}

fn digest_serialized<T: Serialize>(
    domain: &str,
    value: &T,
) -> Result<String, PersonalAgentRetrievalError> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(&bytes);
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn validate_token(field: &'static str, value: &str) -> Result<(), PersonalAgentRetrievalError> {
    if value.is_empty()
        || value.len() > 192
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return validation(format!("invalid {field}"));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), PersonalAgentRetrievalError> {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return validation("digest lacks the blake3 prefix");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return validation("digest is not canonical lowercase blake3");
    }
    Ok(())
}

fn validation<T>(message: impl Into<String>) -> Result<T, PersonalAgentRetrievalError> {
    Err(PersonalAgentRetrievalError::Validation(message.into()))
}

async fn write_immutable_receipt(
    storage: &AgentStorage,
    paths: &RetrievalOwnerPaths,
    receipt: &PersonalAgentRetrievalReceiptV1,
) -> Result<(), PersonalAgentRetrievalError> {
    let path = paths.receipt(receipt.generation);
    let bytes = serde_json::to_vec(receipt)?;
    match storage
        .read_private_bytes_bounded(&path, APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES)
        .await
    {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {
            return Err(PersonalAgentRetrievalError::Conflict(
                "immutable receipt generation",
            ));
        },
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    storage.write_private_bytes_atomic(path, &bytes).await?;
    Ok(())
}

async fn read_receipt(
    storage: &AgentStorage,
    paths: &RetrievalOwnerPaths,
    owner: &RetrievalOwnerIdentityV1,
    generation: u64,
) -> Result<PersonalAgentRetrievalReceiptV1, PersonalAgentRetrievalError> {
    let bytes = storage
        .read_private_bytes_bounded(
            paths.receipt(generation),
            APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES,
        )
        .await?;
    let receipt: PersonalAgentRetrievalReceiptV1 = serde_json::from_slice(&bytes)?;
    validate_receipt(&receipt, owner)?;
    if receipt.generation != generation {
        return Err(PersonalAgentRetrievalError::Conflict(
            "receipt filename generation",
        ));
    }
    Ok(receipt)
}

async fn read_optional_receipt(
    storage: &AgentStorage,
    paths: &RetrievalOwnerPaths,
    owner: &RetrievalOwnerIdentityV1,
    generation: u64,
) -> Result<Option<PersonalAgentRetrievalReceiptV1>, PersonalAgentRetrievalError> {
    match read_receipt(storage, paths, owner, generation).await {
        Ok(value) => Ok(Some(value)),
        Err(PersonalAgentRetrievalError::Storage(AgentStorageError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        },
        Err(error) => Err(error),
    }
}

async fn read_optional_private_json<T: for<'de> Deserialize<'de>>(
    storage: &AgentStorage,
    path: &Path,
    maximum_bytes: usize,
) -> Result<Option<T>, PersonalAgentRetrievalError> {
    match storage
        .read_private_bytes_bounded(path, maximum_bytes)
        .await
    {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        },
        Err(error) => Err(error.into()),
    }
}

async fn write_private_json<T: Serialize>(
    storage: &AgentStorage,
    path: &Path,
    value: &T,
    maximum_bytes: usize,
) -> Result<(), PersonalAgentRetrievalError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > maximum_bytes {
        return validation("private retrieval owner file exceeds its byte ceiling");
    }
    storage.write_private_bytes_atomic(path, &bytes).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use magician_app_contract::contribution::{
        AppContributionClassification, AppContributionEvidenceClass,
        AppContributionHandlingLabelsV1, AppContributionModelProcessing,
        AppContributionSettlementRefV1, AppContributionSourceHeaderV1, AppContributionSourceRefV1,
        AppMemoryInvalidationReasonV1,
    };
    use tempfile::TempDir;

    use crate::magician_v2::apps::{
        models::{AppReference, AppScopeBindingRef},
        records::AppScope,
    };

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn provider_identity(label: &str) -> PersonalAgentRetrievalProviderIdentityV1 {
        PersonalAgentRetrievalProviderIdentityV1::from_trusted_provider(
            format!("provider:{label}"),
            1,
            digest(&format!("provider-authority:{label}")),
            digest("projection-schema"),
        )
        .expect("provider")
    }

    fn proposal(source_revision: u64, text: &str) -> AppPersonalAgentRetrievalProjectionProposalV1 {
        let source_digest = digest(&format!("source:{source_revision}"));
        AppPersonalAgentRetrievalProjectionProposalV1 {
            header: AppContributionSourceHeaderV1 {
                contract_version: 1,
                destination_contract_id: APP_RETRIEVAL_PROJECTION_CONTRACT_ID.to_owned(),
                destination_contract_version: 1,
                destination_schema_digest: digest("projection-schema"),
                proposal_id: format!("retrieval-proposal:{source_revision}"),
                proposal_revision: source_revision,
                scope_binding_ref: "scope-binding-1".to_owned(),
                installation_id: "installation:1".to_owned(),
                installation_generation: 2,
                package_revision_ref: "package:1".to_owned(),
                package_content_digest: digest("package"),
                grant_revision: 3,
                grant_authority_digest: digest("grant"),
                schema_revision: 4,
                schema_digest: digest("schema"),
                workflow_id: "workflow:project".to_owned(),
                workflow_digest: digest("workflow"),
                action_id: "action:project".to_owned(),
                action_digest: digest("action"),
                contribution_port_id: "personal_agent_retrieval".to_owned(),
                contribution_port_digest: digest("port"),
                settlement: AppContributionSettlementRefV1::TypedResult {
                    result_ref: "result:1".to_owned(),
                    output_revision: source_revision,
                    result_digest: digest(&format!("result:{source_revision}")),
                },
                sources: vec![AppContributionSourceRefV1 {
                    installation_id: "installation:1".to_owned(),
                    entity_name: "research_note".to_owned(),
                    record_id: "record:1".to_owned(),
                    record_revision: source_revision,
                    selected_fields: vec!["summary".to_owned()],
                    canonical_source_ref: "source:record:1".to_owned(),
                    canonical_source_digest: source_digest,
                    handling_labels: AppContributionHandlingLabelsV1 {
                        classification: AppContributionClassification::Personal,
                        model_processing: AppContributionModelProcessing::LocalOnly,
                        policy_digest: digest("source-policy"),
                        provenance_digest: digest("source-provenance"),
                    },
                }],
                handling_labels: AppContributionHandlingLabelsV1 {
                    classification: AppContributionClassification::Personal,
                    model_processing: AppContributionModelProcessing::LocalOnly,
                    policy_digest: digest("policy"),
                    provenance_digest: digest("provenance"),
                },
                purpose: "personal-agent-retrieval".to_owned(),
                audiences: vec!["personal-agent".to_owned()],
                evidence_class: AppContributionEvidenceClass::Authoritative,
                issued_at_ms: Utc::now().timestamp_millis() - 1_000,
                expires_at_ms: Utc::now().timestamp_millis() + 60_000,
                dedupe_key: "retrieval:record:1".to_owned(),
                update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
                retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
            },
            target_agent_id: "personal-assistant".to_owned(),
            target_goal_id: None,
            projection_text: text.to_owned(),
            projection_digest: content_digest(text.as_bytes()),
            proposal_digest: String::new(),
        }
        .seal()
        .expect("proposal")
    }

    fn fence(
        provider: PersonalAgentRetrievalProviderIdentityV1,
    ) -> PersonalAgentRetrievalGrantFenceV1 {
        PersonalAgentRetrievalGrantFenceV1::from_reviewed_grant(
            "installation:1",
            2,
            digest("package"),
            3,
            digest("grant"),
            digest("port"),
            "personal-assistant",
            None,
            provider,
        )
        .expect("fence")
    }

    fn setup() -> (
        TempDir,
        PersonalAgentRetrievalOwner,
        AuthenticatedAppScope,
        PersonalAgentRetrievalProviderIdentityV1,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp
            .path()
            .join("scopes")
            .join("principal-a")
            .join("workspace-a")
            .join("agent_runtime");
        std::fs::create_dir_all(&root).expect("root");
        let provider = provider_identity("one");
        let owner = PersonalAgentRetrievalOwner::new(
            AgentStorage::with_scoped_memory_root(&root),
            "principal-a",
            "workspace-a",
            "scope-binding-1",
            provider.clone(),
        )
        .expect("owner");
        let issued_at = Utc::now() - Duration::minutes(1);
        let auth = AuthenticatedAppScope::from_system_worker(
            AppScope {
                principal: AppReference::parse("principal-a").expect("principal"),
                workspace: AppReference::parse("workspace-a").expect("workspace"),
            },
            AppScopeBindingRef::parse("scope-binding-1").expect("binding"),
            AppReference::parse("worker:retrieval-test").expect("worker"),
            AppReference::parse("run:retrieval-test").expect("run"),
            issued_at,
            issued_at + Duration::minutes(5),
        )
        .expect("auth");
        (temp, owner, auth, provider)
    }

    #[tokio::test]
    async fn owner_change_reseeds_an_empty_head_but_never_a_sealed_one() {
        let (_temp, first, auth, _provider) = setup();
        first
            .recover(&auth)
            .await
            .expect("first recovery seeds the head");
        let paths = RetrievalOwnerPaths::new(first.storage.root());
        let second = PersonalAgentRetrievalOwner::new(
            AgentStorage::with_scoped_memory_root(first.storage.root()),
            "principal-a",
            "workspace-a",
            "scope-binding-1",
            provider_identity("two"),
        )
        .expect("owner under the next provider revision");

        // Generation 0 under the previous owner: nothing to lose, re-seed.
        second
            .recover(&auth)
            .await
            .expect("an empty head adopts the current owner");
        let head = read_optional_private_json::<RetrievalDestinationHeadV1>(
            &second.storage,
            &paths.head,
            MAX_HEAD_BYTES,
        )
        .await
        .expect("head readable")
        .expect("head present");
        assert_eq!(
            head.owner_identity_digest,
            owner_digest(&second.owner).expect("owner digest")
        );

        // A sealed head under another owner still fails closed.
        let sealed = RetrievalDestinationHeadV1 {
            schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
            owner_identity_digest: owner_digest(&first.owner).expect("owner digest"),
            generation: 1,
            last_recorded_at_ms: 1,
            latest_receipt_digest: Some(digest("receipt")),
        };
        write_private_json(&first.storage, &paths.head, &sealed, MAX_HEAD_BYTES)
            .await
            .expect("sealed head written");
        assert!(matches!(
            second.recover(&auth).await,
            Err(PersonalAgentRetrievalError::Conflict(
                "destination head identity"
            ))
        ));
    }

    #[tokio::test]
    async fn exact_replay_is_idempotent_and_provider_substitution_fails_closed() {
        let (_temp, owner, auth, provider) = setup();
        let initial_proposal = proposal(1, "Bounded projection");
        let recorded_at_ms = Utc::now().timestamp_millis();
        let first = owner
            .stage_projection(
                &auth,
                fence(provider.clone()),
                initial_proposal.clone(),
                None,
                recorded_at_ms,
            )
            .await
            .expect("stage");
        let PersonalAgentRetrievalApplyOutcomeV1::Applied(receipt) = first else {
            panic!("first stage must append")
        };
        let replay = owner
            .stage_projection(
                &auth,
                fence(provider.clone()),
                initial_proposal,
                None,
                recorded_at_ms,
            )
            .await
            .expect("replay");
        assert!(matches!(
            replay,
            PersonalAgentRetrievalApplyOutcomeV1::ExactReplay(value)
                if value.receipt_digest == receipt.receipt_digest
        ));

        let substituted = fence(provider_identity("two"));
        assert!(owner
            .stage_projection(
                &auth,
                substituted,
                proposal(2, "Substituted provider"),
                Some(PersonalAgentRetrievalExpectedHeadV1 {
                    generation: receipt.generation,
                    receipt_digest: receipt.receipt_digest,
                }),
                recorded_at_ms + 1,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn query_revalidates_grant_and_expiry_without_memory_conversion() {
        let (_temp, owner, auth, provider) = setup();
        let proposal = proposal(1, "Retrieval-only text");
        let recorded_at_ms = Utc::now().timestamp_millis();
        let expires_at_ms = proposal.header.expires_at_ms;
        owner
            .stage_projection(
                &auth,
                fence(provider.clone()),
                proposal,
                None,
                recorded_at_ms,
            )
            .await
            .expect("stage");
        let live = owner
            .query_projection(
                &auth,
                PersonalAgentRetrievalQueryV1 {
                    target_agent_id: "personal-assistant".to_owned(),
                    target_goal_id: None,
                    installation_id: "installation:1".to_owned(),
                    installation_generation: 2,
                    package_content_digest: digest("package"),
                    grant_revision: 3,
                    grant_authority_digest: digest("grant"),
                    provider: provider.clone(),
                    maximum_results: 8,
                },
                recorded_at_ms,
            )
            .await
            .expect("query");
        assert_eq!(live.entries.len(), 1);
        assert_eq!(
            live.entries[0].proposal.projection_text,
            "Retrieval-only text"
        );

        let expired = owner
            .query_projection(
                &auth,
                PersonalAgentRetrievalQueryV1 {
                    target_agent_id: "personal-assistant".to_owned(),
                    target_goal_id: None,
                    installation_id: "installation:1".to_owned(),
                    installation_generation: 2,
                    package_content_digest: digest("package"),
                    grant_revision: 3,
                    grant_authority_digest: digest("grant"),
                    provider,
                    maximum_results: 8,
                },
                expires_at_ms,
            )
            .await
            .expect("expired query");
        assert!(expired.entries.is_empty());
    }

    #[tokio::test]
    async fn invalidation_removes_query_projection_and_rollback_is_rejected() {
        let (_temp, owner, auth, provider) = setup();
        let proposal = proposal(1, "Projection");
        let recorded_at_ms = Utc::now().timestamp_millis();
        let staged = owner
            .stage_projection(
                &auth,
                fence(provider.clone()),
                proposal.clone(),
                None,
                recorded_at_ms,
            )
            .await
            .expect("stage");
        let PersonalAgentRetrievalApplyOutcomeV1::Applied(staged) = staged else {
            panic!("stage")
        };
        let source = &proposal.header.sources[0];
        let invalidation = AppMemoryInvalidationV1 {
            contract_version: 1,
            invalidation_id: "invalidation:1".to_owned(),
            installation_id: proposal.header.installation_id.clone(),
            scope_binding_ref: proposal.header.scope_binding_ref.clone(),
            proposal_id: proposal.header.proposal_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            source_event_ref: source.canonical_source_ref.clone(),
            source_event_revision: 2,
            source_identity_digest: source.canonical_source_digest.clone(),
            dedupe_key: proposal.header.dedupe_key.clone(),
            reason: AppMemoryInvalidationReasonV1::SourceDeleted,
            issued_at_ms: recorded_at_ms + 1,
            invalidation_digest: String::new(),
        }
        .seal()
        .expect("invalidation");
        let invalidated = owner
            .invalidate_projection(
                &auth,
                fence(provider.clone()),
                invalidation,
                Some(PersonalAgentRetrievalExpectedHeadV1 {
                    generation: staged.generation,
                    receipt_digest: staged.receipt_digest,
                }),
                recorded_at_ms + 2,
            )
            .await
            .expect("invalidate");
        let PersonalAgentRetrievalApplyOutcomeV1::Applied(invalidated) = invalidated else {
            panic!("invalidate")
        };
        assert_eq!(
            invalidated.invalidation_disposition,
            Some(AppMemoryInvalidationDispositionV1::Tombstoned)
        );
        assert!(owner
            .stage_projection(
                &auth,
                fence(provider.clone()),
                proposal,
                Some(PersonalAgentRetrievalExpectedHeadV1 {
                    generation: invalidated.generation,
                    receipt_digest: invalidated.receipt_digest,
                }),
                recorded_at_ms + 3,
            )
            .await
            .is_err());

        let query = owner
            .query_projection(
                &auth,
                PersonalAgentRetrievalQueryV1 {
                    target_agent_id: "personal-assistant".to_owned(),
                    target_goal_id: None,
                    installation_id: "installation:1".to_owned(),
                    installation_generation: 2,
                    package_content_digest: digest("package"),
                    grant_revision: 3,
                    grant_authority_digest: digest("grant"),
                    provider,
                    maximum_results: 8,
                },
                recorded_at_ms + 3,
            )
            .await
            .expect("query after invalidation");
        assert!(query.entries.is_empty());
    }

    #[tokio::test]
    async fn recovery_adopts_one_orphan_receipt_but_rejects_a_longer_rollback() {
        let (_temp, owner, auth, provider) = setup();
        let recorded_at_ms = Utc::now().timestamp_millis();
        let first = owner
            .stage_projection(
                &auth,
                fence(provider.clone()),
                proposal(1, "First projection"),
                None,
                recorded_at_ms,
            )
            .await
            .expect("first stage");
        let PersonalAgentRetrievalApplyOutcomeV1::Applied(first) = first else {
            panic!("first stage")
        };
        let paths = RetrievalOwnerPaths::new(owner.storage.root());
        let empty_head = RetrievalDestinationHeadV1 {
            schema_version: RETRIEVAL_OWNER_SCHEMA_VERSION,
            owner_identity_digest: owner_digest(&owner.owner).expect("owner digest"),
            generation: 0,
            last_recorded_at_ms: 0,
            latest_receipt_digest: None,
        };
        write_private_json(&owner.storage, &paths.head, &empty_head, MAX_HEAD_BYTES)
            .await
            .expect("reset head");
        write_private_json(
            &owner.storage,
            &paths.projection,
            &empty_projection(owner.owner.clone()),
            MAX_PROJECTION_BYTES,
        )
        .await
        .expect("reset projection");
        let recovered = owner
            .recover(&auth)
            .await
            .expect("recover one orphan")
            .expect("non-empty recovered head");
        assert_eq!(recovered.generation, first.generation);
        assert_eq!(recovered.receipt_digest, first.receipt_digest);

        let second = owner
            .stage_projection(
                &auth,
                fence(provider),
                proposal(2, "Second projection"),
                Some(recovered),
                recorded_at_ms + 1,
            )
            .await
            .expect("second stage");
        assert!(matches!(
            second,
            PersonalAgentRetrievalApplyOutcomeV1::Applied(_)
        ));
        write_private_json(&owner.storage, &paths.head, &empty_head, MAX_HEAD_BYTES)
            .await
            .expect("rollback head");
        write_private_json(
            &owner.storage,
            &paths.projection,
            &empty_projection(owner.owner.clone()),
            MAX_PROJECTION_BYTES,
        )
        .await
        .expect("rollback projection");
        assert!(owner.recover(&auth).await.is_err());
    }
}
