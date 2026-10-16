//! Destination owner for app-proposed memory candidates.
//!
//! Apps never call this owner. The governed projection worker moves typed source
//! leases into it, while owner decisions require the separately signed,
//! head-bound desktop envelope. The durable order is immutable receipt -> head
//! -> projection. Recovery may advance one exact orphan receipt and may rebuild
//! at most `MAX_RECOVERY_RECEIPTS` receipts.
//!
//! Lock order is fixed: in-process `AgentMemoryService` contribution mutex ->
//! private cross-process owner lock -> bounded file reads/writes. No registry
//! transaction or source outbox lease is held while the destination lock is
//! acquired; the source is acknowledged only after this owner returns.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::Utc;
use magician_app_contract::contribution::{
    content_digest, AppMemoryCandidateProposalV1, AppMemoryDestinationOperationV1,
    AppMemoryDestinationReceiptV1, AppMemoryIngressDispositionV1, AppMemoryIngressReceiptV1,
    AppMemoryInvalidationDispositionV1, AppMemoryInvalidationReasonV1,
    AppMemoryInvalidationReceiptV1, AppMemoryOwnerDecisionEnvelopeV1, AppMemoryOwnerDecisionV1,
    AppMemoryOwnerReviewV1, APP_CONTRIBUTION_CONTRACT_VERSION,
    APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES,
};
use magician_app_contract::macos_host::app_macos_desktop_identity_digest;
use serde::{Deserialize, Serialize};

use crate::magician_v2::apps::authority::AuthenticatedAppScope;
use crate::magician_v2::apps::contribution::invalidation_advances_exact_source;
use crate::magician_v2::apps::memory_contribution_outbox::{
    AppMemoryContributionDispatchAck, AppMemoryContributionDispatchPermit,
    AppMemoryExpectedDestinationHead, AppMemoryInvalidationDispatchAck,
    AppMemoryInvalidationDispatchPermit,
};

use super::{
    memory::{AgentMemoryError, AgentMemoryService},
    storage::{AgentStorage, AgentStorageError},
};

const APP_MEMORY_OWNER_SCHEMA: u16 = 1;
const MAX_PROJECTION_ENTRIES: usize = 4_096;
const MAX_SOURCE_DISPATCH_SLOTS: usize = MAX_PROJECTION_ENTRIES * 2;
const MAX_RECOVERY_RECEIPTS: u64 = 64;
const MAX_RECENT_OPERATION_REPLAY: u64 = 64;
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_LIVE_ENTRY_BYTES: usize = 4 * 1024 * 1024;
const MAX_COMPACTED_SOURCE_HEAD_BYTES: usize = 8 * 1024;
const MAX_REPLAY_METADATA_BYTES: usize = 1024 * 1024;
const MAX_PROJECTION_BYTES: usize = MAX_LIVE_ENTRY_BYTES
    + MAX_PROJECTION_ENTRIES * MAX_COMPACTED_SOURCE_HEAD_BYTES
    + MAX_REPLAY_METADATA_BYTES;
const MAX_CHECKPOINT_BYTES: usize = MAX_PROJECTION_BYTES + 64 * 1024;
const MAX_SOURCE_DISPATCH_MATERIAL_BYTES: usize = 8 * 1024;
pub const APP_MEMORY_CONTRIBUTION_STATE_MAX_ITEMS: usize = 32;
pub const APP_MEMORY_CONTRIBUTION_STATE_MAX_BYTES: usize = 512 * 1024;

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryProjectionStateV1 {
    Proposed,
    Accepted,
    Rejected,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryContributionStateV1 {
    Proposed,
    Accepted,
    Rejected,
    Stale,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryContributionStateReasonV1 {
    AwaitingOwnerReview,
    OwnerAccepted,
    OwnerRejected,
    OwnerRevoked,
    SourceInvalidated,
    CompactedLegacy,
}

/// Bounded owner-facing projection of the canonical destination state. Live
/// rows retain their reviewed text; compact terminal rows intentionally expose
/// only source identity and reason. `revoke_review` is a complete, head-bound
/// document and is useful only to the code-verified desktop signer.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryContributionStateItemV1 {
    pub proposal_id: String,
    pub proposal_digest: String,
    pub installation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contribution_port_id: Option<String>,
    pub source_event_ref: String,
    pub source_event_revision: u64,
    pub state: AppMemoryContributionStateV1,
    pub reason: AppMemoryContributionStateReasonV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_invalidation_reason: Option<AppMemoryInvalidationReasonV1>,
    pub state_changed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_or_summary: Option<String>,
    pub details_compacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_review: Option<AppMemoryOwnerReviewV1>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryContributionStateSnapshotV1 {
    pub destination_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_receipt_digest: Option<String>,
    pub items: Vec<AppMemoryContributionStateItemV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryProjectionEntryV1 {
    pub proposal: AppMemoryCandidateProposalV1,
    pub state: AppMemoryProjectionStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_decision_receipt_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_digest: Option<String>,
    pub source_event_revision: u64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub state_changed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryDestinationProjectionV1 {
    pub schema_version: u16,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_receipt_digest: Option<String>,
    /// Generations at or below this floor were checkpointed and no longer
    /// promise byte-identical operation replay.
    pub replay_floor_generation: u64,
    pub entries: BTreeMap<String, AppMemoryProjectionEntryV1>,
    /// Exact invalidation high-water that arrived before its proposal. The key
    /// is a domain-separated digest of proposal/source/dedupe identity.
    pub invalidation_high_water: BTreeMap<String, AppMemoryInvalidationHighWaterV1>,
    /// Latest source-dispatched operation and compact receipt material for each
    /// exact source head. This survives receipt-window pruning so response-loss
    /// retries can re-mint the identical source acknowledgement.
    pub source_dispatch_high_water: BTreeMap<String, AppMemorySourceDispatchHighWaterV1>,
    /// Operation digest -> generation. Values contain no receipt digest, so the
    /// logical projection digest cannot become circular.
    pub applied_operations: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryInvalidationHighWaterV1 {
    pub installation_id: String,
    pub scope_binding_ref: String,
    pub proposal_id: String,
    pub proposal_digest: String,
    pub source_identity_digest: String,
    pub source_event_ref: String,
    pub dedupe_key: String,
    pub source_event_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_digest: Option<String>,
    pub disposition: AppMemoryInvalidationDispositionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<AppMemoryContributionStateReasonV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_invalidation_reason: Option<AppMemoryInvalidationReasonV1>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub state_changed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_issued_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contribution_port_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemorySourceDispatchHighWaterV1 {
    pub schema_version: u16,
    pub source_head_key: String,
    pub dispatch_slot_key: String,
    pub operation_digest: String,
    pub generation: u64,
    pub receipt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidation_disposition: Option<AppMemoryInvalidationDispositionV1>,
    pub resulting_projection_digest: String,
    pub recorded_at_ms: i64,
    pub receipt_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_predecessor_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_predecessor_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppMemoryDestinationHeadV1 {
    schema_version: u16,
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_receipt_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppMemoryDestinationCheckpointV1 {
    schema_version: u16,
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_receipt_digest: Option<String>,
    projection: AppMemoryDestinationProjectionV1,
    projection_digest: String,
    source_high_water_digest: String,
    checkpoint_digest: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppMemoryDestinationApplyResult {
    Proposal {
        destination_receipt: AppMemoryDestinationReceiptV1,
        source_ack: AppMemoryIngressAckPermit,
    },
    Decision {
        destination_receipt: AppMemoryDestinationReceiptV1,
    },
    Invalidation {
        destination_receipt: AppMemoryDestinationReceiptV1,
        source_ack: AppMemoryInvalidationAckPermit,
    },
}

/// Move-only proof that a source proposal may be acknowledged. It has no
/// Deserialize/Clone implementation and only this destination owner can mint
/// it. The sealed receipt inside is evidence, not bearer authentication.
#[derive(Debug, PartialEq, Eq)]
pub struct AppMemoryIngressAckPermit {
    receipt: AppMemoryIngressReceiptV1,
    dispatch_ack: Option<AppMemoryContributionDispatchAck>,
}

impl AppMemoryIngressAckPermit {
    pub(crate) fn into_parts(
        self,
    ) -> Result<(AppMemoryIngressReceiptV1, AppMemoryContributionDispatchAck), &'static str> {
        Ok((
            self.receipt,
            self.dispatch_ack
                .ok_or("app-memory acknowledgement lacks its owned dispatch identity")?,
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AppMemoryInvalidationAckPermit {
    receipt: AppMemoryInvalidationReceiptV1,
    dispatch_ack: Option<AppMemoryInvalidationDispatchAck>,
}

impl AppMemoryInvalidationAckPermit {
    pub(crate) fn into_parts(
        self,
    ) -> Result<
        (
            AppMemoryInvalidationReceiptV1,
            AppMemoryInvalidationDispatchAck,
        ),
        &'static str,
    > {
        Ok((
            self.receipt,
            self.dispatch_ack.ok_or(
                "app-memory invalidation acknowledgement lacks its owned dispatch identity",
            )?,
        ))
    }
}

impl AgentMemoryService {
    pub(crate) async fn pending_app_memory_owner_reviews(
        &self,
        authenticated: &AuthenticatedAppScope,
        desktop_identity_key_id: &str,
        desktop_identity_digest: &str,
        limit: usize,
        now_ms: i64,
    ) -> Result<Vec<AppMemoryOwnerReviewV1>, AgentMemoryError> {
        authenticated.ensure_live_at(&Utc::now()).map_err(|error| {
            AgentMemoryError::Validation(format!(
                "app-memory owner review scope is not live: {error}"
            ))
        })?;
        let (principal, workspace) = self.scoped_memory_scope().ok_or_else(|| {
            AgentMemoryError::Validation(
                "app-memory owner review requires a scoped destination".to_owned(),
            )
        })?;
        if authenticated.scope().principal.as_str() != principal
            || authenticated.scope().workspace.as_str() != workspace
            || limit == 0
            || limit > magician_app_contract::contribution::APP_MEMORY_OWNER_REVIEW_MAX_ITEMS
        {
            return Err(AgentMemoryError::Validation(
                "app-memory owner review scope or limit is invalid".to_owned(),
            ));
        }
        let projection = self.recover_app_memory_destination().await?;
        let mut reviews = Vec::with_capacity(limit);
        let mut response_bytes = 32usize;
        for entry in projection.entries.values() {
            if reviews.len() == limit {
                break;
            }
            if entry.state != AppMemoryProjectionStateV1::Proposed
                || entry.proposal.header.scope_binding_ref
                    != authenticated.scope_binding_ref().as_str()
                || entry.proposal.header.expires_at_ms <= now_ms
            {
                continue;
            }
            let review = AppMemoryOwnerReviewV1::mint(
                projection.generation,
                projection.latest_receipt_digest.clone(),
                desktop_identity_key_id.to_owned(),
                desktop_identity_digest.to_owned(),
                entry.proposal.clone(),
            )
            .map_err(contract_error)?;
            let next_bytes = serde_json::to_vec(&review)?.len();
            if response_bytes.saturating_add(next_bytes).saturating_add(1)
                > magician_app_contract::contribution::APP_MEMORY_OWNER_REVIEW_LIST_MAX_BYTES
            {
                break;
            }
            response_bytes = response_bytes
                .checked_add(next_bytes.saturating_add(1))
                .ok_or_else(|| {
                    AgentMemoryError::Validation(
                        "app-memory owner review response size overflowed".to_owned(),
                    )
                })?;
            reviews.push(review);
        }
        Ok(reviews)
    }

    pub(crate) async fn app_memory_contribution_state(
        &self,
        authenticated: &AuthenticatedAppScope,
        desktop_identity: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<AppMemoryContributionStateSnapshotV1, AgentMemoryError> {
        authenticated.ensure_live_at(&Utc::now()).map_err(|error| {
            AgentMemoryError::Validation(format!("app-memory state scope is not live: {error}"))
        })?;
        let (principal, workspace) = self.scoped_memory_scope().ok_or_else(|| {
            AgentMemoryError::Validation(
                "app-memory state requires a scoped destination".to_owned(),
            )
        })?;
        if authenticated.scope().principal.as_str() != principal
            || authenticated.scope().workspace.as_str() != workspace
            || limit == 0
            || limit > APP_MEMORY_CONTRIBUTION_STATE_MAX_ITEMS
        {
            return Err(AgentMemoryError::Validation(
                "app-memory state scope or limit is invalid".to_owned(),
            ));
        }
        let projection = self.recover_app_memory_destination().await?;
        let mut items = Vec::with_capacity(limit);
        for entry in projection.entries.values() {
            if entry.proposal.header.scope_binding_ref != authenticated.scope_binding_ref().as_str()
            {
                continue;
            }
            let source = entry.proposal.header.sources.first().ok_or_else(|| {
                AgentMemoryError::Validation(
                    "app-memory state entry lost its exact source".to_owned(),
                )
            })?;
            let accepted = entry.state == AppMemoryProjectionStateV1::Accepted;
            let revoke_review = if accepted {
                match desktop_identity {
                    Some((key_id, digest)) => Some(
                        AppMemoryOwnerReviewV1::mint(
                            projection.generation,
                            projection.latest_receipt_digest.clone(),
                            key_id.to_owned(),
                            digest.to_owned(),
                            entry.proposal.clone(),
                        )
                        .map_err(contract_error)?,
                    ),
                    None => None,
                }
            } else {
                None
            };
            items.push(AppMemoryContributionStateItemV1 {
                proposal_id: entry.proposal.header.proposal_id.clone(),
                proposal_digest: entry.proposal.proposal_digest.clone(),
                installation_id: entry.proposal.header.installation_id.clone(),
                workflow_id: Some(entry.proposal.header.workflow_id.clone()),
                action_id: Some(entry.proposal.header.action_id.clone()),
                contribution_port_id: Some(entry.proposal.header.contribution_port_id.clone()),
                source_event_ref: source.canonical_source_ref.clone(),
                source_event_revision: entry.source_event_revision,
                state: if accepted {
                    AppMemoryContributionStateV1::Accepted
                } else {
                    AppMemoryContributionStateV1::Proposed
                },
                reason: if accepted {
                    AppMemoryContributionStateReasonV1::OwnerAccepted
                } else {
                    AppMemoryContributionStateReasonV1::AwaitingOwnerReview
                },
                source_invalidation_reason: None,
                state_changed_at_ms: entry.state_changed_at_ms,
                expires_at_ms: Some(entry.proposal.header.expires_at_ms),
                retained_until_ms: entry.retained_until_ms,
                claim_or_summary: Some(entry.proposal.claim_or_summary.clone()),
                details_compacted: false,
                revoke_review,
            });
        }
        for high_water in projection.invalidation_high_water.values() {
            if high_water.scope_binding_ref != authenticated.scope_binding_ref().as_str() {
                continue;
            }
            let reason = high_water
                .terminal_reason
                .unwrap_or(AppMemoryContributionStateReasonV1::CompactedLegacy);
            let state = match (reason, high_water.source_invalidation_reason) {
                (AppMemoryContributionStateReasonV1::OwnerRejected, _) => {
                    AppMemoryContributionStateV1::Rejected
                },
                (
                    AppMemoryContributionStateReasonV1::SourceInvalidated,
                    Some(
                        AppMemoryInvalidationReasonV1::SourceUpdated
                        | AppMemoryInvalidationReasonV1::SourceRestored
                        | AppMemoryInvalidationReasonV1::PolicyChanged,
                    ),
                ) => AppMemoryContributionStateV1::Stale,
                _ => AppMemoryContributionStateV1::Tombstoned,
            };
            items.push(AppMemoryContributionStateItemV1 {
                proposal_id: high_water.proposal_id.clone(),
                proposal_digest: high_water.proposal_digest.clone(),
                installation_id: high_water.installation_id.clone(),
                workflow_id: high_water.workflow_id.clone(),
                action_id: high_water.action_id.clone(),
                contribution_port_id: high_water.contribution_port_id.clone(),
                source_event_ref: high_water.source_event_ref.clone(),
                source_event_revision: high_water.source_event_revision,
                state,
                reason,
                source_invalidation_reason: high_water.source_invalidation_reason,
                state_changed_at_ms: high_water.state_changed_at_ms,
                expires_at_ms: high_water.proposal_expires_at_ms,
                retained_until_ms: None,
                claim_or_summary: None,
                details_compacted: true,
                revoke_review: None,
            });
        }
        items.sort_by(|left, right| {
            right
                .state_changed_at_ms
                .cmp(&left.state_changed_at_ms)
                .then_with(|| left.proposal_digest.cmp(&right.proposal_digest))
        });
        items.truncate(limit);
        let mut snapshot = AppMemoryContributionStateSnapshotV1 {
            destination_generation: projection.generation,
            destination_receipt_digest: projection.latest_receipt_digest,
            items,
        };
        while snapshot.items.len() > 1
            && serde_json::to_vec(&snapshot)?.len() > APP_MEMORY_CONTRIBUTION_STATE_MAX_BYTES
        {
            snapshot.items.pop();
        }
        if serde_json::to_vec(&snapshot)?.len() > APP_MEMORY_CONTRIBUTION_STATE_MAX_BYTES {
            return Err(AgentMemoryError::Validation(
                "app-memory state response exceeds its bounded disclosure ceiling".to_owned(),
            ));
        }
        Ok(snapshot)
    }

    pub(crate) async fn apply_app_memory_owner_decision(
        &self,
        authenticated: &AuthenticatedAppScope,
        desktop_identity_public_key_hex: &str,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_digest: &str,
        envelope: AppMemoryOwnerDecisionEnvelopeV1,
        recorded_at_ms: i64,
    ) -> Result<AppMemoryDestinationApplyResult, AgentMemoryError> {
        authenticated.ensure_live_at(&Utc::now()).map_err(|error| {
            AgentMemoryError::Validation(format!(
                "app-memory owner decision scope is not live: {error}"
            ))
        })?;
        let recomputed_identity = app_macos_desktop_identity_digest(
            expected_desktop_identity_key_id,
            desktop_identity_public_key_hex,
        )
        .map_err(|_| {
            AgentMemoryError::Validation(
                "app-memory desktop identity material is invalid".to_owned(),
            )
        })?;
        if recomputed_identity != expected_desktop_identity_digest
            || envelope.review.desktop_identity_key_id != expected_desktop_identity_key_id
            || envelope.review.desktop_identity_digest != expected_desktop_identity_digest
            || envelope.review.proposal.header.scope_binding_ref
                != authenticated.scope_binding_ref().as_str()
        {
            return Err(AgentMemoryError::Validation(
                "app-memory owner decision is not bound to the current desktop and scope"
                    .to_owned(),
            ));
        }
        envelope
            .verify_signature(desktop_identity_public_key_hex)
            .map_err(contract_error)?;
        let expected_head = if envelope.review.destination_generation == 0 {
            None
        } else {
            Some(AppMemoryExpectedDestinationHead {
                generation: envelope.review.destination_generation,
                receipt_digest: envelope
                    .review
                    .destination_receipt_digest
                    .clone()
                    .ok_or_else(|| {
                        AgentMemoryError::Validation(
                            "app-memory owner review lost its destination head".to_owned(),
                        )
                    })?,
            })
        };
        let decision_expires_at_ms = (envelope.decision != AppMemoryOwnerDecisionV1::Revoke)
            .then_some(envelope.review.proposal.header.expires_at_ms);
        let result = self
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::Decide {
                    proposal_id: envelope.review.proposal.header.proposal_id.clone(),
                    proposal_digest: envelope.review.proposal.proposal_digest.clone(),
                    decision: envelope.decision,
                    owner_decision_receipt_digest: envelope.receipt_digest,
                    retained_until_ms: envelope.retained_until_ms,
                },
                recorded_at_ms,
                expected_head,
                None,
                None,
                decision_expires_at_ms,
            )
            .await?;
        super::memory_prompt_blocks::synchronize_app_memory_index_projection(self, Utc::now())
            .await?;
        Ok(result)
    }

    pub(crate) async fn stage_app_memory_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        permit: AppMemoryContributionDispatchPermit,
        recorded_at_ms: i64,
    ) -> Result<AppMemoryDestinationApplyResult, AgentMemoryError> {
        let evidence = permit.into_evidence(recorded_at_ms).map_err(|error| {
            AgentMemoryError::Validation(format!("invalid app-memory dispatch permit: {error}"))
        })?;
        authenticated.ensure_live_at(&Utc::now()).map_err(|error| {
            AgentMemoryError::Validation(format!(
                "app-memory authenticated scope is not live: {error}"
            ))
        })?;
        let (principal, workspace) = self.scoped_memory_scope().ok_or_else(|| {
            AgentMemoryError::Validation(
                "app-memory contribution requires a scoped destination owner".to_owned(),
            )
        })?;
        if authenticated.scope().principal.as_str() != principal
            || authenticated.scope().workspace.as_str() != workspace
            || !evidence.matches_destination_scope(authenticated)
        {
            return Err(AgentMemoryError::Validation(
                "app-memory contribution scope binding does not match its destination".to_owned(),
            ));
        }
        let result = self
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: evidence.proposal,
                },
                recorded_at_ms,
                evidence.expected_destination_head,
                Some(evidence.dispatch_ack),
                None,
                Some(evidence.absolute_expires_at_ms),
            )
            .await?;
        super::memory_prompt_blocks::synchronize_app_memory_index_projection(self, Utc::now())
            .await?;
        Ok(result)
    }

    pub(crate) async fn invalidate_app_memory_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        permit: AppMemoryInvalidationDispatchPermit,
        recorded_at_ms: i64,
    ) -> Result<AppMemoryDestinationApplyResult, AgentMemoryError> {
        let evidence = permit.into_evidence(recorded_at_ms).map_err(|error| {
            AgentMemoryError::Validation(format!("invalid app-memory dispatch permit: {error}"))
        })?;
        authenticated.ensure_live_at(&Utc::now()).map_err(|error| {
            AgentMemoryError::Validation(format!(
                "app-memory authenticated scope is not live: {error}"
            ))
        })?;
        let (principal, workspace) = self.scoped_memory_scope().ok_or_else(|| {
            AgentMemoryError::Validation(
                "app-memory invalidation requires a scoped destination owner".to_owned(),
            )
        })?;
        if authenticated.scope().principal.as_str() != principal
            || authenticated.scope().workspace.as_str() != workspace
            || !evidence.matches_destination_scope(authenticated)
        {
            return Err(AgentMemoryError::Validation(
                "app-memory invalidation scope binding does not match its destination".to_owned(),
            ));
        }
        let result = self
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::Invalidate {
                    invalidation: evidence.invalidation,
                },
                recorded_at_ms,
                evidence.expected_destination_head,
                None,
                Some(evidence.dispatch_ack),
                Some(evidence.absolute_expires_at_ms),
            )
            .await?;
        super::memory_prompt_blocks::synchronize_app_memory_index_projection(self, Utc::now())
            .await?;
        Ok(result)
    }

    /// Appends one already-governed destination operation. This private core is
    /// not exposed outside the owner module: stage/invalidate require move-only
    /// source permits and decision requires the closed signed owner envelope;
    /// there is no general accept/reject/revoke method.
    async fn apply_app_memory_destination_operation(
        &self,
        operation: AppMemoryDestinationOperationV1,
        recorded_at_ms: i64,
        expected_head: Option<AppMemoryExpectedDestinationHead>,
        proposal_dispatch_ack: Option<AppMemoryContributionDispatchAck>,
        invalidation_dispatch_ack: Option<AppMemoryInvalidationDispatchAck>,
        absolute_expires_at_ms: Option<i64>,
    ) -> Result<AppMemoryDestinationApplyResult, AgentMemoryError> {
        if recorded_at_ms < 0 {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt time cannot be negative".to_owned(),
            ));
        }
        validate_operation(&operation)?;
        let operation_digest = digest_serialized("magician.app-memory-operation.v1", &operation)?;
        let in_process_lock = self.app_contribution_lock();
        let _in_process_guard = in_process_lock.lock().await;
        let storage = self.app_contribution_storage();
        let paths = OwnerPaths::new(storage.root());
        storage
            .ensure_private_directory(&paths.authority_root)
            .await?;
        storage.ensure_private_directory(&paths.root).await?;
        storage.ensure_private_directory(&paths.receipts).await?;
        storage
            .ensure_private_directory(&paths.source_dispatches)
            .await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        let mut recovered = recover_owner(storage, &paths).await?;
        if recorded_at_ms < recovered.last_recorded_at_ms {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt time regressed behind the durable head".to_owned(),
            ));
        }

        if let Some(generation) = recovered
            .projection
            .applied_operations
            .get(&operation_digest)
        {
            let receipt = read_receipt(storage, &paths, *generation).await?;
            if receipt.operation != operation {
                return Err(AgentMemoryError::Validation(
                    "app-memory operation replay substituted immutable bytes".to_owned(),
                ));
            }
            return derive_result(receipt, proposal_dispatch_ack, invalidation_dispatch_ack);
        }
        if let Some(receipt) = compacted_source_dispatch_receipt(&recovered.projection, &operation)?
        {
            return derive_result(receipt, proposal_dispatch_ack, invalidation_dispatch_ack);
        }
        if operation_matches_compacted_high_water(&recovered.projection, &operation)? {
            return Err(AgentMemoryError::HistoryCompacted);
        }
        if absolute_expires_at_ms.is_some_and(|expiry| Utc::now().timestamp_millis() >= expiry) {
            return Err(AgentMemoryError::Validation(
                "app-memory dispatch permit expired before destination mutation".to_owned(),
            ));
        }
        validate_expected_head(storage, &paths, &recovered, expected_head.as_ref()).await?;
        if matches!(
            &operation,
            AppMemoryDestinationOperationV1::StageProposal { .. }
        ) && stage_would_exceed_source_head_cap(&recovered.projection, &operation)?
        {
            return Err(AgentMemoryError::Validation(
                "app-memory live admission quota is exhausted".to_owned(),
            ));
        }

        let generation = recovered.head.generation.checked_add(1).ok_or_else(|| {
            AgentMemoryError::Validation("app-memory generation overflow".to_owned())
        })?;
        let invalidation_disposition =
            apply_operation(&mut recovered.projection, &operation, recorded_at_ms)?;
        recovered.projection.generation = generation;
        recovered
            .projection
            .applied_operations
            .insert(operation_digest.clone(), generation);
        advance_replay_window(&mut recovered.projection);
        if serde_json::to_vec(&recovered.projection.entries)?.len() > MAX_LIVE_ENTRY_BYTES {
            return Err(AgentMemoryError::Validation(
                "app-memory live-entry admission exceeds its durable byte quota".to_owned(),
            ));
        }
        let resulting_projection_digest = projection_state_digest(&recovered.projection)?;
        let receipt_id = format!(
            "app-memory-receipt:{}",
            digest_serialized(
                "magician.app-memory-receipt-id.v1",
                &(generation, &operation_digest),
            )?
            .trim_start_matches("blake3:")
        );
        let receipt = AppMemoryDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id,
            generation,
            previous_receipt_digest: recovered.head.latest_receipt_digest.clone(),
            operation,
            invalidation_disposition,
            resulting_projection_digest,
            recorded_at_ms,
            receipt_digest: String::new(),
        }
        .seal()
        .map_err(contract_error)?;

        let source_material =
            source_dispatch_material(&receipt, &operation_digest, expected_head.as_ref())?;
        if let Some(material) = &source_material {
            write_immutable_source_dispatch(storage, &paths, material).await?;
        }
        write_immutable_receipt(storage, &paths, &receipt).await?;
        recovered.head = AppMemoryDestinationHeadV1 {
            schema_version: APP_MEMORY_OWNER_SCHEMA,
            generation,
            latest_receipt_digest: Some(receipt.receipt_digest.clone()),
        };
        if let Some(material) = source_material {
            recovered
                .projection
                .source_dispatch_high_water
                .insert(material.dispatch_slot_key.clone(), material);
        }
        recovered.projection.latest_receipt_digest = Some(receipt.receipt_digest.clone());
        if serde_json::to_vec(&recovered.projection)?.len() > MAX_PROJECTION_BYTES {
            return Err(AgentMemoryError::Validation(
                "app-memory compact projection exceeds its derived durable byte quota".to_owned(),
            ));
        }
        write_json_private(storage, &paths.head, &recovered.head, MAX_HEAD_BYTES).await?;
        write_json_private(
            storage,
            &paths.projection,
            &recovered.projection,
            MAX_PROJECTION_BYTES,
        )
        .await?;
        maybe_checkpoint_owner(storage, &paths, &recovered).await?;
        derive_result(receipt, proposal_dispatch_ack, invalidation_dispatch_ack)
    }

    pub(crate) async fn recover_app_memory_destination(
        &self,
    ) -> Result<AppMemoryDestinationProjectionV1, AgentMemoryError> {
        let in_process_lock = self.app_contribution_lock();
        let _in_process_guard = in_process_lock.lock().await;
        let storage = self.app_contribution_storage();
        let paths = OwnerPaths::new(storage.root());
        storage
            .ensure_private_directory(&paths.authority_root)
            .await?;
        storage.ensure_private_directory(&paths.root).await?;
        storage.ensure_private_directory(&paths.receipts).await?;
        storage
            .ensure_private_directory(&paths.source_dispatches)
            .await?;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&paths.head).await?;
        Ok(recover_owner(storage, &paths).await?.projection)
    }
}

fn stage_would_exceed_source_head_cap(
    projection: &AppMemoryDestinationProjectionV1,
    operation: &AppMemoryDestinationOperationV1,
) -> Result<bool, AgentMemoryError> {
    let AppMemoryDestinationOperationV1::StageProposal { proposal } = operation else {
        return Ok(false);
    };
    let source =
        proposal.header.sources.first().ok_or_else(|| {
            AgentMemoryError::Validation("V1 proposal lost its source".to_owned())
        })?;
    let key = source_head_high_water_key(
        &proposal.header.installation_id,
        &proposal.header.scope_binding_ref,
        &source.canonical_source_ref,
        &proposal.header.dedupe_key,
    )?;
    let mut keys = projection
        .invalidation_high_water
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.extend(
        projection
            .source_dispatch_high_water
            .values()
            .map(|material| material.source_head_key.clone()),
    );
    for entry in projection.entries.values() {
        let source = entry.proposal.header.sources.first().ok_or_else(|| {
            AgentMemoryError::Validation("V1 projection entry lost its source".to_owned())
        })?;
        keys.insert(source_head_high_water_key(
            &entry.proposal.header.installation_id,
            &entry.proposal.header.scope_binding_ref,
            &source.canonical_source_ref,
            &entry.proposal.header.dedupe_key,
        )?);
    }
    Ok(!keys.contains(&key) && keys.len() >= MAX_PROJECTION_ENTRIES)
}

fn advance_replay_window(projection: &mut AppMemoryDestinationProjectionV1) {
    let floor = projection
        .generation
        .saturating_sub(MAX_RECENT_OPERATION_REPLAY);
    projection
        .applied_operations
        .retain(|_, generation| *generation > floor);
    projection.replay_floor_generation = floor;
}

fn operation_source_dispatch_slot(
    operation: &AppMemoryDestinationOperationV1,
) -> Result<Option<(String, String)>, AgentMemoryError> {
    match operation {
        AppMemoryDestinationOperationV1::StageProposal { proposal } => {
            let source = proposal.header.sources.first().ok_or_else(|| {
                AgentMemoryError::Validation("V1 proposal lost its source".to_owned())
            })?;
            let source_head_key = source_head_high_water_key(
                &proposal.header.installation_id,
                &proposal.header.scope_binding_ref,
                &source.canonical_source_ref,
                &proposal.header.dedupe_key,
            )?;
            let dispatch_slot_key = digest_serialized(
                "magician.app-memory-source-dispatch-slot.v1",
                &(&source_head_key, "proposal"),
            )?;
            Ok(Some((source_head_key, dispatch_slot_key)))
        },
        AppMemoryDestinationOperationV1::Invalidate { invalidation } => {
            let source_head_key = pre_admission_tombstone_key(invalidation)?;
            let dispatch_slot_key = digest_serialized(
                "magician.app-memory-source-dispatch-slot.v1",
                &(&source_head_key, "invalidation"),
            )?;
            Ok(Some((source_head_key, dispatch_slot_key)))
        },
        AppMemoryDestinationOperationV1::Decide { .. } => Ok(None),
    }
}

fn source_dispatch_material(
    receipt: &AppMemoryDestinationReceiptV1,
    operation_digest: &str,
    acknowledged_predecessor: Option<&AppMemoryExpectedDestinationHead>,
) -> Result<Option<AppMemorySourceDispatchHighWaterV1>, AgentMemoryError> {
    let Some((source_head_key, dispatch_slot_key)) =
        operation_source_dispatch_slot(&receipt.operation)?
    else {
        return Ok(None);
    };
    Ok(Some(AppMemorySourceDispatchHighWaterV1 {
        schema_version: APP_MEMORY_OWNER_SCHEMA,
        source_head_key,
        dispatch_slot_key,
        operation_digest: operation_digest.to_owned(),
        generation: receipt.generation,
        receipt_id: receipt.receipt_id.clone(),
        previous_receipt_digest: receipt.previous_receipt_digest.clone(),
        invalidation_disposition: receipt.invalidation_disposition,
        resulting_projection_digest: receipt.resulting_projection_digest.clone(),
        recorded_at_ms: receipt.recorded_at_ms,
        receipt_digest: receipt.receipt_digest.clone(),
        acknowledged_predecessor_generation: acknowledged_predecessor.map(|head| head.generation),
        acknowledged_predecessor_digest: acknowledged_predecessor
            .map(|head| head.receipt_digest.clone()),
    }))
}

fn validate_source_dispatch_material(
    material: &AppMemorySourceDispatchHighWaterV1,
) -> Result<(), AgentMemoryError> {
    if material.schema_version != APP_MEMORY_OWNER_SCHEMA
        || material.generation == 0
        || material.recorded_at_ms < 0
        || (material.acknowledged_predecessor_generation.is_none()
            != material.acknowledged_predecessor_digest.is_none())
        || material.acknowledged_predecessor_generation == Some(0)
    {
        return Err(AgentMemoryError::Validation(
            "app-memory compact source-dispatch material has an invalid shape".to_owned(),
        ));
    }
    validate_digest(&material.source_head_key)?;
    validate_digest(&material.dispatch_slot_key)?;
    validate_digest(&material.operation_digest)?;
    validate_id(&material.receipt_id)?;
    if let Some(digest) = &material.previous_receipt_digest {
        validate_digest(digest)?;
    }
    validate_digest(&material.resulting_projection_digest)?;
    validate_digest(&material.receipt_digest)?;
    if let Some(digest) = &material.acknowledged_predecessor_digest {
        validate_digest(digest)?;
    }
    if serde_json::to_vec(material)?.len() > MAX_SOURCE_DISPATCH_MATERIAL_BYTES {
        return Err(AgentMemoryError::Validation(
            "app-memory compact source-dispatch material exceeds its bound".to_owned(),
        ));
    }
    Ok(())
}

fn receipt_from_source_dispatch_material(
    material: &AppMemorySourceDispatchHighWaterV1,
    operation: &AppMemoryDestinationOperationV1,
) -> Result<AppMemoryDestinationReceiptV1, AgentMemoryError> {
    validate_source_dispatch_material(material)?;
    let operation_digest = digest_serialized("magician.app-memory-operation.v1", operation)?;
    let (source_head_key, dispatch_slot_key) = operation_source_dispatch_slot(operation)?
        .ok_or_else(|| {
            AgentMemoryError::Validation("owner decision has no source dispatch receipt".to_owned())
        })?;
    if operation_digest != material.operation_digest
        || source_head_key != material.source_head_key
        || dispatch_slot_key != material.dispatch_slot_key
    {
        return Err(AgentMemoryError::HistoryCompacted);
    }
    let receipt = AppMemoryDestinationReceiptV1 {
        contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
        receipt_id: material.receipt_id.clone(),
        generation: material.generation,
        previous_receipt_digest: material.previous_receipt_digest.clone(),
        operation: operation.clone(),
        invalidation_disposition: material.invalidation_disposition,
        resulting_projection_digest: material.resulting_projection_digest.clone(),
        recorded_at_ms: material.recorded_at_ms,
        receipt_digest: material.receipt_digest.clone(),
    };
    receipt.validate().map_err(contract_error)?;
    Ok(receipt)
}

fn compacted_source_dispatch_receipt(
    projection: &AppMemoryDestinationProjectionV1,
    operation: &AppMemoryDestinationOperationV1,
) -> Result<Option<AppMemoryDestinationReceiptV1>, AgentMemoryError> {
    let Some((_, slot_key)) = operation_source_dispatch_slot(operation)? else {
        return Ok(None);
    };
    let Some(material) = projection.source_dispatch_high_water.get(&slot_key) else {
        return Ok(None);
    };
    let operation_digest = digest_serialized("magician.app-memory-operation.v1", operation)?;
    if material.operation_digest != operation_digest {
        // One compact slot owns the current receipt for an exact source lane,
        // not every future revision of that source. Only byte-identical
        // response-loss replay may reconstruct the compacted receipt here;
        // an advancing operation must continue to the monotonic reducer.
        return Ok(None);
    }
    receipt_from_source_dispatch_material(material, operation).map(Some)
}

fn operation_matches_compacted_high_water(
    projection: &AppMemoryDestinationProjectionV1,
    operation: &AppMemoryDestinationOperationV1,
) -> Result<bool, AgentMemoryError> {
    match operation {
        AppMemoryDestinationOperationV1::StageProposal { proposal } => {
            if projection
                .entries
                .get(&proposal.proposal_digest)
                .is_some_and(|entry| entry.proposal == *proposal)
            {
                return Ok(true);
            }
            let source = proposal.header.sources.first().ok_or_else(|| {
                AgentMemoryError::Validation("V1 proposal lost its source".to_owned())
            })?;
            let key = source_head_high_water_key(
                &proposal.header.installation_id,
                &proposal.header.scope_binding_ref,
                &source.canonical_source_ref,
                &proposal.header.dedupe_key,
            )?;
            Ok(projection
                .invalidation_high_water
                .get(&key)
                .is_some_and(|high_water| {
                    high_water.proposal_id == proposal.header.proposal_id
                        && high_water.proposal_digest == proposal.proposal_digest
                        && high_water.source_identity_digest == source.canonical_source_digest
                }))
        },
        AppMemoryDestinationOperationV1::Invalidate { invalidation } => {
            let key = pre_admission_tombstone_key(invalidation)?;
            Ok(projection
                .invalidation_high_water
                .get(&key)
                .is_some_and(|high_water| {
                    high_water.proposal_id == invalidation.proposal_id
                        && high_water.proposal_digest == invalidation.proposal_digest
                        && high_water.invalidation_digest.as_deref()
                            == Some(invalidation.invalidation_digest.as_str())
                }))
        },
        AppMemoryDestinationOperationV1::Decide {
            proposal_id,
            proposal_digest,
            owner_decision_receipt_digest,
            ..
        } => Ok(projection
            .entries
            .get(proposal_digest)
            .is_some_and(|entry| {
                entry.owner_decision_receipt_digest.as_deref()
                    == Some(owner_decision_receipt_digest.as_str())
            })
            || projection
                .invalidation_high_water
                .values()
                .any(|high_water| {
                    high_water.proposal_id == *proposal_id
                        && high_water.proposal_digest == *proposal_digest
                        && high_water.invalidation_digest.as_deref()
                            == Some(owner_decision_receipt_digest.as_str())
                })),
    }
}

struct OwnerPaths {
    authority_root: PathBuf,
    root: PathBuf,
    receipts: PathBuf,
    source_dispatches: PathBuf,
    head: PathBuf,
    projection: PathBuf,
    checkpoint: PathBuf,
}

impl OwnerPaths {
    fn new(storage_root: &Path) -> Self {
        let authority_root = storage_root.join("app-contributions");
        let root = authority_root.join("memory-v1");
        Self {
            authority_root,
            receipts: root.join("receipts"),
            source_dispatches: root.join("source-dispatches"),
            head: root.join("head.json"),
            projection: root.join("projection.json"),
            checkpoint: root.join("checkpoint.json"),
            root,
        }
    }

    fn receipt(&self, generation: u64) -> PathBuf {
        self.receipts.join(format!("{generation:020}.json"))
    }

    fn source_dispatch(&self, generation: u64) -> PathBuf {
        self.source_dispatches
            .join(format!("{generation:020}.json"))
    }
}

async fn maybe_checkpoint_owner(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
    recovered: &RecoveredOwner,
) -> Result<(), AgentMemoryError> {
    if recovered.head.generation == 0
        || recovered.head.generation % MAX_RECENT_OPERATION_REPLAY != 0
    {
        return Ok(());
    }
    let projection_digest = projection_state_digest(&recovered.projection)?;
    let source_high_water_digest = digest_serialized(
        "magician.app-memory-source-high-water-set.v1",
        &(
            &recovered.projection.invalidation_high_water,
            &recovered.projection.source_dispatch_high_water,
        ),
    )?;
    let mut checkpoint = AppMemoryDestinationCheckpointV1 {
        schema_version: APP_MEMORY_OWNER_SCHEMA,
        generation: recovered.head.generation,
        previous_receipt_digest: recovered.head.latest_receipt_digest.clone(),
        projection: recovered.projection.clone(),
        projection_digest,
        source_high_water_digest,
        checkpoint_digest: String::new(),
    };
    checkpoint.checkpoint_digest = checkpoint_digest(&checkpoint)?;
    write_json_private(
        storage,
        &paths.checkpoint,
        &checkpoint,
        MAX_CHECKPOINT_BYTES,
    )
    .await?;
    let delete_through = checkpoint
        .generation
        .saturating_sub(MAX_RECENT_OPERATION_REPLAY);
    let previous_checkpoint = delete_through.saturating_sub(MAX_RECENT_OPERATION_REPLAY);
    for generation in previous_checkpoint.saturating_add(1)..=delete_through {
        storage
            .remove_private_file_and_sync_parent(paths.receipt(generation))
            .await?;
        storage
            .remove_private_file_and_sync_parent(paths.source_dispatch(generation))
            .await?;
    }
    Ok(())
}

fn checkpoint_digest(
    checkpoint: &AppMemoryDestinationCheckpointV1,
) -> Result<String, AgentMemoryError> {
    digest_serialized(
        "magician.app-memory-checkpoint.v1",
        &(
            checkpoint.schema_version,
            checkpoint.generation,
            &checkpoint.previous_receipt_digest,
            &checkpoint.projection_digest,
            &checkpoint.source_high_water_digest,
        ),
    )
}

fn validate_checkpoint(
    checkpoint: &AppMemoryDestinationCheckpointV1,
) -> Result<(), AgentMemoryError> {
    if checkpoint.schema_version != APP_MEMORY_OWNER_SCHEMA
        || checkpoint.generation == 0
        || checkpoint.projection.generation != checkpoint.generation
        || checkpoint.previous_receipt_digest != checkpoint.projection.latest_receipt_digest
        || checkpoint.projection_digest != projection_state_digest(&checkpoint.projection)?
        || checkpoint.source_high_water_digest
            != digest_serialized(
                "magician.app-memory-source-high-water-set.v1",
                &(
                    &checkpoint.projection.invalidation_high_water,
                    &checkpoint.projection.source_dispatch_high_water,
                ),
            )?
        || checkpoint.checkpoint_digest != checkpoint_digest(checkpoint)?
    {
        return Err(AgentMemoryError::Validation(
            "app-memory checkpoint is invalid or substituted".to_owned(),
        ));
    }
    validate_projection_shape(&checkpoint.projection)
}

struct RecoveredOwner {
    head: AppMemoryDestinationHeadV1,
    projection: AppMemoryDestinationProjectionV1,
    last_recorded_at_ms: i64,
}

async fn recover_owner(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
) -> Result<RecoveredOwner, AgentMemoryError> {
    let checkpoint: Option<AppMemoryDestinationCheckpointV1> =
        read_optional_json(storage, &paths.checkpoint, MAX_CHECKPOINT_BYTES).await?;
    if let Some(value) = &checkpoint {
        validate_checkpoint(value)?;
    }
    let mut head = match read_optional_json(storage, &paths.head, MAX_HEAD_BYTES).await? {
        Some(value) => value,
        None => AppMemoryDestinationHeadV1 {
            schema_version: APP_MEMORY_OWNER_SCHEMA,
            generation: 0,
            latest_receipt_digest: None,
        },
    };
    validate_head(&head)?;
    if let Some(checkpoint) = &checkpoint {
        if head.generation < checkpoint.generation
            || (head.generation == checkpoint.generation
                && head.latest_receipt_digest != checkpoint.previous_receipt_digest)
        {
            return Err(AgentMemoryError::Validation(
                "app-memory head rolled back behind its durable checkpoint".to_owned(),
            ));
        }
    }

    // Source-dispatch material precedes receipt publication so a durable
    // receipt can always be replayed into its compact acknowledgement. If no
    // exact next receipt exists, that material never became authoritative and
    // is removed under the owner lock before the generation is reused. Once a
    // receipt exists, its material is preserved and validated during replay.
    let next = head
        .generation
        .checked_add(1)
        .ok_or_else(|| AgentMemoryError::Validation("app-memory generation overflow".to_owned()))?;
    if let Some(orphan) = read_optional_receipt(storage, paths, next).await? {
        let expected_previous = head.latest_receipt_digest.as_deref();
        if orphan.generation != next
            || orphan.previous_receipt_digest.as_deref() != expected_previous
        {
            return Err(AgentMemoryError::Validation(
                "app-memory orphan receipt does not extend the exact head".to_owned(),
            ));
        }
        head.generation = next;
        head.latest_receipt_digest = Some(orphan.receipt_digest);
        write_json_private(storage, &paths.head, &head, MAX_HEAD_BYTES).await?;
    } else {
        storage
            .remove_private_file_and_sync_parent(paths.source_dispatch(next))
            .await?;
    }
    let mut last_recorded_at_ms = 0;
    if head.generation > 0 {
        let head_receipt = read_receipt(storage, paths, head.generation).await?;
        if head.latest_receipt_digest.as_deref() != Some(head_receipt.receipt_digest.as_str()) {
            return Err(AgentMemoryError::Validation(
                "app-memory head does not name its immutable receipt".to_owned(),
            ));
        }
        last_recorded_at_ms = head_receipt.recorded_at_ms;
    }

    let mut projection: AppMemoryDestinationProjectionV1 = match (
        read_optional_json::<AppMemoryDestinationProjectionV1>(
            storage,
            &paths.projection,
            MAX_PROJECTION_BYTES,
        )
        .await?,
        checkpoint.as_ref(),
    ) {
        (Some(value), Some(checkpoint)) if value.generation < checkpoint.generation => {
            checkpoint.projection.clone()
        },
        (Some(value), _) => value,
        (None, Some(checkpoint)) => checkpoint.projection.clone(),
        (None, None) => empty_projection(),
    };
    validate_projection_shape(&projection)?;
    if projection.generation > head.generation
        || head.generation.saturating_sub(projection.generation) > MAX_RECOVERY_RECEIPTS
    {
        return Err(AgentMemoryError::Validation(
            "app-memory projection recovery exceeds its bounded window".to_owned(),
        ));
    }
    let mut replay_recorded_at_ms = 0;
    if projection.generation > 0 {
        let projected_receipt = read_receipt(storage, paths, projection.generation).await?;
        if projection.latest_receipt_digest.as_deref()
            != Some(projected_receipt.receipt_digest.as_str())
        {
            return Err(AgentMemoryError::Validation(
                "app-memory projection does not name its immutable receipt".to_owned(),
            ));
        }
        replay_recorded_at_ms = projected_receipt.recorded_at_ms;
    }
    while projection.generation < head.generation {
        let generation = projection.generation + 1;
        let receipt = read_receipt(storage, paths, generation).await?;
        if receipt.previous_receipt_digest.as_deref() != projection.latest_receipt_digest.as_deref()
        {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt predecessor chain diverged".to_owned(),
            ));
        }
        if receipt.recorded_at_ms < replay_recorded_at_ms {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt time regressed".to_owned(),
            ));
        }
        let operation_digest =
            digest_serialized("magician.app-memory-operation.v1", &receipt.operation)?;
        if projection
            .applied_operations
            .contains_key(&operation_digest)
        {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt repeats a prior operation at a new generation".to_owned(),
            ));
        }
        let replay_disposition =
            apply_operation(&mut projection, &receipt.operation, receipt.recorded_at_ms)?;
        if replay_disposition != receipt.invalidation_disposition {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt invalidation disposition diverged on replay".to_owned(),
            ));
        }
        projection.generation = generation;
        projection
            .applied_operations
            .insert(operation_digest, generation);
        if let Some((source_head_key, dispatch_slot_key)) =
            operation_source_dispatch_slot(&receipt.operation)?
        {
            let material: AppMemorySourceDispatchHighWaterV1 = read_optional_json(
                storage,
                &paths.source_dispatch(generation),
                MAX_SOURCE_DISPATCH_MATERIAL_BYTES,
            )
            .await?
            .ok_or_else(|| {
                AgentMemoryError::Validation(
                    "app-memory source receipt lost its compact dispatch material".to_owned(),
                )
            })?;
            if material.source_head_key != source_head_key
                || material.dispatch_slot_key != dispatch_slot_key
                || receipt_from_source_dispatch_material(&material, &receipt.operation)? != receipt
            {
                return Err(AgentMemoryError::Validation(
                    "app-memory source dispatch material diverged from its receipt".to_owned(),
                ));
            }
            projection
                .source_dispatch_high_water
                .insert(dispatch_slot_key, material);
        }
        advance_replay_window(&mut projection);
        if projection_state_digest(&projection)? != receipt.resulting_projection_digest {
            let mut legacy_projection = projection.clone();
            strip_app_memory_inspection_metadata(&mut legacy_projection);
            if projection_state_digest(&legacy_projection)? != receipt.resulting_projection_digest {
                return Err(AgentMemoryError::Validation(
                    "app-memory receipt projection digest mismatch".to_owned(),
                ));
            }
            projection = legacy_projection;
        }
        projection.latest_receipt_digest = Some(receipt.receipt_digest);
        replay_recorded_at_ms = receipt.recorded_at_ms;
    }
    if projection.generation != head.generation
        || projection.latest_receipt_digest != head.latest_receipt_digest
    {
        return Err(AgentMemoryError::Validation(
            "app-memory head and projection do not converge".to_owned(),
        ));
    }
    if projection.generation > 0 {
        let terminal_receipt = read_receipt(storage, paths, projection.generation).await?;
        if projection_state_digest(&projection)? != terminal_receipt.resulting_projection_digest {
            return Err(AgentMemoryError::Validation(
                "app-memory current projection does not match its terminal receipt".to_owned(),
            ));
        }
    }
    if projection.generation > 0 {
        write_json_private(
            storage,
            &paths.projection,
            &projection,
            MAX_PROJECTION_BYTES,
        )
        .await?;
    }
    Ok(RecoveredOwner {
        head,
        projection,
        last_recorded_at_ms,
    })
}

fn strip_app_memory_inspection_metadata(projection: &mut AppMemoryDestinationProjectionV1) {
    for entry in projection.entries.values_mut() {
        entry.state_changed_at_ms = 0;
    }
    for high_water in projection.invalidation_high_water.values_mut() {
        high_water.terminal_reason = None;
        high_water.source_invalidation_reason = None;
        high_water.state_changed_at_ms = 0;
        high_water.proposal_issued_at_ms = None;
        high_water.proposal_expires_at_ms = None;
        high_water.workflow_id = None;
        high_water.action_id = None;
        high_water.contribution_port_id = None;
    }
}

async fn validate_expected_head(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
    recovered: &RecoveredOwner,
    expected: Option<&AppMemoryExpectedDestinationHead>,
) -> Result<(), AgentMemoryError> {
    let Some(expected) = expected else {
        // No source acknowledgement exists. Empty is valid, and an exact
        // destination prefix ahead of the source is a recoverable unacked send.
        return Ok(());
    };
    if expected.generation == 0 || recovered.head.generation < expected.generation {
        return Err(AgentMemoryError::Validation(
            "app-memory destination rolled back behind the immutable source acknowledgement"
                .to_owned(),
        ));
    }
    validate_digest(&expected.receipt_digest)?;
    let acknowledged = match read_optional_receipt(storage, paths, expected.generation).await? {
        Some(receipt) => receipt,
        None => {
            let exact_compacted_lineage = recovered
                .projection
                .source_dispatch_high_water
                .values()
                .any(|material| {
                    (material.generation == expected.generation
                        && material.receipt_digest == expected.receipt_digest)
                        || (material.acknowledged_predecessor_generation
                            == Some(expected.generation)
                            && material.acknowledged_predecessor_digest.as_deref()
                                == Some(expected.receipt_digest.as_str()))
                });
            if exact_compacted_lineage {
                return Ok(());
            }
            return Err(AgentMemoryError::HistoryCompacted);
        },
    };
    if acknowledged.receipt_digest != expected.receipt_digest {
        return Err(AgentMemoryError::Validation(
            "app-memory destination prefix mismatches the immutable source acknowledgement"
                .to_owned(),
        ));
    }
    Ok(())
}

fn empty_projection() -> AppMemoryDestinationProjectionV1 {
    AppMemoryDestinationProjectionV1 {
        schema_version: APP_MEMORY_OWNER_SCHEMA,
        generation: 0,
        latest_receipt_digest: None,
        replay_floor_generation: 0,
        entries: BTreeMap::new(),
        invalidation_high_water: BTreeMap::new(),
        source_dispatch_high_water: BTreeMap::new(),
        applied_operations: BTreeMap::new(),
    }
}

fn validate_operation(operation: &AppMemoryDestinationOperationV1) -> Result<(), AgentMemoryError> {
    match operation {
        AppMemoryDestinationOperationV1::StageProposal { proposal } => {
            proposal.validate().map_err(contract_error)?;
            if proposal.header.update_policy
                != magician_app_contract::contribution::AppContributionUpdatePolicy::ReplaceExactSourceHead
                || proposal.header.retraction_policy
                    != magician_app_contract::contribution::AppContributionRetractionPolicy::TombstoneOnAnySourceDrift
                || proposal.header.sources.len() != 1
            {
                return Err(AgentMemoryError::Validation(
                    "V1 app-memory ingress requires one exact replaceable source head"
                        .to_owned(),
                ));
            }
            Ok(())
        },
        AppMemoryDestinationOperationV1::Decide {
            proposal_id,
            proposal_digest,
            owner_decision_receipt_digest,
            retained_until_ms,
            ..
        } => {
            validate_id(proposal_id)?;
            validate_digest(proposal_digest)?;
            validate_digest(owner_decision_receipt_digest)?;
            if retained_until_ms.is_some_and(|value| value < 0) {
                return Err(AgentMemoryError::Validation(
                    "app-memory retention deadline is invalid".to_owned(),
                ));
            }
            Ok(())
        },
        AppMemoryDestinationOperationV1::Invalidate { invalidation } => {
            invalidation.validate().map_err(contract_error)
        },
    }
}

fn pre_admission_tombstone_key(
    invalidation: &magician_app_contract::contribution::AppMemoryInvalidationV1,
) -> Result<String, AgentMemoryError> {
    source_head_high_water_key(
        &invalidation.installation_id,
        &invalidation.scope_binding_ref,
        &invalidation.source_event_ref,
        &invalidation.dedupe_key,
    )
}

fn source_head_high_water_key(
    installation_id: &str,
    scope_binding_ref: &str,
    source_event_ref: &str,
    dedupe_key: &str,
) -> Result<String, AgentMemoryError> {
    digest_serialized(
        "magician.app-memory-source-high-water.v1",
        &(
            installation_id,
            scope_binding_ref,
            source_event_ref,
            dedupe_key,
        ),
    )
}

fn apply_operation(
    projection: &mut AppMemoryDestinationProjectionV1,
    operation: &AppMemoryDestinationOperationV1,
    recorded_at_ms: i64,
) -> Result<Option<AppMemoryInvalidationDispositionV1>, AgentMemoryError> {
    match operation {
        AppMemoryDestinationOperationV1::StageProposal { proposal } => {
            let source = proposal.header.sources.first().ok_or_else(|| {
                AgentMemoryError::Validation(
                    "V1 app-memory proposal has no exact source".to_owned(),
                )
            })?;
            let tombstone_key = source_head_high_water_key(
                &proposal.header.installation_id,
                &proposal.header.scope_binding_ref,
                &source.canonical_source_ref,
                &proposal.header.dedupe_key,
            )?;
            if let Some(high_water) = projection.invalidation_high_water.get(&tombstone_key) {
                let invalidates_this_proposal = high_water.proposal_id
                    == proposal.header.proposal_id
                    && high_water.proposal_digest == proposal.proposal_digest;
                if source.record_revision < high_water.source_event_revision
                    || (source.record_revision == high_water.source_event_revision
                        && invalidates_this_proposal)
                {
                    return Err(AgentMemoryError::Validation(
                        "app-memory proposal is at or behind compacted source high-water"
                            .to_owned(),
                    ));
                }
            }
            let prior = projection
                .entries
                .iter()
                .filter(|(_, existing)| {
                    existing.proposal.header.installation_id == proposal.header.installation_id
                        && existing.proposal.header.dedupe_key == proposal.header.dedupe_key
                })
                .map(|(digest, existing)| (digest.clone(), existing.clone()))
                .collect::<Vec<_>>();
            if prior.len() > 1 {
                return Err(AgentMemoryError::Validation(
                    "app-memory destination contains multiple exact-source heads".to_owned(),
                ));
            }
            if let Some((prior_digest, prior_entry)) = prior.into_iter().next() {
                let prior_source =
                    prior_entry.proposal.header.sources.first().ok_or_else(|| {
                        AgentMemoryError::Validation(
                            "prior V1 app-memory proposal has no exact source".to_owned(),
                        )
                    })?;
                if proposal.header.proposal_revision
                    <= prior_entry.proposal.header.proposal_revision
                    || source.record_revision <= prior_source.record_revision
                    || source.canonical_source_ref != prior_source.canonical_source_ref
                {
                    return Err(AgentMemoryError::Validation(
                        "replace-exact-source-head did not advance the destination head".to_owned(),
                    ));
                }
                let prior_tombstone_key = source_head_high_water_key(
                    &prior_entry.proposal.header.installation_id,
                    &prior_entry.proposal.header.scope_binding_ref,
                    &prior_source.canonical_source_ref,
                    &prior_entry.proposal.header.dedupe_key,
                )?;
                projection.entries.remove(&prior_digest);
                projection.invalidation_high_water.insert(
                    prior_tombstone_key,
                    AppMemoryInvalidationHighWaterV1 {
                        installation_id: prior_entry.proposal.header.installation_id.clone(),
                        scope_binding_ref: prior_entry.proposal.header.scope_binding_ref.clone(),
                        proposal_id: prior_entry.proposal.header.proposal_id.clone(),
                        proposal_digest: prior_entry.proposal.proposal_digest.clone(),
                        source_identity_digest: prior_source.canonical_source_digest.clone(),
                        source_event_ref: prior_source.canonical_source_ref.clone(),
                        dedupe_key: prior_entry.proposal.header.dedupe_key.clone(),
                        source_event_revision: source.record_revision,
                        invalidation_digest: None,
                        disposition: AppMemoryInvalidationDispositionV1::Tombstoned,
                        terminal_reason: Some(
                            AppMemoryContributionStateReasonV1::SourceInvalidated,
                        ),
                        source_invalidation_reason: Some(
                            AppMemoryInvalidationReasonV1::SourceUpdated,
                        ),
                        state_changed_at_ms: recorded_at_ms,
                        proposal_issued_at_ms: Some(prior_entry.proposal.header.issued_at_ms),
                        proposal_expires_at_ms: Some(prior_entry.proposal.header.expires_at_ms),
                        workflow_id: Some(prior_entry.proposal.header.workflow_id.clone()),
                        action_id: Some(prior_entry.proposal.header.action_id.clone()),
                        contribution_port_id: Some(
                            prior_entry.proposal.header.contribution_port_id.clone(),
                        ),
                    },
                );
            }
            match projection.entries.get(&proposal.proposal_digest) {
                None => {},
                Some(_) => {
                    return Err(AgentMemoryError::Validation(
                        "app-memory exact proposal must replay its original operation".to_owned(),
                    ));
                },
            }
            if projection.entries.values().any(|existing| {
                existing.proposal.header.installation_id == proposal.header.installation_id
                    && existing.proposal.header.proposal_id == proposal.header.proposal_id
                    && proposal.header.proposal_revision
                        <= existing.proposal.header.proposal_revision
            }) {
                return Err(AgentMemoryError::Validation(
                    "app-memory proposal revision is stale or substituted".to_owned(),
                ));
            }
            projection.entries.insert(
                proposal.proposal_digest.clone(),
                AppMemoryProjectionEntryV1 {
                    proposal: proposal.clone(),
                    state: AppMemoryProjectionStateV1::Proposed,
                    owner_decision_receipt_digest: None,
                    retained_until_ms: None,
                    invalidation_digest: None,
                    source_event_revision: source.record_revision,
                    state_changed_at_ms: recorded_at_ms,
                },
            );
            Ok(None)
        },
        AppMemoryDestinationOperationV1::Decide {
            proposal_id,
            proposal_digest,
            decision,
            owner_decision_receipt_digest,
            retained_until_ms,
        } => {
            let entry = projection.entries.get_mut(proposal_digest).ok_or_else(|| {
                AgentMemoryError::Validation(
                    "owner decision references an unknown app-memory proposal".to_owned(),
                )
            })?;
            let state_matches = match decision {
                AppMemoryOwnerDecisionV1::Accept | AppMemoryOwnerDecisionV1::Reject => {
                    entry.state == AppMemoryProjectionStateV1::Proposed
                },
                AppMemoryOwnerDecisionV1::Revoke => {
                    entry.state == AppMemoryProjectionStateV1::Accepted
                },
            };
            if entry.proposal.header.proposal_id != *proposal_id
                || entry.proposal.proposal_digest != *proposal_digest
                || !state_matches
            {
                return Err(AgentMemoryError::Validation(
                    "owner decision does not match the current exact proposal".to_owned(),
                ));
            }
            if matches!(
                decision,
                AppMemoryOwnerDecisionV1::Reject | AppMemoryOwnerDecisionV1::Revoke
            ) {
                let terminal = entry.clone();
                let source = terminal.proposal.header.sources.first().ok_or_else(|| {
                    AgentMemoryError::Validation("terminal proposal lost its source".to_owned())
                })?;
                let key = source_head_high_water_key(
                    &terminal.proposal.header.installation_id,
                    &terminal.proposal.header.scope_binding_ref,
                    &source.canonical_source_ref,
                    &terminal.proposal.header.dedupe_key,
                )?;
                projection.entries.remove(proposal_digest);
                projection.invalidation_high_water.insert(
                    key,
                    AppMemoryInvalidationHighWaterV1 {
                        installation_id: terminal.proposal.header.installation_id.clone(),
                        scope_binding_ref: terminal.proposal.header.scope_binding_ref.clone(),
                        proposal_id: terminal.proposal.header.proposal_id.clone(),
                        proposal_digest: terminal.proposal.proposal_digest.clone(),
                        source_identity_digest: source.canonical_source_digest.clone(),
                        source_event_ref: source.canonical_source_ref.clone(),
                        dedupe_key: terminal.proposal.header.dedupe_key.clone(),
                        source_event_revision: source.record_revision,
                        invalidation_digest: Some(owner_decision_receipt_digest.clone()),
                        disposition: AppMemoryInvalidationDispositionV1::Tombstoned,
                        terminal_reason: Some(match decision {
                            AppMemoryOwnerDecisionV1::Reject => {
                                AppMemoryContributionStateReasonV1::OwnerRejected
                            },
                            AppMemoryOwnerDecisionV1::Revoke => {
                                AppMemoryContributionStateReasonV1::OwnerRevoked
                            },
                            AppMemoryOwnerDecisionV1::Accept => unreachable!(),
                        }),
                        source_invalidation_reason: None,
                        state_changed_at_ms: recorded_at_ms,
                        proposal_issued_at_ms: Some(terminal.proposal.header.issued_at_ms),
                        proposal_expires_at_ms: Some(terminal.proposal.header.expires_at_ms),
                        workflow_id: Some(terminal.proposal.header.workflow_id.clone()),
                        action_id: Some(terminal.proposal.header.action_id.clone()),
                        contribution_port_id: Some(
                            terminal.proposal.header.contribution_port_id.clone(),
                        ),
                    },
                );
            } else {
                if retained_until_ms.is_none_or(|expiry| {
                    expiry > entry.proposal.header.expires_at_ms
                        || expiry <= entry.proposal.header.issued_at_ms
                }) {
                    return Err(AgentMemoryError::Validation(
                        "accepted app-memory retention exceeds its reviewed proposal ceiling"
                            .to_owned(),
                    ));
                }
                entry.state = AppMemoryProjectionStateV1::Accepted;
                entry.owner_decision_receipt_digest = Some(owner_decision_receipt_digest.clone());
                entry.retained_until_ms = *retained_until_ms;
                entry.state_changed_at_ms = recorded_at_ms;
            }
            Ok(None)
        },
        AppMemoryDestinationOperationV1::Invalidate { invalidation } => {
            let key = pre_admission_tombstone_key(invalidation)?;
            let Some(entry) = projection.entries.get(&invalidation.proposal_digest) else {
                match projection.invalidation_high_water.get_mut(&key) {
                    Some(existing)
                        if existing.installation_id == invalidation.installation_id
                            && existing.scope_binding_ref == invalidation.scope_binding_ref
                            && existing.proposal_id == invalidation.proposal_id
                            && existing.proposal_digest == invalidation.proposal_digest
                            && existing.source_identity_digest
                                == invalidation.source_identity_digest
                            && existing.source_event_ref == invalidation.source_event_ref
                            && existing.dedupe_key == invalidation.dedupe_key
                            && existing.source_event_revision
                                == invalidation.source_event_revision
                            && existing.invalidation_digest.as_deref()
                                == Some(invalidation.invalidation_digest.as_str()) =>
                    {
                        return Ok(Some(existing.disposition));
                    },
                    Some(existing)
                        if existing.installation_id == invalidation.installation_id
                            && existing.scope_binding_ref == invalidation.scope_binding_ref
                            && existing.proposal_id == invalidation.proposal_id
                            && existing.proposal_digest == invalidation.proposal_digest
                            && existing.source_identity_digest
                                == invalidation.source_identity_digest
                            && existing.source_event_ref == invalidation.source_event_ref
                            && existing.dedupe_key == invalidation.dedupe_key
                            && existing.source_event_revision
                                == invalidation.source_event_revision
                            && existing.invalidation_digest.is_none() =>
                    {
                        existing.invalidation_digest =
                            Some(invalidation.invalidation_digest.clone());
                        existing.disposition =
                            AppMemoryInvalidationDispositionV1::AlreadyTombstoned;
                        existing.terminal_reason =
                            Some(AppMemoryContributionStateReasonV1::SourceInvalidated);
                        existing.source_invalidation_reason = Some(invalidation.reason);
                        existing.state_changed_at_ms = recorded_at_ms;
                        return Ok(Some(AppMemoryInvalidationDispositionV1::AlreadyTombstoned));
                    },
                    Some(existing)
                        if existing.installation_id == invalidation.installation_id
                            && existing.scope_binding_ref == invalidation.scope_binding_ref
                            && existing.proposal_id == invalidation.proposal_id
                            && existing.proposal_digest == invalidation.proposal_digest
                            && existing.source_identity_digest
                                == invalidation.source_identity_digest
                            && existing.source_event_ref == invalidation.source_event_ref
                            && existing.dedupe_key == invalidation.dedupe_key
                            && invalidation_advances_exact_source(
                                invalidation.reason,
                                invalidation.source_event_revision,
                                existing.source_event_revision,
                            ) =>
                    {
                        existing.source_event_revision = invalidation.source_event_revision;
                        existing.invalidation_digest =
                            Some(invalidation.invalidation_digest.clone());
                        existing.disposition =
                            AppMemoryInvalidationDispositionV1::AlreadyTombstoned;
                        existing.terminal_reason =
                            Some(AppMemoryContributionStateReasonV1::SourceInvalidated);
                        existing.source_invalidation_reason = Some(invalidation.reason);
                        existing.state_changed_at_ms = recorded_at_ms;
                        return Ok(Some(AppMemoryInvalidationDispositionV1::AlreadyTombstoned));
                    },
                    Some(_) => {
                        return Err(AgentMemoryError::Validation(
                            "pre-admission invalidation high-water was substituted".to_owned(),
                        ));
                    },
                    None => {
                        projection.invalidation_high_water.insert(
                            key,
                            AppMemoryInvalidationHighWaterV1 {
                                installation_id: invalidation.installation_id.clone(),
                                scope_binding_ref: invalidation.scope_binding_ref.clone(),
                                proposal_id: invalidation.proposal_id.clone(),
                                proposal_digest: invalidation.proposal_digest.clone(),
                                source_identity_digest: invalidation.source_identity_digest.clone(),
                                source_event_ref: invalidation.source_event_ref.clone(),
                                dedupe_key: invalidation.dedupe_key.clone(),
                                source_event_revision: invalidation.source_event_revision,
                                invalidation_digest: Some(invalidation.invalidation_digest.clone()),
                                disposition:
                                    AppMemoryInvalidationDispositionV1::SupersededBeforeAdmission,
                                terminal_reason: Some(
                                    AppMemoryContributionStateReasonV1::SourceInvalidated,
                                ),
                                source_invalidation_reason: Some(invalidation.reason),
                                state_changed_at_ms: recorded_at_ms,
                                proposal_issued_at_ms: None,
                                proposal_expires_at_ms: None,
                                workflow_id: None,
                                action_id: None,
                                contribution_port_id: None,
                            },
                        );
                        return Ok(Some(
                            AppMemoryInvalidationDispositionV1::SupersededBeforeAdmission,
                        ));
                    },
                }
            };
            if entry.proposal.header.proposal_id != invalidation.proposal_id
                || entry.proposal.proposal_digest != invalidation.proposal_digest
                || entry.proposal.header.installation_id != invalidation.installation_id
                || entry.proposal.header.scope_binding_ref != invalidation.scope_binding_ref
                || !invalidation_advances_exact_source(
                    invalidation.reason,
                    invalidation.source_event_revision,
                    entry.source_event_revision,
                )
                || !entry.proposal.header.sources.iter().any(|source| {
                    source.canonical_source_digest == invalidation.source_identity_digest
                })
            {
                return Err(AgentMemoryError::Validation(
                    "app-memory invalidation does not match the exact source head".to_owned(),
                ));
            }
            let terminal = entry.clone();
            projection.entries.remove(&invalidation.proposal_digest);
            projection.invalidation_high_water.insert(
                key,
                AppMemoryInvalidationHighWaterV1 {
                    installation_id: invalidation.installation_id.clone(),
                    scope_binding_ref: invalidation.scope_binding_ref.clone(),
                    proposal_id: invalidation.proposal_id.clone(),
                    proposal_digest: invalidation.proposal_digest.clone(),
                    source_identity_digest: invalidation.source_identity_digest.clone(),
                    source_event_ref: invalidation.source_event_ref.clone(),
                    dedupe_key: invalidation.dedupe_key.clone(),
                    source_event_revision: invalidation.source_event_revision,
                    invalidation_digest: Some(invalidation.invalidation_digest.clone()),
                    disposition: AppMemoryInvalidationDispositionV1::Tombstoned,
                    terminal_reason: Some(AppMemoryContributionStateReasonV1::SourceInvalidated),
                    source_invalidation_reason: Some(invalidation.reason),
                    state_changed_at_ms: recorded_at_ms,
                    proposal_issued_at_ms: Some(terminal.proposal.header.issued_at_ms),
                    proposal_expires_at_ms: Some(terminal.proposal.header.expires_at_ms),
                    workflow_id: Some(terminal.proposal.header.workflow_id.clone()),
                    action_id: Some(terminal.proposal.header.action_id.clone()),
                    contribution_port_id: Some(
                        terminal.proposal.header.contribution_port_id.clone(),
                    ),
                },
            );
            Ok(Some(AppMemoryInvalidationDispositionV1::Tombstoned))
        },
    }
}

fn derive_result(
    receipt: AppMemoryDestinationReceiptV1,
    proposal_dispatch_ack: Option<AppMemoryContributionDispatchAck>,
    invalidation_dispatch_ack: Option<AppMemoryInvalidationDispatchAck>,
) -> Result<AppMemoryDestinationApplyResult, AgentMemoryError> {
    match &receipt.operation {
        AppMemoryDestinationOperationV1::StageProposal { proposal } => {
            let source_receipt = AppMemoryIngressReceiptV1 {
                contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
                receipt_id: format!("memory-ingress:{}", receipt.receipt_id),
                proposal_id: proposal.header.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                destination_generation: receipt.generation,
                destination_receipt_digest: receipt.receipt_digest.clone(),
                disposition: AppMemoryIngressDispositionV1::Staged,
                recorded_at_ms: receipt.recorded_at_ms,
                receipt_digest: String::new(),
            }
            .seal()
            .map_err(contract_error)?;
            Ok(AppMemoryDestinationApplyResult::Proposal {
                destination_receipt: receipt,
                source_ack: AppMemoryIngressAckPermit {
                    receipt: source_receipt,
                    dispatch_ack: proposal_dispatch_ack,
                },
            })
        },
        AppMemoryDestinationOperationV1::Decide { .. } => {
            Ok(AppMemoryDestinationApplyResult::Decision {
                destination_receipt: receipt,
            })
        },
        AppMemoryDestinationOperationV1::Invalidate { invalidation } => {
            let disposition = receipt.invalidation_disposition.ok_or_else(|| {
                AgentMemoryError::Validation(
                    "destination invalidation receipt omitted its disposition".to_owned(),
                )
            })?;
            let source_receipt = AppMemoryInvalidationReceiptV1 {
                contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
                receipt_id: format!("memory-invalidation-ack:{}", receipt.receipt_id),
                invalidation_id: invalidation.invalidation_id.clone(),
                invalidation_digest: invalidation.invalidation_digest.clone(),
                proposal_id: invalidation.proposal_id.clone(),
                destination_generation: receipt.generation,
                destination_receipt_digest: receipt.receipt_digest.clone(),
                disposition,
                recorded_at_ms: receipt.recorded_at_ms,
                receipt_digest: String::new(),
            }
            .seal()
            .map_err(contract_error)?;
            Ok(AppMemoryDestinationApplyResult::Invalidation {
                destination_receipt: receipt,
                source_ack: AppMemoryInvalidationAckPermit {
                    receipt: source_receipt,
                    dispatch_ack: invalidation_dispatch_ack,
                },
            })
        },
    }
}

fn validate_head(head: &AppMemoryDestinationHeadV1) -> Result<(), AgentMemoryError> {
    if head.schema_version != APP_MEMORY_OWNER_SCHEMA
        || (head.generation == 0) != head.latest_receipt_digest.is_none()
    {
        return Err(AgentMemoryError::Validation(
            "app-memory destination head has an invalid shape".to_owned(),
        ));
    }
    if let Some(digest) = &head.latest_receipt_digest {
        validate_digest(digest)?;
    }
    Ok(())
}

fn validate_projection_shape(
    projection: &AppMemoryDestinationProjectionV1,
) -> Result<(), AgentMemoryError> {
    let expected_operations = usize::try_from(
        projection
            .generation
            .saturating_sub(projection.replay_floor_generation),
    )
    .map_err(|_| {
        AgentMemoryError::Validation("app-memory projection generation overflow".to_owned())
    })?;
    if projection.schema_version != APP_MEMORY_OWNER_SCHEMA
        || projection
            .entries
            .values()
            .filter(|entry| {
                matches!(
                    entry.state,
                    AppMemoryProjectionStateV1::Proposed | AppMemoryProjectionStateV1::Accepted
                )
            })
            .count()
            > MAX_PROJECTION_ENTRIES
        || projection.applied_operations.len() != expected_operations
        || projection.replay_floor_generation > projection.generation
        || projection
            .generation
            .saturating_sub(projection.replay_floor_generation)
            > MAX_RECENT_OPERATION_REPLAY
        || projection.invalidation_high_water.len() > MAX_PROJECTION_ENTRIES
        || projection.source_dispatch_high_water.len() > MAX_SOURCE_DISPATCH_SLOTS
        || serde_json::to_vec(&projection.entries)?.len() > MAX_LIVE_ENTRY_BYTES
        || serde_json::to_vec(projection)?.len() > MAX_PROJECTION_BYTES
        || (projection.generation == 0) != projection.latest_receipt_digest.is_none()
        || projection
            .applied_operations
            .values()
            .any(|generation| *generation == 0 || *generation > projection.generation)
    {
        return Err(AgentMemoryError::Validation(
            "app-memory projection has an invalid bounded shape".to_owned(),
        ));
    }
    if let Some(digest) = &projection.latest_receipt_digest {
        validate_digest(digest)?;
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
        .ne(projection.replay_floor_generation.saturating_add(1)..=projection.generation)
    {
        return Err(AgentMemoryError::Validation(
            "app-memory projection operation generations are not contiguous".to_owned(),
        ));
    }
    for operation_digest in projection.applied_operations.keys() {
        validate_digest(operation_digest)?;
    }
    for (key, material) in &projection.source_dispatch_high_water {
        validate_source_dispatch_material(material)?;
        if material.dispatch_slot_key != *key
            || material.generation > projection.generation
            || material
                .acknowledged_predecessor_generation
                .is_some_and(|generation| generation >= material.generation)
        {
            return Err(AgentMemoryError::Validation(
                "app-memory compact source-dispatch high-water was substituted".to_owned(),
            ));
        }
    }
    for (key, high_water) in &projection.invalidation_high_water {
        validate_digest(key)?;
        validate_id(&high_water.proposal_id)?;
        validate_digest(&high_water.proposal_digest)?;
        validate_digest(&high_water.source_identity_digest)?;
        validate_id(&high_water.installation_id)?;
        validate_id(&high_water.scope_binding_ref)?;
        validate_id(&high_water.source_event_ref)?;
        validate_id(&high_water.dedupe_key)?;
        if high_water.source_event_revision == 0 || high_water.state_changed_at_ms < 0 {
            return Err(AgentMemoryError::Validation(
                "app-memory pre-admission tombstone revision is invalid".to_owned(),
            ));
        }
        if let Some(digest) = &high_water.invalidation_digest {
            validate_digest(digest)?;
        }
        if matches!(
            high_water.terminal_reason,
            Some(AppMemoryContributionStateReasonV1::SourceInvalidated)
        ) != high_water.source_invalidation_reason.is_some()
            || matches!(
                high_water.terminal_reason,
                Some(
                    AppMemoryContributionStateReasonV1::AwaitingOwnerReview
                        | AppMemoryContributionStateReasonV1::OwnerAccepted
                        | AppMemoryContributionStateReasonV1::CompactedLegacy
                )
            )
            || matches!(
                high_water.terminal_reason,
                Some(
                    AppMemoryContributionStateReasonV1::OwnerRejected
                        | AppMemoryContributionStateReasonV1::OwnerRevoked
                )
            ) && high_water.source_invalidation_reason.is_some()
            || matches!(
                (high_water.proposal_issued_at_ms, high_water.proposal_expires_at_ms),
                (Some(issued), Some(expires)) if issued < 0 || expires <= issued
            )
            || high_water.proposal_issued_at_ms.is_some()
                != high_water.proposal_expires_at_ms.is_some()
            || high_water.workflow_id.is_some() != high_water.action_id.is_some()
            || high_water.workflow_id.is_some() != high_water.contribution_port_id.is_some()
        {
            return Err(AgentMemoryError::Validation(
                "app-memory compact history metadata is inconsistent".to_owned(),
            ));
        }
        if high_water.invalidation_digest.is_none()
            && high_water.disposition != AppMemoryInvalidationDispositionV1::Tombstoned
        {
            return Err(AgentMemoryError::Validation(
                "synthetic source-head tombstone has an invalid disposition".to_owned(),
            ));
        }
        if source_head_high_water_key(
            &high_water.installation_id,
            &high_water.scope_binding_ref,
            &high_water.source_event_ref,
            &high_water.dedupe_key,
        )? != *key
        {
            return Err(AgentMemoryError::Validation(
                "app-memory pre-admission tombstone key was substituted".to_owned(),
            ));
        }
    }
    let mut exact_source_heads = BTreeSet::new();
    for (proposal_digest, entry) in &projection.entries {
        validate_digest(proposal_digest)?;
        if entry.proposal.proposal_digest != *proposal_digest {
            return Err(AgentMemoryError::Validation(
                "app-memory projection key mismatch".to_owned(),
            ));
        }
        entry.proposal.validate().map_err(contract_error)?;
        let source = entry.proposal.header.sources.first().ok_or_else(|| {
            AgentMemoryError::Validation("V1 projection entry has no exact source".to_owned())
        })?;
        if entry.proposal.header.update_policy
            != magician_app_contract::contribution::AppContributionUpdatePolicy::ReplaceExactSourceHead
            || entry.proposal.header.retraction_policy
                != magician_app_contract::contribution::AppContributionRetractionPolicy::TombstoneOnAnySourceDrift
            || entry.proposal.header.sources.len() != 1
            || entry.source_event_revision != source.record_revision
            || entry.state_changed_at_ms < 0
            || !exact_source_heads.insert((
                entry.proposal.header.installation_id.clone(),
                entry.proposal.header.dedupe_key.clone(),
            ))
            || projection.invalidation_high_water.get(
                &source_head_high_water_key(
                    &entry.proposal.header.installation_id,
                    &entry.proposal.header.scope_binding_ref,
                    &source.canonical_source_ref,
                    &entry.proposal.header.dedupe_key,
                )?,
            ).is_some_and(|high_water| {
                high_water.proposal_digest == entry.proposal.proposal_digest
                    || high_water.source_event_revision > source.record_revision
            })
        {
            return Err(AgentMemoryError::Validation(
                "app-memory projection contains an invalid exact source head".to_owned(),
            ));
        }
        if let Some(digest) = &entry.owner_decision_receipt_digest {
            validate_digest(digest)?;
        }
        if let Some(digest) = &entry.invalidation_digest {
            validate_digest(digest)?;
        }
        match entry.state {
            AppMemoryProjectionStateV1::Proposed
                if entry.owner_decision_receipt_digest.is_some()
                    || entry.invalidation_digest.is_some() =>
            {
                return Err(AgentMemoryError::Validation(
                    "proposed app-memory entry carries terminal evidence".to_owned(),
                ));
            },
            AppMemoryProjectionStateV1::Accepted
                if entry.owner_decision_receipt_digest.is_none()
                    || entry.invalidation_digest.is_some() =>
            {
                return Err(AgentMemoryError::Validation(
                    "decided app-memory entry lacks exact decision evidence".to_owned(),
                ));
            },
            AppMemoryProjectionStateV1::Rejected | AppMemoryProjectionStateV1::Tombstoned => {
                return Err(AgentMemoryError::Validation(
                    "terminal app-memory entries must compact into source high-water".to_owned(),
                ));
            },
            _ => {},
        }
    }
    let mut bounded_source_keys = projection
        .invalidation_high_water
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    bounded_source_keys.extend(
        projection
            .source_dispatch_high_water
            .values()
            .map(|material| material.source_head_key.clone()),
    );
    for entry in projection.entries.values() {
        let source = entry.proposal.header.sources.first().ok_or_else(|| {
            AgentMemoryError::Validation("V1 projection entry lost its source".to_owned())
        })?;
        bounded_source_keys.insert(source_head_high_water_key(
            &entry.proposal.header.installation_id,
            &entry.proposal.header.scope_binding_ref,
            &source.canonical_source_ref,
            &entry.proposal.header.dedupe_key,
        )?);
    }
    if bounded_source_keys.len() > MAX_PROJECTION_ENTRIES {
        return Err(AgentMemoryError::Validation(
            "app-memory source-head high-water quota is exceeded".to_owned(),
        ));
    }
    Ok(())
}

fn projection_state_digest(
    projection: &AppMemoryDestinationProjectionV1,
) -> Result<String, AgentMemoryError> {
    let mut logical = projection.clone();
    logical.latest_receipt_digest = None;
    logical.source_dispatch_high_water.clear();
    digest_serialized("magician.app-memory-projection-state.v1", &logical)
}

async fn write_immutable_receipt(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
    receipt: &AppMemoryDestinationReceiptV1,
) -> Result<(), AgentMemoryError> {
    receipt.validate().map_err(contract_error)?;
    let path = paths.receipt(receipt.generation);
    let bytes = serde_json::to_vec(receipt)?;
    if bytes.len() > APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES {
        return Err(AgentMemoryError::Validation(
            "app-memory receipt exceeds bound".to_owned(),
        ));
    }
    match storage
        .read_private_bytes_bounded(&path, APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES)
        .await
    {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {
            return Err(AgentMemoryError::Validation(
                "app-memory receipt generation was substituted".to_owned(),
            ))
        },
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    storage.write_private_bytes_atomic(path, &bytes).await?;
    Ok(())
}

async fn write_immutable_source_dispatch(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
    material: &AppMemorySourceDispatchHighWaterV1,
) -> Result<(), AgentMemoryError> {
    validate_source_dispatch_material(material)?;
    let path = paths.source_dispatch(material.generation);
    let bytes = serde_json::to_vec(material)?;
    match storage
        .read_private_bytes_bounded(&path, MAX_SOURCE_DISPATCH_MATERIAL_BYTES)
        .await
    {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => {
            return Err(AgentMemoryError::Validation(
                "app-memory source-dispatch generation was substituted".to_owned(),
            ));
        },
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    storage.write_private_bytes_atomic(path, &bytes).await?;
    Ok(())
}

async fn read_receipt(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
    generation: u64,
) -> Result<AppMemoryDestinationReceiptV1, AgentMemoryError> {
    let bytes = storage
        .read_private_bytes_bounded(
            paths.receipt(generation),
            APP_CONTRIBUTION_MAX_DESTINATION_RECEIPT_BYTES,
        )
        .await?;
    let receipt: AppMemoryDestinationReceiptV1 = serde_json::from_slice(&bytes)?;
    receipt.validate().map_err(contract_error)?;
    if receipt.generation != generation {
        return Err(AgentMemoryError::Validation(
            "app-memory receipt filename/generation mismatch".to_owned(),
        ));
    }
    Ok(receipt)
}

async fn read_optional_receipt(
    storage: &super::storage::AgentStorage,
    paths: &OwnerPaths,
    generation: u64,
) -> Result<Option<AppMemoryDestinationReceiptV1>, AgentMemoryError> {
    match read_receipt(storage, paths, generation).await {
        Ok(value) => Ok(Some(value)),
        Err(AgentMemoryError::Storage(AgentStorageError::Io(error)))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        },
        Err(error) => Err(error),
    }
}

async fn read_optional_json<T: for<'de> Deserialize<'de>>(
    storage: &super::storage::AgentStorage,
    path: &Path,
    max_bytes: usize,
) -> Result<Option<T>, AgentMemoryError> {
    match storage.read_private_bytes_bounded(path, max_bytes).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        },
        Err(error) => Err(error.into()),
    }
}

async fn write_json_private<T: Serialize>(
    storage: &super::storage::AgentStorage,
    path: &Path,
    value: &T,
    max_bytes: usize,
) -> Result<(), AgentMemoryError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > max_bytes {
        return Err(AgentMemoryError::Validation(
            "app-memory owner document exceeds bound".to_owned(),
        ));
    }
    storage.write_private_bytes_atomic(path, &bytes).await?;
    Ok(())
}

fn digest_serialized<T: Serialize>(domain: &str, value: &T) -> Result<String, AgentMemoryError> {
    let bytes = serde_json::to_vec(value)?;
    let mut framed = Vec::with_capacity(domain.len() + bytes.len() + 16);
    framed.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    framed.extend_from_slice(domain.as_bytes());
    framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    framed.extend_from_slice(&bytes);
    Ok(content_digest(&framed))
}

fn validate_id(value: &str) -> Result<(), AgentMemoryError> {
    if value.is_empty()
        || value.len() > 192
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(AgentMemoryError::Validation(
            "app-memory identity is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), AgentMemoryError> {
    let valid = value.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if !valid {
        return Err(AgentMemoryError::Validation(
            "app-memory digest is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn contract_error(
    error: magician_app_contract::contribution::AppContributionContractError,
) -> AgentMemoryError {
    AgentMemoryError::Validation(format!("invalid app-memory contract: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_app_contract::contribution::{
        AppContributionClassification, AppContributionEvidenceClass,
        AppContributionHandlingLabelsV1, AppContributionModelProcessing,
        AppContributionRetractionPolicy, AppContributionSettlementRefV1,
        AppContributionSourceHeaderV1, AppContributionSourceRefV1, AppContributionUpdatePolicy,
        AppMemoryInvalidationReasonV1, AppMemoryInvalidationV1, AppMemorySemanticDestinationV1,
        AppMemoryTierScopeV1, APP_MEMORY_CANDIDATE_CONTRACT_ID,
    };

    static_assertions::assert_not_impl_any!(
        AppMemoryIngressAckPermit: Clone, serde::Serialize, serde::de::DeserializeOwned
    );
    static_assertions::assert_not_impl_any!(
        AppMemoryInvalidationAckPermit: Clone, serde::Serialize, serde::de::DeserializeOwned
    );

    fn digest(value: &str) -> String {
        content_digest(value.as_bytes())
    }

    fn proposal() -> AppMemoryCandidateProposalV1 {
        let claim = "owner-reviewed candidate".to_owned();
        AppMemoryCandidateProposalV1 {
            header: AppContributionSourceHeaderV1 {
                contract_version: 1,
                destination_contract_id: APP_MEMORY_CANDIDATE_CONTRACT_ID.to_owned(),
                destination_contract_version: 1,
                destination_schema_digest: digest("destination-schema"),
                proposal_id: "proposal:1".to_owned(),
                proposal_revision: 1,
                scope_binding_ref: "scope:1".to_owned(),
                installation_id: "installation:1".to_owned(),
                installation_generation: 1,
                package_revision_ref: "package:1".to_owned(),
                package_content_digest: digest("package"),
                grant_revision: 1,
                grant_authority_digest: digest("grant"),
                schema_revision: 1,
                schema_digest: digest("schema"),
                workflow_id: "workflow:1".to_owned(),
                workflow_digest: digest("workflow"),
                action_id: "action:1".to_owned(),
                action_digest: digest("action"),
                contribution_port_id: "memory".to_owned(),
                contribution_port_digest: digest("port"),
                settlement: AppContributionSettlementRefV1::Mutation {
                    mutation_receipt_id: "mutation:1".to_owned(),
                    first_change_sequence: 1,
                    last_change_sequence: 1,
                },
                sources: vec![AppContributionSourceRefV1 {
                    installation_id: "installation:1".to_owned(),
                    entity_name: "note".to_owned(),
                    record_id: "record:1".to_owned(),
                    record_revision: 1,
                    selected_fields: vec!["summary".to_owned()],
                    canonical_source_ref: "source:1".to_owned(),
                    canonical_source_digest: digest("source"),
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
                purpose: "owner-review".to_owned(),
                audiences: vec!["personal-agent".to_owned()],
                evidence_class: AppContributionEvidenceClass::Derived,
                issued_at_ms: 1,
                expires_at_ms: 2_000,
                dedupe_key: "dedupe:1".to_owned(),
                update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
                retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
            },
            intended_tier_scope: AppMemoryTierScopeV1::User,
            semantic_destination: AppMemorySemanticDestinationV1::Knowledge,
            claim_digest: content_digest(claim.as_bytes()),
            claim_or_summary: claim,
            evidence_refs: vec!["source:1".to_owned()],
            proposal_digest: String::new(),
        }
        .seal()
        .expect("proposal")
    }

    #[test]
    fn empty_projection_is_closed_and_bounded() {
        let projection = empty_projection();
        validate_projection_shape(&projection).expect("empty projection");
        let mut value = serde_json::to_value(&projection).expect("projection json");
        value
            .as_object_mut()
            .expect("object")
            .insert("rank".to_owned(), serde_json::json!(1));
        assert!(serde_json::from_value::<AppMemoryDestinationProjectionV1>(value).is_err());
    }

    #[test]
    fn receipt_filename_is_fixed_width_generation_order() {
        let paths = OwnerPaths::new(Path::new("root"));
        assert!(paths.receipt(2) < paths.receipt(10));
    }

    #[test]
    fn replay_window_and_checkpoint_shape_are_permanently_bounded() {
        let mut projection = empty_projection();
        projection.generation = MAX_RECENT_OPERATION_REPLAY + 17;
        projection.latest_receipt_digest = Some(digest("latest"));
        for generation in 1..=projection.generation {
            projection
                .applied_operations
                .insert(digest(&format!("operation:{generation}")), generation);
        }
        advance_replay_window(&mut projection);
        assert_eq!(projection.replay_floor_generation, 17);
        assert_eq!(
            projection.applied_operations.len(),
            MAX_RECENT_OPERATION_REPLAY as usize
        );
        validate_projection_shape(&projection).expect("bounded replay projection");

        let mut checkpoint = AppMemoryDestinationCheckpointV1 {
            schema_version: APP_MEMORY_OWNER_SCHEMA,
            generation: projection.generation,
            previous_receipt_digest: projection.latest_receipt_digest.clone(),
            projection_digest: projection_state_digest(&projection).expect("projection digest"),
            source_high_water_digest: digest_serialized(
                "magician.app-memory-source-high-water-set.v1",
                &(
                    &projection.invalidation_high_water,
                    &projection.source_dispatch_high_water,
                ),
            )
            .expect("source high-water digest"),
            projection,
            checkpoint_digest: String::new(),
        };
        checkpoint.checkpoint_digest = checkpoint_digest(&checkpoint).expect("checkpoint digest");
        validate_checkpoint(&checkpoint).expect("checkpoint");
    }

    #[test]
    fn compact_source_dispatch_reconstructs_exact_response_lost_receipt() {
        let proposal = proposal();
        let operation = AppMemoryDestinationOperationV1::StageProposal {
            proposal: proposal.clone(),
        };
        let mut projection = empty_projection();
        apply_operation(&mut projection, &operation, 1).expect("stage");
        projection.generation = 1;
        let operation_digest =
            digest_serialized("magician.app-memory-operation.v1", &operation).expect("digest");
        projection
            .applied_operations
            .insert(operation_digest.clone(), 1);
        let receipt = AppMemoryDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "app-memory-receipt:1".to_owned(),
            generation: 1,
            previous_receipt_digest: None,
            operation: operation.clone(),
            invalidation_disposition: None,
            resulting_projection_digest: projection_state_digest(&projection)
                .expect("projection digest"),
            recorded_at_ms: 1,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("receipt");
        let material = source_dispatch_material(&receipt, &operation_digest, None)
            .expect("material")
            .expect("source material");
        assert_eq!(
            receipt_from_source_dispatch_material(&material, &operation).expect("reconstructed"),
            receipt,
        );
        assert!(
            MAX_PROJECTION_BYTES
                >= MAX_LIVE_ENTRY_BYTES + MAX_PROJECTION_ENTRIES * MAX_COMPACTED_SOURCE_HEAD_BYTES
        );
    }

    #[test]
    fn rejected_entry_compacts_into_one_source_high_water() {
        let proposal = proposal();
        let mut projection = empty_projection();
        apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::StageProposal {
                proposal: proposal.clone(),
            },
            1,
        )
        .expect("stage");
        apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::Decide {
                proposal_id: proposal.header.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                decision: AppMemoryOwnerDecisionV1::Reject,
                owner_decision_receipt_digest: digest("reject"),
                retained_until_ms: None,
            },
            2,
        )
        .expect("reject");
        assert!(projection.entries.is_empty());
        assert_eq!(projection.invalidation_high_water.len(), 1);
    }

    #[test]
    fn accepted_entry_requires_an_exact_owner_revoke_and_compacts_once() {
        let proposal = proposal();
        let mut projection = empty_projection();
        apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::StageProposal {
                proposal: proposal.clone(),
            },
            1,
        )
        .expect("stage");
        apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::Decide {
                proposal_id: proposal.header.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                decision: AppMemoryOwnerDecisionV1::Accept,
                owner_decision_receipt_digest: digest("accept"),
                retained_until_ms: Some(proposal.header.expires_at_ms),
            },
            2,
        )
        .expect("accept");
        assert!(apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::Decide {
                proposal_id: proposal.header.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                decision: AppMemoryOwnerDecisionV1::Reject,
                owner_decision_receipt_digest: digest("wrong-transition"),
                retained_until_ms: None,
            },
            3,
        )
        .is_err());
        apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::Decide {
                proposal_id: proposal.header.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                decision: AppMemoryOwnerDecisionV1::Revoke,
                owner_decision_receipt_digest: digest("revoke"),
                retained_until_ms: None,
            },
            3,
        )
        .expect("revoke");
        assert!(projection.entries.is_empty());
        let terminal = projection
            .invalidation_high_water
            .values()
            .next()
            .expect("terminal high-water");
        assert_eq!(
            terminal.terminal_reason,
            Some(AppMemoryContributionStateReasonV1::OwnerRevoked)
        );
        assert_eq!(terminal.state_changed_at_ms, 3);
        let source = proposal.header.sources.first().expect("source");
        let invalidation = AppMemoryInvalidationV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            invalidation_id: "invalidation:after-owner-revoke".to_owned(),
            installation_id: proposal.header.installation_id.clone(),
            scope_binding_ref: proposal.header.scope_binding_ref.clone(),
            proposal_id: proposal.header.proposal_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            source_event_ref: source.canonical_source_ref.clone(),
            source_event_revision: source.record_revision,
            source_identity_digest: source.canonical_source_digest.clone(),
            dedupe_key: proposal.header.dedupe_key.clone(),
            reason: AppMemoryInvalidationReasonV1::GrantRevoked,
            issued_at_ms: 4,
            invalidation_digest: String::new(),
        }
        .seal()
        .expect("post-revoke invalidation");
        assert_eq!(
            apply_operation(
                &mut projection,
                &AppMemoryDestinationOperationV1::Invalidate { invalidation },
                4,
            )
            .expect("source invalidation advances an owner tombstone"),
            Some(AppMemoryInvalidationDispositionV1::AlreadyTombstoned),
        );
        assert_eq!(
            projection
                .invalidation_high_water
                .values()
                .next()
                .and_then(|head| head.source_invalidation_reason),
            Some(AppMemoryInvalidationReasonV1::GrantRevoked),
        );
    }

    #[test]
    fn live_admission_quota_never_blocks_decision_or_invalidation() {
        let proposal = proposal();
        let mut projection = empty_projection();
        projection.entries.insert(
            proposal.proposal_digest.clone(),
            AppMemoryProjectionEntryV1 {
                proposal: proposal.clone(),
                state: AppMemoryProjectionStateV1::Proposed,
                owner_decision_receipt_digest: None,
                retained_until_ms: None,
                invalidation_digest: None,
                source_event_revision: 1,
                state_changed_at_ms: 1,
            },
        );
        for index in 1..MAX_PROJECTION_ENTRIES {
            projection.entries.insert(
                digest(&format!("quota-entry:{index}")),
                AppMemoryProjectionEntryV1 {
                    proposal: proposal.clone(),
                    state: AppMemoryProjectionStateV1::Proposed,
                    owner_decision_receipt_digest: None,
                    retained_until_ms: None,
                    invalidation_digest: None,
                    source_event_revision: 1,
                    state_changed_at_ms: 1,
                },
            );
        }
        apply_operation(
            &mut projection,
            &AppMemoryDestinationOperationV1::Decide {
                proposal_id: proposal.header.proposal_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                decision: AppMemoryOwnerDecisionV1::Accept,
                owner_decision_receipt_digest: digest("owner-decision"),
                retained_until_ms: Some(proposal.header.expires_at_ms),
            },
            2,
        )
        .expect("decision remains a safety transition");
        let source = proposal.header.sources.first().expect("source");
        let invalidation = AppMemoryInvalidationV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            invalidation_id: "invalidation:quota".to_owned(),
            installation_id: proposal.header.installation_id.clone(),
            scope_binding_ref: proposal.header.scope_binding_ref.clone(),
            proposal_id: proposal.header.proposal_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            source_event_ref: source.canonical_source_ref.clone(),
            source_event_revision: 2,
            source_identity_digest: source.canonical_source_digest.clone(),
            dedupe_key: proposal.header.dedupe_key.clone(),
            reason: AppMemoryInvalidationReasonV1::SourceDeleted,
            issued_at_ms: 20,
            invalidation_digest: String::new(),
        }
        .seal()
        .expect("invalidation");
        assert_eq!(
            apply_operation(
                &mut projection,
                &AppMemoryDestinationOperationV1::Invalidate { invalidation },
                20,
            )
            .expect("invalidation remains a safety transition"),
            Some(AppMemoryInvalidationDispositionV1::Tombstoned)
        );
    }

    #[tokio::test]
    async fn exact_replay_and_head_to_projection_repair_are_deterministic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        let operation = AppMemoryDestinationOperationV1::StageProposal {
            proposal: proposal(),
        };
        let first = service
            .apply_app_memory_destination_operation(operation.clone(), 10, None, None, None, None)
            .await
            .expect("first");
        let replay = service
            .apply_app_memory_destination_operation(operation, 999, None, None, None, None)
            .await
            .expect("replay");
        assert_eq!(first, replay);

        let paths = OwnerPaths::new(service.app_contribution_storage().root());
        std::fs::remove_file(&paths.head).expect("simulate crash after receipt");
        std::fs::remove_file(&paths.projection).expect("drop derived projection");
        let repaired = service
            .recover_app_memory_destination()
            .await
            .expect("repair");
        assert_eq!(repaired.generation, 1);
        assert_eq!(repaired.entries.len(), 1);
    }

    #[tokio::test]
    async fn staged_proposal_is_never_materialized_in_hybrid_index_projection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: proposal(),
                },
                10,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("stage destination proposal");

        super::super::memory_prompt_blocks::synchronize_app_memory_index_projection(
            &service,
            Utc::now(),
        )
        .await
        .expect("reconcile destination-owned index projection");
        let projection =
            magician_vector_index::memory_candidates::load_app_memory_index_projection(
                service.storage(),
            )
            .await
            .expect("load index projection")
            .expect("projection exists");

        assert!(projection.entries.is_empty());
        assert_eq!(projection.destination_generation, 1);
    }

    #[tokio::test]
    async fn pre_receipt_source_dispatch_orphan_is_removed_before_generation_reuse() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        let storage = service.app_contribution_storage();
        let paths = OwnerPaths::new(storage.root());
        let operation = AppMemoryDestinationOperationV1::StageProposal {
            proposal: proposal(),
        };
        let operation_digest =
            digest_serialized("magician.app-memory-operation.v1", &operation).expect("digest");
        let mut projection = empty_projection();
        assert_eq!(
            apply_operation(&mut projection, &operation, 10).expect("stage orphan"),
            None,
        );
        projection.generation = 1;
        projection
            .applied_operations
            .insert(operation_digest.clone(), 1);
        let orphan_receipt = AppMemoryDestinationReceiptV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            receipt_id: "app-memory-receipt:orphan".to_owned(),
            generation: 1,
            previous_receipt_digest: None,
            operation: operation.clone(),
            invalidation_disposition: None,
            resulting_projection_digest: projection_state_digest(&projection)
                .expect("projection digest"),
            recorded_at_ms: 10,
            receipt_digest: String::new(),
        }
        .seal()
        .expect("orphan receipt");
        let orphan_material = source_dispatch_material(&orphan_receipt, &operation_digest, None)
            .expect("source material")
            .expect("proposal source material");
        storage
            .ensure_private_directory(&paths.source_dispatches)
            .await
            .expect("private source-dispatch directory");
        write_immutable_source_dispatch(storage, &paths, &orphan_material)
            .await
            .expect("pre-receipt source material");
        assert!(!paths.receipt(1).exists());

        let applied = service
            .apply_app_memory_destination_operation(operation, 20, None, None, None, None)
            .await
            .expect("retry reuses uncommitted generation");
        let AppMemoryDestinationApplyResult::Proposal {
            destination_receipt,
            ..
        } = applied
        else {
            panic!("expected proposal result");
        };
        assert_eq!(destination_receipt.generation, 1);
        assert_eq!(destination_receipt.recorded_at_ms, 20);
        let committed_material: AppMemorySourceDispatchHighWaterV1 = read_optional_json(
            storage,
            &paths.source_dispatch(1),
            MAX_SOURCE_DISPATCH_MATERIAL_BYTES,
        )
        .await
        .expect("read committed material")
        .expect("committed material");
        assert_eq!(committed_material.recorded_at_ms, 20);
        assert_ne!(
            committed_material.receipt_digest,
            orphan_material.receipt_digest
        );
    }

    #[tokio::test]
    async fn substituted_sealed_bytes_never_replay() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        let original = proposal();
        service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: original.clone(),
                },
                10,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("first");
        let mut substituted = original;
        substituted.claim_or_summary.push_str(" substituted");
        assert!(service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: substituted
                },
                11,
                None,
                None,
                None,
                None,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn invalidation_before_admission_persists_an_exact_tombstone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        let proposal = proposal();
        let source = proposal.header.sources.first().expect("source");
        let invalidation = AppMemoryInvalidationV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            invalidation_id: "invalidation:before-admission".to_owned(),
            installation_id: proposal.header.installation_id.clone(),
            scope_binding_ref: proposal.header.scope_binding_ref.clone(),
            proposal_id: proposal.header.proposal_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            source_event_ref: source.canonical_source_ref.clone(),
            source_event_revision: source.record_revision + 1,
            source_identity_digest: source.canonical_source_digest.clone(),
            dedupe_key: proposal.header.dedupe_key.clone(),
            reason: AppMemoryInvalidationReasonV1::SourceDeleted,
            issued_at_ms: 20,
            invalidation_digest: String::new(),
        }
        .seal()
        .expect("sealed invalidation");
        let result = service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::Invalidate { invalidation },
                20,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("pre-admission tombstone");
        let AppMemoryDestinationApplyResult::Invalidation {
            destination_receipt,
            ..
        } = result
        else {
            panic!("expected invalidation result");
        };
        assert_eq!(
            destination_receipt.invalidation_disposition,
            Some(AppMemoryInvalidationDispositionV1::SupersededBeforeAdmission)
        );
        assert!(service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal { proposal },
                21,
                None,
                None,
                None,
                None,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn replace_exact_source_head_retracts_the_prior_projection_atomically() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        let first = proposal();
        service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: first.clone(),
                },
                10,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("first source head");

        let mut second = first.clone();
        second.header.proposal_id = "proposal:2".to_owned();
        second.header.proposal_revision = 2;
        second.header.sources[0].record_revision = 2;
        second.header.sources[0].canonical_source_digest = digest("source:revision:2");
        second.claim_or_summary = "replacement owner-reviewed candidate".to_owned();
        second.claim_digest = content_digest(second.claim_or_summary.as_bytes());
        second.proposal_digest.clear();
        let second = second.seal().expect("replacement proposal");
        service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: second.clone(),
                },
                11,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("replacement source head");

        let projection = service
            .recover_app_memory_destination()
            .await
            .expect("projection");
        assert_eq!(projection.entries.len(), 1);
        assert!(projection.entries.contains_key(&second.proposal_digest));
        assert!(!projection.entries.contains_key(&first.proposal_digest));
        assert_eq!(projection.invalidation_high_water.len(), 1);
    }

    #[tokio::test]
    async fn acknowledged_source_head_rejects_a_missing_destination_prefix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AgentMemoryService::with_base_path(temp.path());
        let first = service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: proposal(),
                },
                10,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("destination head");
        let AppMemoryDestinationApplyResult::Proposal {
            destination_receipt,
            ..
        } = first
        else {
            panic!("expected proposal result");
        };
        let expected = AppMemoryExpectedDestinationHead {
            generation: destination_receipt.generation,
            receipt_digest: destination_receipt.receipt_digest,
        };
        let paths = OwnerPaths::new(service.app_contribution_storage().root());
        std::fs::remove_dir_all(paths.authority_root).expect("simulate whole-prefix loss");

        assert!(service
            .apply_app_memory_destination_operation(
                AppMemoryDestinationOperationV1::StageProposal {
                    proposal: proposal(),
                },
                11,
                Some(expected),
                None,
                None,
                None,
            )
            .await
            .is_err());
    }
}
