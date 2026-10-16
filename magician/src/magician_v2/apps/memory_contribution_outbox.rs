//! Typed, inert delivery journals for app memory proposals and invalidations.
//!
//! Lock order is fixed: registry per-scope async writer -> SQLite IMMEDIATE
//! transaction -> one outbox row lease. Destination storage locks are never
//! held while these functions run. Acknowledgement always happens in a later
//! source transaction after the destination has durably returned its exact
//! sealed receipt.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use magician_app_contract::contribution::{
    content_digest, AppContributionSettlementRefV1, AppMemoryCandidateProposalV1,
    AppMemoryIngressReceiptV1, AppMemoryInvalidationReasonV1, AppMemoryInvalidationReceiptV1,
    AppMemoryInvalidationV1, APP_CONTRIBUTION_CONTRACT_VERSION,
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES, APP_CONTRIBUTION_MAX_INVALIDATION_BYTES,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use uuid::Uuid;

use super::{
    authority::AuthenticatedAppScope,
    contribution::{
        enforce_memory_contribution_quota, invalidation_advances_exact_source,
        AppContributionError, PreparedMemoryContribution, PreparedMemoryInvalidation,
        APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE,
    },
    records::AppScope,
    registry::{AppRegistryError, AppRegistryService},
};
use crate::magician_v2::agents::app_memory_ingress::{
    AppMemoryIngressAckPermit, AppMemoryInvalidationAckPermit,
};

const MAX_CLAIM_BATCH: usize = 16;
const MAX_LEASE: StdDuration = StdDuration::from_secs(5 * 60);
#[allow(dead_code)] // Retained for the explicit pre-dispatch release compatibility seam.
const MAX_RETRY_DELAY: StdDuration = StdDuration::from_secs(60 * 60);
// One immutable leased/dispatching row plus one coalesced maximum pending row
// may exist for each exact source head.
const MAX_INVALIDATION_ROWS_PER_SCOPE: i64 = APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE * 2;
const MAX_INVALIDATION_BYTES_PER_SCOPE: i64 = MAX_INVALIDATION_ROWS_PER_SCOPE
    * (APP_CONTRIBUTION_MAX_INVALIDATION_BYTES + APP_CONTRIBUTION_MAX_DOCUMENT_BYTES) as i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMemoryOutboxAppendOutcome {
    Appended,
    ExactReplay,
    /// A newer/equivalent safety invalidation for the same exact source head
    /// is already pending. V1 retains one durable invalidation owner per head.
    Coalesced,
}

#[derive(Debug)]
pub struct AppMemoryContributionLease {
    sequence: i64,
    event_id: String,
    proposal: AppMemoryCandidateProposalV1,
    proposal_json: Vec<u8>,
    lease_owner: String,
    lease_token: String,
    lease_expires_at: DateTime<Utc>,
    #[allow(dead_code)] // Persisted lease telemetry; retry ownership is epoch-based.
    attempt_count: u32,
    lease_epoch: u64,
    principal: String,
    workspace: String,
    scope_binding_ref: String,
}

impl AppMemoryContributionDispatchEvidence {
    pub(crate) fn matches_destination_scope(&self, authenticated: &AuthenticatedAppScope) -> bool {
        self.principal == authenticated.scope().principal.as_str()
            && self.workspace == authenticated.scope().workspace.as_str()
            && self.proposal.header.scope_binding_ref == self.scope_binding_ref
            && self.scope_binding_ref == authenticated.scope_binding_ref().as_str()
    }
}

#[derive(Debug)]
pub struct AppMemoryInvalidationLease {
    sequence: i64,
    event_id: String,
    installation_id: String,
    invalidation: AppMemoryInvalidationV1,
    invalidation_json: Vec<u8>,
    source_proposal_json: Vec<u8>,
    lease_owner: String,
    lease_token: String,
    lease_expires_at: DateTime<Utc>,
    #[allow(dead_code)] // Persisted lease telemetry; retry ownership is epoch-based.
    attempt_count: u32,
    lease_epoch: u64,
    principal: String,
    workspace: String,
    scope_binding_ref: String,
}

fn validate_retained_invalidation_source(
    prepared: &PreparedMemoryInvalidation,
    installation_id: &str,
    source_entity_name: &str,
    source_record_id: &str,
    source_proposal_json: &[u8],
) -> Result<(), AppContributionError> {
    let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(source_proposal_json)?;
    let proposal = PreparedMemoryContribution::new(proposal)?;
    let source = proposal.proposal.header.sources.first().ok_or_else(|| {
        AppRegistryError::InvalidControlPlane(
            "retained invalidation source proposal has no exact source".to_owned(),
        )
    })?;
    if proposal.proposal.header.installation_id != installation_id
        || proposal.proposal.header.proposal_id != prepared.invalidation.proposal_id
        || proposal.proposal.proposal_digest != prepared.invalidation.proposal_digest
        || proposal.proposal.header.dedupe_key != prepared.invalidation.dedupe_key
        || proposal.proposal.header.scope_binding_ref != prepared.invalidation.scope_binding_ref
        || source.entity_name != source_entity_name
        || source.record_id != source_record_id
        || source.canonical_source_ref != prepared.invalidation.source_event_ref
        || source.canonical_source_digest != prepared.invalidation.source_identity_digest
        || !invalidation_advances_exact_source(
            prepared.invalidation.reason,
            prepared.invalidation.source_event_revision,
            source.record_revision,
        )
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppMemoryExpectedDestinationHead {
    pub generation: u64,
    pub receipt_digest: String,
}

#[derive(Debug)]
pub struct AppMemoryContributionDispatchPermit {
    evidence: AppMemoryContributionDispatchEvidence,
}

#[derive(Debug)]
pub struct AppMemoryInvalidationDispatchPermit {
    evidence: AppMemoryInvalidationDispatchEvidence,
}

#[derive(Debug)]
pub(crate) struct AppMemoryContributionDispatchEvidence {
    pub proposal: AppMemoryCandidateProposalV1,
    pub dispatch_ack: AppMemoryContributionDispatchAck,
    pub principal: String,
    pub workspace: String,
    pub scope_binding_ref: String,
    pub expected_destination_head: Option<AppMemoryExpectedDestinationHead>,
    pub absolute_expires_at_ms: i64,
}

#[derive(Debug)]
pub(crate) struct AppMemoryInvalidationDispatchEvidence {
    pub invalidation: AppMemoryInvalidationV1,
    pub dispatch_ack: AppMemoryInvalidationDispatchAck,
    pub principal: String,
    pub workspace: String,
    pub scope_binding_ref: String,
    pub expected_destination_head: Option<AppMemoryExpectedDestinationHead>,
    pub absolute_expires_at_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AppMemoryContributionDispatchAck {
    sequence: i64,
    event_id: String,
    payload_json: Vec<u8>,
    lease_owner: String,
    lease_token: String,
    lease_epoch: u64,
    lease_expires_at: DateTime<Utc>,
    principal: String,
    workspace: String,
    scope_binding_ref: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AppMemoryInvalidationDispatchAck {
    sequence: i64,
    event_id: String,
    payload_json: Vec<u8>,
    lease_owner: String,
    lease_token: String,
    lease_epoch: u64,
    lease_expires_at: DateTime<Utc>,
    principal: String,
    workspace: String,
    scope_binding_ref: String,
}

impl AppMemoryContributionDispatchPermit {
    pub(crate) fn into_evidence(
        self,
        now_ms: i64,
    ) -> Result<AppMemoryContributionDispatchEvidence, AppContributionError> {
        if now_ms < 0 || now_ms >= self.evidence.absolute_expires_at_ms {
            return Err(AppRegistryError::OutboxLeaseStale.into());
        }
        Ok(self.evidence)
    }
}

impl AppMemoryInvalidationDispatchPermit {
    pub(crate) fn into_evidence(
        self,
        now_ms: i64,
    ) -> Result<AppMemoryInvalidationDispatchEvidence, AppContributionError> {
        if now_ms < 0 || now_ms >= self.evidence.absolute_expires_at_ms {
            return Err(AppRegistryError::OutboxLeaseStale.into());
        }
        Ok(self.evidence)
    }
}

impl AppMemoryInvalidationDispatchEvidence {
    pub(crate) fn matches_destination_scope(&self, authenticated: &AuthenticatedAppScope) -> bool {
        self.principal == authenticated.scope().principal.as_str()
            && self.workspace == authenticated.scope().workspace.as_str()
            && self.scope_binding_ref == authenticated.scope_binding_ref().as_str()
            && self.invalidation.scope_binding_ref == self.scope_binding_ref
    }
}

pub(crate) fn append_memory_contribution_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    proposal: AppMemoryCandidateProposalV1,
    settlement_ref: &str,
    replacement_reason: Option<AppMemoryInvalidationReasonV1>,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    validate_v17_storage_accounting(transaction)?;
    let prepared = PreparedMemoryContribution::new(proposal)?;
    let sealed_settlement_ref = match &prepared.proposal.header.settlement {
        AppContributionSettlementRefV1::Mutation {
            mutation_receipt_id,
            ..
        } => mutation_receipt_id.as_str(),
        AppContributionSettlementRefV1::TypedResult { result_ref, .. } => result_ref.as_str(),
    };
    if prepared.proposal.header.installation_id.is_empty()
        || settlement_ref.is_empty()
        || scope_binding_ref.is_empty()
        || prepared.proposal.header.scope_binding_ref != scope_binding_ref
        || settlement_ref.len() > 192
        || settlement_ref != sealed_settlement_ref
        || prepared.proposal.header.issued_at_ms > now.timestamp_millis()
        || prepared.proposal.header.expires_at_ms <= now.timestamp_millis()
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory contribution settlement identity is invalid".to_owned(),
        )
        .into());
    }
    if let Some((
        stored_proposal_digest,
        stored_payload_digest,
        stored_installation,
        stored_dedupe,
        stored_revision,
        stored_source_ref,
        stored_source_revision,
        stored_source_digest,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
        stored_entity,
        stored_record,
    )) = transaction
        .query_row(
            "SELECT proposal_digest, payload_digest, installation_id, dedupe_key,
                    proposal_revision, source_event_ref, source_event_revision,
                    source_identity_digest, principal, workspace, scope_binding_ref,
                    source_entity_name, source_record_id
               FROM app_memory_contribution_terminal WHERE event_id=?1",
            [&prepared.event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            },
        )
        .optional()?
    {
        let source = prepared.proposal.header.sources.first().ok_or_else(|| {
            AppRegistryError::InvalidControlPlane("V1 contribution lost its source".to_owned())
        })?;
        if stored_proposal_digest == prepared.proposal.proposal_digest
            && stored_payload_digest == prepared.payload_digest
            && stored_installation == prepared.proposal.header.installation_id
            && stored_dedupe == prepared.proposal.header.dedupe_key
            && u64::try_from(stored_revision).ok()
                == Some(prepared.proposal.header.proposal_revision)
            && stored_source_ref == source.canonical_source_ref
            && u64::try_from(stored_source_revision).ok() == Some(source.record_revision)
            && stored_source_digest == source.canonical_source_digest
            && stored_principal == scope.principal.as_str()
            && stored_workspace == scope.workspace.as_str()
            && stored_scope_binding == scope_binding_ref
            && stored_entity == source.entity_name
            && stored_record == source.record_id
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if let Some((stored_proposal_digest, stored_payload_digest)) = transaction
        .query_row(
            "SELECT proposal_digest, payload_digest
               FROM app_memory_contribution_terminal
              WHERE installation_id=?1 AND proposal_id=?2 AND proposal_revision=?3",
            params![
                prepared.proposal.header.installation_id,
                prepared.proposal.header.proposal_id,
                i64::try_from(prepared.proposal.header.proposal_revision).map_err(|_| {
                    AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
                })?,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored_proposal_digest == prepared.proposal.proposal_digest
            && stored_payload_digest == prepared.payload_digest
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if let Some((stored, stored_payload_digest)) = transaction
        .query_row(
            "SELECT proposal_json, payload_digest FROM app_memory_contribution_outbox WHERE \
             event_id = ?1",
            [&prepared.event_id],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored == prepared.proposal_json && stored_payload_digest == prepared.payload_digest {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if let Some((
        head_event_id,
        head_proposal_id,
        head_revision,
        head_proposal_digest,
        head_payload_digest,
        head_principal,
        head_workspace,
        head_scope_binding,
        head_source_ref,
        head_source_revision,
        head_source_digest,
        head_source_entity,
        head_source_record,
        head_proposal_json,
    )) = transaction
        .query_row(
            "SELECT event_id, proposal_id, proposal_revision, proposal_digest,
                payload_digest, principal, workspace, scope_binding_ref,
                source_event_ref, source_event_revision, source_identity_digest,
                source_entity_name, source_record_id, proposal_json
           FROM app_memory_contribution_heads
          WHERE installation_id=?1 AND dedupe_key=?2",
            params![
                prepared.proposal.header.installation_id,
                prepared.proposal.header.dedupe_key
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Option<Vec<u8>>>(13)?,
                ))
            },
        )
        .optional()?
    {
        let incoming_revision =
            i64::try_from(prepared.proposal.header.proposal_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
            })?;
        if head_event_id == prepared.event_id
            || (head_proposal_id == prepared.proposal.header.proposal_id
                && head_revision == incoming_revision)
        {
            let source = prepared.proposal.header.sources.first().ok_or_else(|| {
                AppRegistryError::InvalidControlPlane("V1 proposal lost its source".to_owned())
            })?;
            if head_proposal_digest != prepared.proposal.proposal_digest
                || head_payload_digest != prepared.payload_digest
                || head_principal != scope.principal.as_str()
                || head_workspace != scope.workspace.as_str()
                || head_scope_binding != scope_binding_ref
                || head_source_ref != source.canonical_source_ref
                || u64::try_from(head_source_revision).ok() != Some(source.record_revision)
                || head_source_digest != source.canonical_source_digest
                || head_source_entity != source.entity_name
                || head_source_record != source.record_id
            {
                return Err(AppContributionError::SubstitutedReplay);
            }
            return match head_proposal_json {
                Some(bytes) if bytes == prepared.proposal_json => {
                    Ok(AppMemoryOutboxAppendOutcome::ExactReplay)
                },
                Some(_) => Err(AppContributionError::SubstitutedReplay),
                None => Err(AppContributionError::HistoryCompacted),
            };
        }
        if head_revision > incoming_revision {
            return Err(AppContributionError::HistoryCompacted);
        }
    }
    let prior_head = transaction
        .query_row(
            "SELECT event_id, proposal_id, proposal_revision, proposal_digest,
                    payload_digest, source_event_ref, source_event_revision,
                    source_identity_digest, lifecycle_state,
                    latest_invalidation_revision
               FROM app_memory_contribution_heads
              WHERE installation_id=?1 AND dedupe_key=?2",
            params![
                prepared.proposal.header.installation_id,
                prepared.proposal.header.dedupe_key,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            },
        )
        .optional()?;
    let source = prepared.proposal.header.sources.first().ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("V1 contribution has no exact source".to_owned())
    })?;
    if let Some((
        _,
        _,
        prior_revision,
        _,
        _,
        prior_source_ref,
        prior_source_revision,
        _,
        _,
        latest_invalidation_revision,
    )) = &prior_head
    {
        let proposal_revision =
            i64::try_from(prepared.proposal.header.proposal_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
            })?;
        let source_revision = i64::try_from(source.record_revision).map_err(|_| {
            AppRegistryError::InvalidControlPlane("source revision overflow".to_owned())
        })?;
        let source_high_water = latest_invalidation_revision
            .unwrap_or(*prior_source_revision)
            .max(*prior_source_revision);
        if proposal_revision <= *prior_revision
            || source_revision <= source_high_water
            || source.canonical_source_ref != *prior_source_ref
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "replace-exact-source-head did not advance the exact prior source".to_owned(),
            )
            .into());
        }
    }
    if let Some((stored, stored_payload_digest)) = transaction
        .query_row(
            "SELECT proposal_json, payload_digest FROM app_memory_contribution_outbox
          WHERE installation_id=?1 AND proposal_id=?2 AND proposal_revision=?3",
            params![
                prepared.proposal.header.installation_id,
                prepared.proposal.header.proposal_id,
                i64::try_from(prepared.proposal.header.proposal_revision).map_err(|_| {
                    AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
                })?,
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored == prepared.proposal_json && stored_payload_digest == prepared.payload_digest {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    let replacement = prior_head
        .as_ref()
        .filter(|head| head.8.as_str() != "settled")
        .map(
            |(
                prior_event_id,
                prior_proposal_id,
                _prior_revision,
                prior_proposal_digest,
                prior_payload_digest,
                _prior_source_ref,
                _prior_source_revision,
                prior_source_identity_digest,
                _lifecycle_state,
                _latest_invalidation_revision,
            )|
             -> Result<_, AppContributionError> {
                let reason = replacement_reason.ok_or_else(|| {
                    AppRegistryError::InvalidControlPlane(
                        "replace-exact-source-head lost its source transition reason".to_owned(),
                    )
                })?;
                let invalidation = AppMemoryInvalidationV1 {
                    contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
                    invalidation_id: deterministic_invalidation_id(
                        prior_proposal_digest,
                        &source.canonical_source_digest,
                        source.record_revision,
                    ),
                    installation_id: prepared.proposal.header.installation_id.clone(),
                    scope_binding_ref: prepared.proposal.header.scope_binding_ref.clone(),
                    proposal_id: prior_proposal_id.clone(),
                    proposal_digest: prior_proposal_digest.clone(),
                    source_event_ref: source.canonical_source_ref.clone(),
                    source_event_revision: source.record_revision,
                    source_identity_digest: prior_source_identity_digest.clone(),
                    dedupe_key: prepared.proposal.header.dedupe_key.clone(),
                    reason,
                    issued_at_ms: now.timestamp_millis(),
                    invalidation_digest: String::new(),
                }
                .seal()?;
                Ok((
                    invalidation,
                    prior_event_id.clone(),
                    prior_payload_digest.clone(),
                ))
            },
        )
        .transpose()?;
    let retained_invalidation_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_memory_invalidation_outbox
          WHERE installation_id=?1 AND dedupe_key=?2
            AND delivery_state IN ('pending','leased','dispatching')",
        params![
            prepared.proposal.header.installation_id,
            prepared.proposal.header.dedupe_key,
        ],
        |row| row.get(0),
    )?;
    if retained_invalidation_count != 0 {
        return Err(AppContributionError::Quota(
            "source head awaits durable invalidation acknowledgement",
        ));
    }
    let replacing_event_id = prior_head.as_ref().map(|head| head.0.as_str());
    let settlement_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_memory_contribution_outbox
          WHERE installation_id=?1 AND settlement_ref=?2
            AND (?3 IS NULL OR event_id <> ?3)
            AND proposal_expires_at > ?4
            AND delivery_state IN ('pending', 'leased', 'dispatching')",
        params![
            prepared.proposal.header.installation_id,
            settlement_ref,
            replacing_event_id,
            timestamp(now),
        ],
        |row| row.get(0),
    )?;
    if settlement_count >= MAX_CLAIM_BATCH as i64 {
        return Err(AppContributionError::Quota("per-settlement proposals"));
    }
    enforce_memory_contribution_quota(
        transaction,
        scope,
        &prepared.proposal.header.installation_id,
        prepared.proposal_json.len(),
        now,
        replacing_event_id,
    )?;
    if let Some((invalidation, prior_event_id, prior_payload_digest)) = replacement {
        insert_prevalidated_invalidation(
            transaction,
            &prepared.proposal.header.installation_id,
            invalidation,
            now,
        )?;
        terminalize_live_contribution(
            transaction,
            &prior_event_id,
            &prior_payload_digest,
            "superseded",
            now,
        )?;
    }
    transaction.execute(
        "INSERT INTO app_memory_contribution_outbox(
             event_id, proposal_id, proposal_revision, proposal_digest,
             payload_digest, source_event_ref, source_event_revision,
             source_identity_digest, principal, workspace, scope_binding_ref,
             installation_id, dedupe_key, settlement_ref,
             proposal_expires_at, proposal_json,
             delivery_state, available_at, attempt_count, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
         'pending', ?17, 0, ?17)",
        params![
            prepared.event_id,
            prepared.proposal.header.proposal_id,
            i64::try_from(prepared.proposal.header.proposal_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
            })?,
            prepared.proposal.proposal_digest,
            prepared.payload_digest,
            source.canonical_source_ref,
            i64::try_from(source.record_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("source revision overflow".to_owned())
            })?,
            source.canonical_source_digest,
            scope.principal.as_str(),
            scope.workspace.as_str(),
            scope_binding_ref,
            prepared.proposal.header.installation_id,
            prepared.proposal.header.dedupe_key,
            settlement_ref,
            DateTime::<Utc>::from_timestamp_millis(prepared.proposal.header.expires_at_ms)
                .ok_or_else(|| AppRegistryError::InvalidControlPlane(
                    "proposal expiry is outside the supported timestamp range".to_owned()
                ))?
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            prepared.proposal_json,
            timestamp(now),
        ],
    )?;
    for source in &prepared.proposal.header.sources {
        transaction.execute(
            "INSERT INTO app_memory_contribution_sources(
                 event_id, installation_id, entity_name, record_id,
                record_revision, source_identity_digest
                 , canonical_source_ref
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                prepared.event_id,
                source.installation_id,
                source.entity_name,
                source.record_id,
                i64::try_from(source.record_revision).map_err(|_| {
                    AppRegistryError::InvalidControlPlane("source revision overflow".to_owned())
                })?,
                source.canonical_source_digest,
                source.canonical_source_ref,
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO app_memory_contribution_heads(
             installation_id, dedupe_key, principal, workspace, scope_binding_ref,
             event_id, proposal_id,
             proposal_revision, proposal_digest, payload_digest,
             source_event_ref, source_event_revision, source_identity_digest,
             source_entity_name, source_record_id, proposal_expires_at,
             proposal_json, lifecycle_state, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
         'live', ?18)
         ON CONFLICT(installation_id, dedupe_key) DO UPDATE SET
             principal=excluded.principal,
             workspace=excluded.workspace,
             scope_binding_ref=excluded.scope_binding_ref,
             event_id=excluded.event_id,
             proposal_id=excluded.proposal_id,
             proposal_revision=excluded.proposal_revision,
             proposal_digest=excluded.proposal_digest,
             payload_digest=excluded.payload_digest,
             source_event_ref=excluded.source_event_ref,
             source_event_revision=excluded.source_event_revision,
             source_identity_digest=excluded.source_identity_digest,
             source_entity_name=excluded.source_entity_name,
             source_record_id=excluded.source_record_id,
             proposal_expires_at=excluded.proposal_expires_at,
             proposal_json=excluded.proposal_json,
             lifecycle_state='live',
             expiration_invalidation_event_id=NULL,
             updated_at=excluded.updated_at",
        params![
            prepared.proposal.header.installation_id,
            prepared.proposal.header.dedupe_key,
            scope.principal.as_str(),
            scope.workspace.as_str(),
            scope_binding_ref,
            prepared.event_id,
            prepared.proposal.header.proposal_id,
            i64::try_from(prepared.proposal.header.proposal_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
            })?,
            prepared.proposal.proposal_digest,
            prepared.payload_digest,
            source.canonical_source_ref,
            i64::try_from(source.record_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("source revision overflow".to_owned())
            })?,
            source.canonical_source_digest,
            source.entity_name,
            source.record_id,
            DateTime::<Utc>::from_timestamp_millis(prepared.proposal.header.expires_at_ms)
                .ok_or_else(|| AppRegistryError::InvalidControlPlane(
                    "proposal expiry is outside the supported timestamp range".to_owned()
                ))?
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            prepared.proposal_json,
            timestamp(now),
        ],
    )?;
    Ok(AppMemoryOutboxAppendOutcome::Appended)
}

pub(crate) fn append_memory_invalidation_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    installation_id: &str,
    invalidation: AppMemoryInvalidationV1,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    validate_v17_storage_accounting(transaction)?;
    let prepared = PreparedMemoryInvalidation::new(invalidation)?;
    if installation_id.is_empty()
        || installation_id.len() > 128
        || prepared.invalidation.installation_id != installation_id
        || prepared.invalidation.scope_binding_ref != scope_binding_ref
        || scope.principal.as_str().is_empty()
        || scope.workspace.as_str().is_empty()
        || prepared.invalidation.issued_at_ms > now.timestamp_millis()
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory invalidation installation identity is invalid".to_owned(),
        )
        .into());
    }
    if let Some((
        event_id,
        stored_digest,
        stored_payload_digest,
        stored_installation,
        stored_proposal_id,
        stored_proposal_digest,
        stored_dedupe,
        stored_source_ref,
        stored_source_revision,
        stored_source_digest,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
    )) = transaction
        .query_row(
            "SELECT event_id, invalidation_digest, payload_digest, installation_id,
                proposal_id, proposal_digest, dedupe_key, source_event_ref,
                source_event_revision, source_identity_digest,
                principal, workspace, scope_binding_ref
           FROM app_memory_invalidation_terminal
          WHERE event_id=?1 OR (installation_id=?2 AND invalidation_id=?3)",
            params![
                prepared.event_id,
                installation_id,
                prepared.invalidation.invalidation_id
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            },
        )
        .optional()?
    {
        if event_id == prepared.event_id
            && stored_digest == prepared.invalidation.invalidation_digest
            && stored_payload_digest == prepared.payload_digest
            && stored_installation == installation_id
            && stored_proposal_id == prepared.invalidation.proposal_id
            && stored_proposal_digest == prepared.invalidation.proposal_digest
            && stored_dedupe == prepared.invalidation.dedupe_key
            && stored_source_ref == prepared.invalidation.source_event_ref
            && u64::try_from(stored_source_revision).ok()
                == Some(prepared.invalidation.source_event_revision)
            && stored_source_digest == prepared.invalidation.source_identity_digest
            && stored_principal == scope.principal.as_str()
            && stored_workspace == scope.workspace.as_str()
            && stored_scope_binding == scope_binding_ref
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if let Some((stored, stored_payload_digest)) = transaction
        .query_row(
            "SELECT invalidation_json, payload_digest FROM app_memory_invalidation_outbox
          WHERE event_id=?1 OR (installation_id=?2 AND invalidation_id=?3)",
            params![
                prepared.event_id,
                installation_id,
                prepared.invalidation.invalidation_id
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored == prepared.invalidation_json && stored_payload_digest == prepared.payload_digest
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    let exact_source: Option<(String, i64, String, String, String)> = transaction
        .query_row(
            "SELECT dedupe_key, source_event_revision, principal, workspace, scope_binding_ref
           FROM app_memory_contribution_heads
          WHERE installation_id=?1 AND proposal_id=?2 AND proposal_digest=?3
            AND source_identity_digest=?4 AND source_event_ref=?5",
            params![
                installation_id,
                prepared.invalidation.proposal_id,
                prepared.invalidation.proposal_digest,
                prepared.invalidation.source_identity_digest,
                prepared.invalidation.source_event_ref,
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((
        exact_dedupe_key,
        prior_source_revision,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
    )) = exact_source
    else {
        let advanced = transaction
            .query_row(
                "SELECT principal, workspace, scope_binding_ref, source_event_ref,
                    source_event_revision
               FROM app_memory_contribution_heads
              WHERE installation_id=?1 AND dedupe_key=?2",
                params![installation_id, prepared.invalidation.dedupe_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        if advanced.is_some_and(|head| {
            head.0 == scope.principal.as_str()
                && head.1 == scope.workspace.as_str()
                && head.2 == scope_binding_ref
                && head.3 == prepared.invalidation.source_event_ref
                && u64::try_from(head.4)
                    .ok()
                    .is_some_and(|revision| revision >= prepared.invalidation.source_event_revision)
        }) {
            return Err(AppContributionError::HistoryCompacted);
        }
        return Err(AppRegistryError::InvalidControlPlane(
            "memory invalidation has no exact source high-water".to_owned(),
        )
        .into());
    };
    if exact_dedupe_key != prepared.invalidation.dedupe_key
        || stored_principal != scope.principal.as_str()
        || stored_workspace != scope.workspace.as_str()
        || stored_scope_binding != scope_binding_ref
        || u64::try_from(prior_source_revision)
            .ok()
            .is_none_or(|revision| {
                !invalidation_advances_exact_source(
                    prepared.invalidation.reason,
                    prepared.invalidation.source_event_revision,
                    revision,
                )
            })
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory invalidation is not bound to one exact journaled source".to_owned(),
        )
        .into());
    }
    insert_prepared_invalidation(transaction, installation_id, prepared, now)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_memory_source_invalidation_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    installation_id: &str,
    workflow_id: &str,
    action_id: &str,
    contribution_port_id: &str,
    entity_name: &str,
    record_id: &str,
    source_event_revision: u64,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<Option<AppMemoryOutboxAppendOutcome>, AppContributionError> {
    append_memory_record_invalidations_inner(
        transaction,
        scope,
        scope_binding_ref,
        installation_id,
        Some((workflow_id, action_id, contribution_port_id)),
        entity_name,
        record_id,
        source_event_revision,
        reason,
        now,
    )
}

/// Atomically invalidate every memory contribution whose sole retained source
/// is the exact record being physically forgotten. Unlike workflow terminal
/// publication, record forget is a source-owner transition and therefore must
/// cover every reviewed port that projected the record, including cascades.
pub fn append_memory_record_forget_invalidations_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    installation_id: &str,
    entity_name: &str,
    record_id: &str,
    source_event_revision: u64,
    now: &DateTime<Utc>,
) -> Result<Option<AppMemoryOutboxAppendOutcome>, AppContributionError> {
    append_memory_record_invalidations_inner(
        transaction,
        scope,
        scope_binding_ref,
        installation_id,
        None,
        entity_name,
        record_id,
        source_event_revision,
        AppMemoryInvalidationReasonV1::SourceForgotten,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn append_memory_record_invalidations_inner(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    installation_id: &str,
    expected_port: Option<(&str, &str, &str)>,
    entity_name: &str,
    record_id: &str,
    source_event_revision: u64,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<Option<AppMemoryOutboxAppendOutcome>, AppContributionError> {
    let (expected_workflow, expected_action, expected_port_id) = expected_port
        .map(|(workflow, action, port)| (Some(workflow), Some(action), Some(port)))
        .unwrap_or((None, None, None));
    let limit = APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE.saturating_add(1);
    let mut statement = transaction.prepare(
        "SELECT proposal_json FROM app_memory_contribution_heads
          WHERE installation_id=?1 AND principal=?2 AND workspace=?3
            AND scope_binding_ref=?4 AND source_entity_name=?5
            AND source_record_id=?6 AND proposal_json IS NOT NULL
            AND lifecycle_state IN ('live','invalidating')
            AND (?7 IS NULL OR (
                json_extract(CAST(proposal_json AS TEXT), '$.header.workflow_id')=?7
                AND json_extract(CAST(proposal_json AS TEXT), '$.header.action_id')=?8
                AND json_extract(CAST(proposal_json AS TEXT), '$.header.contribution_port_id')=?9
            ))
          ORDER BY dedupe_key LIMIT ?10",
    )?;
    let candidates = statement
        .query_map(
            params![
                installation_id,
                scope.principal.as_str(),
                scope.workspace.as_str(),
                scope_binding_ref,
                entity_name,
                record_id,
                expected_workflow,
                expected_action,
                expected_port_id,
                limit,
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if i64::try_from(candidates.len()).unwrap_or(i64::MAX)
        > APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE
    {
        return Err(AppContributionError::Quota(
            "source invalidation contribution heads",
        ));
    }
    let mut outcome = None;
    for proposal_json in candidates {
        let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&proposal_json)?;
        proposal.validate()?;
        if let Some((workflow_id, action_id, contribution_port_id)) = expected_port {
            if proposal.header.workflow_id != workflow_id
                || proposal.header.action_id != action_id
                || proposal.header.contribution_port_id != contribution_port_id
            {
                return Err(AppContributionError::SubstitutedReplay);
            }
        }
        let source = proposal
            .header
            .sources
            .first()
            .filter(|_| proposal.header.sources.len() == 1)
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "memory source invalidation lost its exact proposal source".to_owned(),
                )
            })?;
        if source.entity_name != entity_name
            || source.record_id != record_id
            || source_event_revision <= source.record_revision
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "memory source invalidation did not advance its exact record".to_owned(),
            )
            .into());
        }
        let invalidation = source_invalidation_for_memory_proposal(
            &proposal,
            scope_binding_ref,
            source_event_revision,
            reason,
            now,
        )?;
        outcome = Some(append_memory_invalidation_in_transaction(
            transaction,
            scope,
            scope_binding_ref,
            installation_id,
            invalidation,
            now,
        )?);
    }
    Ok(outcome)
}

/// Atomically invalidate every retained contribution source for one
/// installation when its grant or lifecycle authority stops being usable.
/// Control-plane invalidations deliberately retain the entity revision: they
/// must not consume the next real record revision.
pub fn append_memory_installation_invalidations_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    installation_id: &str,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<usize, AppContributionError> {
    let limit = APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE.saturating_add(1);
    let mut statement = transaction.prepare(
        "SELECT scope_binding_ref, proposal_json
           FROM app_memory_contribution_heads
          WHERE installation_id=?1 AND principal=?2 AND workspace=?3
            AND proposal_json IS NOT NULL AND lifecycle_state='live'
          ORDER BY dedupe_key LIMIT ?4",
    )?;
    let candidates = statement
        .query_map(
            params![
                installation_id,
                scope.principal.as_str(),
                scope.workspace.as_str(),
                limit,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if i64::try_from(candidates.len()).unwrap_or(i64::MAX)
        > APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE
    {
        return Err(AppContributionError::Quota(
            "installation memory invalidation source heads",
        ));
    }
    let mut appended = 0usize;
    for (scope_binding_ref, proposal_json) in candidates {
        let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&proposal_json)?;
        proposal.validate()?;
        if proposal.header.installation_id != installation_id
            || proposal.header.scope_binding_ref != scope_binding_ref
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
        let source = proposal
            .header
            .sources
            .first()
            .filter(|_| proposal.header.sources.len() == 1)
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "installation memory invalidation lost its exact source".to_owned(),
                )
            })?;
        let invalidation = source_invalidation_for_memory_proposal(
            &proposal,
            &scope_binding_ref,
            source.record_revision,
            reason,
            now,
        )?;
        append_memory_invalidation_in_transaction(
            transaction,
            scope,
            &scope_binding_ref,
            installation_id,
            invalidation,
            now,
        )?;
        appended = appended.saturating_add(1);
    }
    Ok(appended)
}

fn source_invalidation_for_memory_proposal(
    proposal: &AppMemoryCandidateProposalV1,
    scope_binding_ref: &str,
    source_event_revision: u64,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<AppMemoryInvalidationV1, AppContributionError> {
    let source = proposal
        .header
        .sources
        .first()
        .filter(|_| proposal.header.sources.len() == 1)
        .ok_or_else(|| {
            AppRegistryError::InvalidControlPlane(
                "memory invalidation lost its exact proposal source".to_owned(),
            )
        })?;
    let identity = serde_json::to_vec(&serde_json::json!({
        "schema": "magician.app-source-invalidation-id.v1",
        "destination": "memory",
        "proposal_digest": &proposal.proposal_digest,
        "source_event_revision": source_event_revision,
        "reason": reason,
    }))?;
    Ok(AppMemoryInvalidationV1 {
        contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
        invalidation_id: format!(
            "memory-invalidation:{}",
            content_digest(&identity).trim_start_matches("blake3:")
        ),
        installation_id: proposal.header.installation_id.clone(),
        scope_binding_ref: scope_binding_ref.to_owned(),
        proposal_id: proposal.header.proposal_id.clone(),
        proposal_digest: proposal.proposal_digest.clone(),
        source_event_ref: source.canonical_source_ref.clone(),
        source_event_revision,
        source_identity_digest: source.canonical_source_digest.clone(),
        dedupe_key: proposal.header.dedupe_key.clone(),
        reason,
        issued_at_ms: now.timestamp_millis(),
        invalidation_digest: String::new(),
    }
    .seal()?)
}

fn deterministic_invalidation_id(
    proposal_digest: &str,
    source_digest: &str,
    source_revision: u64,
) -> String {
    let digest = magician_app_contract::contribution::content_digest(
        format!("replace\0{proposal_digest}\0{source_digest}\0{source_revision}").as_bytes(),
    );
    format!(
        "memory-invalidation:{}",
        digest.trim_start_matches("blake3:")
    )
}

fn insert_prevalidated_invalidation(
    transaction: &Transaction<'_>,
    installation_id: &str,
    invalidation: AppMemoryInvalidationV1,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    insert_prepared_invalidation(
        transaction,
        installation_id,
        PreparedMemoryInvalidation::new(invalidation)?,
        now,
    )
}

fn insert_prepared_invalidation(
    transaction: &Transaction<'_>,
    installation_id: &str,
    prepared: PreparedMemoryInvalidation,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    let source_scope = transaction
        .query_row(
            "SELECT principal, workspace, scope_binding_ref,
                    latest_invalidation_id, latest_invalidation_digest,
                    latest_invalidation_revision,
                    superseded_invalidation_id, superseded_invalidation_digest,
                    superseded_invalidation_revision,
                    source_entity_name, source_record_id, proposal_json
               FROM app_memory_contribution_heads
              WHERE installation_id=?1 AND dedupe_key=?2
                AND proposal_id=?3 AND proposal_digest=?4
                AND source_identity_digest=?5",
            params![
                installation_id,
                prepared.invalidation.dedupe_key,
                prepared.invalidation.proposal_id,
                prepared.invalidation.proposal_digest,
                prepared.invalidation.source_identity_digest,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<Vec<u8>>>(11)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            AppRegistryError::InvalidControlPlane(
                "memory invalidation lost its exact source-head binding".to_owned(),
            )
        })?;
    if prepared.invalidation.installation_id != installation_id
        || prepared.invalidation.scope_binding_ref != source_scope.2
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory invalidation substituted installation or scope binding".to_owned(),
        )
        .into());
    }
    let source_proposal_json = match source_scope.11.clone() {
        Some(bytes) => bytes,
        None => transaction
            .query_row(
                "SELECT source_proposal_json FROM app_memory_invalidation_outbox
              WHERE installation_id=?1 AND dedupe_key=?2 AND proposal_id=?3
                AND proposal_digest=?4 AND source_identity_digest=?5
                AND delivery_state IN ('pending','leased','dispatching')
              ORDER BY sequence ASC LIMIT 1",
                params![
                    installation_id,
                    prepared.invalidation.dedupe_key,
                    prepared.invalidation.proposal_id,
                    prepared.invalidation.proposal_digest,
                    prepared.invalidation.source_identity_digest,
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "settled source high-water has no retained invalidation evidence".to_owned(),
                )
            })?,
    };
    validate_retained_invalidation_source(
        &prepared,
        installation_id,
        &source_scope.9,
        &source_scope.10,
        &source_proposal_json,
    )?;
    if let Some((
        stored_digest,
        stored_payload_digest,
        stored_installation,
        stored_proposal_id,
        stored_proposal_digest,
        stored_dedupe,
        stored_source_ref,
        stored_source_revision,
        stored_source_digest,
        stored_scope_binding,
    )) = transaction
        .query_row(
            "SELECT invalidation_digest, payload_digest, installation_id,
                    proposal_id, proposal_digest, dedupe_key, source_event_ref,
                    source_event_revision, source_identity_digest, scope_binding_ref
               FROM app_memory_invalidation_terminal
              WHERE event_id=?1 OR (installation_id=?2 AND invalidation_id=?3)",
            params![
                prepared.event_id,
                installation_id,
                prepared.invalidation.invalidation_id,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?
    {
        if stored_digest == prepared.invalidation.invalidation_digest
            && stored_payload_digest == prepared.payload_digest
            && stored_installation == prepared.invalidation.installation_id
            && stored_proposal_id == prepared.invalidation.proposal_id
            && stored_proposal_digest == prepared.invalidation.proposal_digest
            && stored_dedupe == prepared.invalidation.dedupe_key
            && stored_source_ref == prepared.invalidation.source_event_ref
            && u64::try_from(stored_source_revision).ok()
                == Some(prepared.invalidation.source_event_revision)
            && stored_source_digest == prepared.invalidation.source_identity_digest
            && stored_scope_binding == prepared.invalidation.scope_binding_ref
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if let Some((stored, stored_payload_digest)) = transaction
        .query_row(
            "SELECT invalidation_json, payload_digest
               FROM app_memory_invalidation_outbox
              WHERE event_id=?1 OR (installation_id=?2 AND invalidation_id=?3)",
            params![
                prepared.event_id,
                installation_id,
                prepared.invalidation.invalidation_id,
            ],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored == prepared.invalidation_json && stored_payload_digest == prepared.payload_digest
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if source_scope.3.as_deref() == Some(prepared.invalidation.invalidation_id.as_str()) {
        return if source_scope.4.as_deref()
            == Some(prepared.invalidation.invalidation_digest.as_str())
            && source_scope.5 == i64::try_from(prepared.invalidation.source_event_revision).ok()
        {
            Err(AppContributionError::HistoryCompacted)
        } else {
            Err(AppContributionError::SubstitutedReplay)
        };
    }
    if source_scope.5.is_some_and(|revision| {
        i64::try_from(prepared.invalidation.source_event_revision)
            .ok()
            .is_none_or(|incoming| incoming <= revision)
    }) {
        return Err(AppContributionError::HistoryCompacted);
    }
    if source_scope.6.as_deref() == Some(prepared.invalidation.invalidation_id.as_str()) {
        return if source_scope.7.as_deref()
            == Some(prepared.invalidation.invalidation_digest.as_str())
            && source_scope.8 == i64::try_from(prepared.invalidation.source_event_revision).ok()
        {
            Err(AppContributionError::HistoryCompacted)
        } else {
            Err(AppContributionError::SubstitutedReplay)
        };
    }
    if source_scope.8.is_some_and(|revision| {
        i64::try_from(prepared.invalidation.source_event_revision)
            .ok()
            .is_none_or(|incoming| incoming <= revision)
    }) {
        return Err(AppContributionError::HistoryCompacted);
    }
    let retained = {
        let mut statement = transaction.prepare(
            "SELECT sequence, event_id, invalidation_id, invalidation_digest,
                    source_event_revision, delivery_state
               FROM app_memory_invalidation_outbox
              WHERE installation_id=?1 AND dedupe_key=?2 AND proposal_id=?3
                AND proposal_digest=?4 AND source_identity_digest=?5
                AND delivery_state IN ('pending','leased','dispatching')
              ORDER BY sequence ASC LIMIT 3",
        )?;
        let mapped = statement.query_map(
            params![
                installation_id,
                prepared.invalidation.dedupe_key,
                prepared.invalidation.proposal_id,
                prepared.invalidation.proposal_digest,
                prepared.invalidation.source_identity_digest,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    if retained.len() > 2
        || retained.iter().filter(|row| row.5 == "pending").count() > 1
        || retained.iter().filter(|row| row.5 != "pending").count() > 1
        || retained
            .iter()
            .find(|row| row.5 == "pending")
            .is_some_and(|pending| {
                retained
                    .iter()
                    .find(|row| row.5 != "pending")
                    .is_some_and(|active| pending.4 <= active.4)
            })
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory invalidation coalescing exceeded its bounded source lanes".to_owned(),
        )
        .into());
    }
    let incoming_revision =
        i64::try_from(prepared.invalidation.source_event_revision).map_err(|_| {
            AppRegistryError::InvalidControlPlane("source event revision overflow".to_owned())
        })?;
    if let Some(maximum) = retained.iter().max_by_key(|row| row.4) {
        if incoming_revision < maximum.4 {
            return Err(AppContributionError::HistoryCompacted);
        }
        if incoming_revision == maximum.4 {
            return if maximum.2 == prepared.invalidation.invalidation_id
                && maximum.3 == prepared.invalidation.invalidation_digest
            {
                Ok(AppMemoryOutboxAppendOutcome::Coalesced)
            } else {
                Err(AppContributionError::SubstitutedReplay)
            };
        }
    }
    if let Some(pending) = retained.iter().find(|row| row.5 == "pending") {
        if transaction.execute(
            "UPDATE app_memory_contribution_heads SET
                 superseded_invalidation_id=CASE
                   WHEN COALESCE(superseded_invalidation_revision,0) < ?1 THEN ?2
                   ELSE superseded_invalidation_id END,
                 superseded_invalidation_digest=CASE
                   WHEN COALESCE(superseded_invalidation_revision,0) < ?1 THEN ?3
                   ELSE superseded_invalidation_digest END,
                 \
             superseded_invalidation_revision=MAX(COALESCE(superseded_invalidation_revision,0),?\
             1),
                 updated_at=?4
              WHERE installation_id=?5 AND dedupe_key=?6",
            params![
                pending.4,
                pending.2,
                pending.3,
                timestamp(now),
                installation_id,
                prepared.invalidation.dedupe_key
            ],
        )? != 1
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
        if transaction.execute(
            "UPDATE app_memory_invalidation_outbox SET
                 event_id=?1, invalidation_id=?2, invalidation_digest=?3,
                 payload_digest=?4, source_event_ref=?5, source_event_revision=?6,
                 invalidation_json=?7, available_at=?8, created_at=?8
              WHERE sequence=?9 AND event_id=?10 AND delivery_state='pending'",
            params![
                prepared.event_id,
                prepared.invalidation.invalidation_id,
                prepared.invalidation.invalidation_digest,
                prepared.payload_digest,
                prepared.invalidation.source_event_ref,
                incoming_revision,
                prepared.invalidation_json,
                timestamp(now),
                pending.0,
                pending.1,
            ],
        )? != 1
        {
            return Err(AppRegistryError::OutboxLeaseStale.into());
        }
        return Ok(AppMemoryOutboxAppendOutcome::Coalesced);
    }
    transaction.execute(
        "INSERT INTO app_memory_invalidation_outbox(
             event_id, invalidation_id, invalidation_digest, payload_digest,
             proposal_id, proposal_digest, installation_id, dedupe_key,
             source_event_ref, source_event_revision, source_identity_digest,
             principal, workspace, scope_binding_ref,
             source_entity_name, source_record_id, source_proposal_json,
             invalidation_json, delivery_state, available_at, attempt_count, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
         ?18, 'pending', ?19, 0, ?19)",
        params![
            prepared.event_id,
            prepared.invalidation.invalidation_id,
            prepared.invalidation.invalidation_digest,
            prepared.payload_digest,
            prepared.invalidation.proposal_id,
            prepared.invalidation.proposal_digest,
            installation_id,
            prepared.invalidation.dedupe_key,
            prepared.invalidation.source_event_ref,
            i64::try_from(prepared.invalidation.source_event_revision).map_err(|_| {
                AppRegistryError::InvalidControlPlane("source event revision overflow".to_owned())
            })?,
            prepared.invalidation.source_identity_digest,
            source_scope.0,
            source_scope.1,
            source_scope.2,
            source_scope.9,
            source_scope.10,
            source_proposal_json,
            prepared.invalidation_json,
            timestamp(now),
        ],
    )?;
    Ok(AppMemoryOutboxAppendOutcome::Appended)
}

fn terminalize_live_contribution(
    transaction: &Transaction<'_>,
    event_id: &str,
    expected_payload_digest: &str,
    terminal_state: &'static str,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let row = transaction
        .query_row(
            "SELECT installation_id, dedupe_key, proposal_id, proposal_revision,
                    proposal_digest, payload_digest, source_event_ref,
                    source_event_revision, source_identity_digest, delivery_state,
                    ingress_receipt_json, ingress_receipt_digest,
                    principal, workspace, scope_binding_ref,
                    (SELECT entity_name FROM app_memory_contribution_sources
                      WHERE event_id=app_memory_contribution_outbox.event_id),
                    (SELECT record_id FROM app_memory_contribution_sources
                      WHERE event_id=app_memory_contribution_outbox.event_id)
               FROM app_memory_contribution_outbox WHERE event_id=?1",
            [event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<Vec<u8>>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                ))
            },
        )
        .optional()?;
    let Some((
        installation_id,
        dedupe_key,
        proposal_id,
        proposal_revision,
        proposal_digest,
        payload_digest,
        source_event_ref,
        source_event_revision,
        source_identity_digest,
        delivery_state,
        receipt_json,
        receipt_digest,
        principal,
        workspace,
        scope_binding_ref,
        source_entity_name,
        source_record_id,
    )) = row
    else {
        let exact_terminal: Option<String> = transaction
            .query_row(
                "SELECT payload_digest FROM app_memory_contribution_terminal WHERE event_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .optional()?;
        return match exact_terminal {
            Some(digest) if digest == expected_payload_digest => Ok(()),
            Some(_) => Err(AppContributionError::SubstitutedReplay),
            None => Err(AppRegistryError::OutboxLeaseStale.into()),
        };
    };
    if payload_digest != expected_payload_digest {
        return Err(AppContributionError::SubstitutedReplay);
    }
    let (destination_generation, destination_receipt_digest, source_ack_receipt_digest) =
        match (delivery_state.as_str(), receipt_json, receipt_digest) {
            ("delivered", Some(bytes), Some(stored_digest)) => {
                let receipt: AppMemoryIngressReceiptV1 = serde_json::from_slice(&bytes)?;
                receipt.validate()?;
                if receipt.receipt_digest != stored_digest {
                    return Err(AppContributionError::SubstitutedReplay);
                }
                (
                    Some(i64::try_from(receipt.destination_generation).map_err(|_| {
                        AppRegistryError::InvalidControlPlane(
                            "destination generation overflow".to_owned(),
                        )
                    })?),
                    Some(receipt.destination_receipt_digest),
                    Some(stored_digest),
                )
            },
            (_, None, None) => (None, None, None),
            _ => return Err(AppContributionError::SubstitutedReplay),
        };
    let inserted = transaction.execute(
        "INSERT INTO app_memory_contribution_terminal(
             event_id, installation_id, dedupe_key, proposal_id, proposal_revision,
             proposal_digest, payload_digest, source_event_ref, source_event_revision,
             source_identity_digest, principal, workspace, scope_binding_ref,
             source_entity_name, source_record_id, terminal_state, destination_generation,
             destination_receipt_digest, source_ack_receipt_digest, terminalized_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
         ?18, ?19, ?20)
         ON CONFLICT(event_id) DO NOTHING",
        params![
            event_id,
            installation_id,
            dedupe_key,
            proposal_id,
            proposal_revision,
            proposal_digest,
            payload_digest,
            source_event_ref,
            source_event_revision,
            source_identity_digest,
            principal,
            workspace,
            scope_binding_ref,
            source_entity_name,
            source_record_id,
            terminal_state,
            destination_generation,
            destination_receipt_digest,
            source_ack_receipt_digest,
            timestamp(now),
        ],
    )?;
    if inserted == 0 {
        let existing = transaction.query_row(
            "SELECT terminal_state, payload_digest, source_ack_receipt_digest,
                    destination_generation, destination_receipt_digest
               FROM app_memory_contribution_terminal WHERE event_id=?1",
            [event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )?;
        if existing
            != (
                terminal_state.to_owned(),
                payload_digest.clone(),
                source_ack_receipt_digest.clone(),
                destination_generation,
                destination_receipt_digest.clone(),
            )
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
    }
    transaction.execute(
        "DELETE FROM app_memory_contribution_sources WHERE event_id=?1",
        [event_id],
    )?;
    transaction.execute(
        "DELETE FROM app_memory_contribution_outbox WHERE event_id=?1",
        [event_id],
    )?;
    Ok(())
}

fn terminalize_live_invalidation(
    transaction: &Transaction<'_>,
    event_id: &str,
    expected_payload_digest: &str,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let row = transaction
        .query_row(
            "SELECT installation_id, invalidation_id, invalidation_digest,
                payload_digest, invalidation_receipt_digest,
                destination_generation, destination_receipt_digest,
                proposal_id, proposal_digest, dedupe_key, source_event_ref,
                source_event_revision, source_identity_digest,
                principal, workspace, scope_binding_ref,
                source_entity_name, source_record_id
           FROM app_memory_invalidation_outbox
          WHERE event_id=?1 AND delivery_state='delivered'",
            [event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                ))
            },
        )
        .optional()?;
    let Some((
        installation_id,
        invalidation_id,
        invalidation_digest,
        payload_digest,
        source_ack_receipt_digest,
        destination_generation,
        destination_receipt_digest,
        proposal_id,
        proposal_digest,
        dedupe_key,
        source_event_ref,
        source_event_revision,
        source_identity_digest,
        principal,
        workspace,
        scope_binding_ref,
        source_entity_name,
        source_record_id,
    )) = row
    else {
        let exact: Option<String> = transaction
            .query_row(
                "SELECT payload_digest FROM app_memory_invalidation_terminal WHERE event_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .optional()?;
        return match exact {
            Some(digest) if digest == expected_payload_digest => Ok(()),
            Some(_) => Err(AppContributionError::SubstitutedReplay),
            None => Err(AppRegistryError::OutboxLeaseStale.into()),
        };
    };
    if payload_digest != expected_payload_digest {
        return Err(AppContributionError::SubstitutedReplay);
    }
    let inserted = transaction.execute(
        "INSERT INTO app_memory_invalidation_terminal(
             event_id, installation_id, invalidation_id, invalidation_digest,
             payload_digest, proposal_id, proposal_digest, dedupe_key,
             source_event_ref, source_event_revision, source_identity_digest,
             principal, workspace, scope_binding_ref, source_entity_name,
             source_record_id, source_ack_receipt_digest, destination_generation,
             destination_receipt_digest, terminalized_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
         ?18, ?19, ?20)
         ON CONFLICT(event_id) DO NOTHING",
        params![
            event_id,
            installation_id,
            invalidation_id,
            invalidation_digest,
            payload_digest,
            proposal_id,
            proposal_digest,
            dedupe_key,
            source_event_ref,
            source_event_revision,
            source_identity_digest,
            principal,
            workspace,
            scope_binding_ref,
            source_entity_name,
            source_record_id,
            source_ack_receipt_digest,
            destination_generation,
            destination_receipt_digest,
            timestamp(now)
        ],
    )?;
    if inserted == 0 {
        let existing = transaction.query_row(
            "SELECT invalidation_digest, payload_digest, source_ack_receipt_digest,
                    destination_generation, destination_receipt_digest
               FROM app_memory_invalidation_terminal WHERE event_id=?1",
            [event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?;
        if existing
            != (
                invalidation_digest.clone(),
                payload_digest.clone(),
                source_ack_receipt_digest.clone(),
                destination_generation,
                destination_receipt_digest.clone(),
            )
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
    }
    transaction.execute(
        "DELETE FROM app_memory_invalidation_outbox WHERE event_id=?1",
        [event_id],
    )?;
    transaction.execute(
        "UPDATE app_memory_contribution_heads
            SET latest_invalidation_id=CASE
                  WHEN COALESCE(latest_invalidation_revision,0) < ?3 THEN ?1
                  ELSE latest_invalidation_id END,
                latest_invalidation_digest=CASE
                  WHEN COALESCE(latest_invalidation_revision,0) < ?3 THEN ?2
                  ELSE latest_invalidation_digest END,
                latest_invalidation_revision=MAX(COALESCE(latest_invalidation_revision,0),?3),
                updated_at=?4
          WHERE installation_id=?5 AND dedupe_key=?6 AND proposal_id=?7
            AND proposal_digest=?8 AND source_identity_digest=?9",
        params![
            invalidation_id,
            invalidation_digest,
            source_event_revision,
            timestamp(now),
            installation_id,
            dedupe_key,
            proposal_id,
            proposal_digest,
            source_identity_digest,
        ],
    )?;
    let invalidated_head: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT event_id, payload_digest, lifecycle_state FROM app_memory_contribution_heads
          WHERE installation_id=?1 AND dedupe_key=?2 AND proposal_id=?3
            AND proposal_digest=?4 AND source_identity_digest=?5",
            params![
                installation_id,
                dedupe_key,
                proposal_id,
                proposal_digest,
                source_identity_digest
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((proposal_event_id, proposal_payload_digest, lifecycle_state)) = invalidated_head {
        let live_row: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM app_memory_contribution_outbox WHERE event_id=?1",
                [&proposal_event_id],
                |row| row.get(0),
            )
            .optional()?;
        if live_row.is_some() {
            terminalize_live_contribution(
                transaction,
                &proposal_event_id,
                &proposal_payload_digest,
                "invalidated",
                now,
            )?;
        }
        if lifecycle_state != "settled" {
            if transaction.execute(
                "UPDATE app_memory_contribution_heads
                    SET lifecycle_state='settled', proposal_json=NULL,
                        expiration_invalidation_event_id=NULL, updated_at=?1
                  WHERE installation_id=?2 AND dedupe_key=?3
                    AND event_id=?4 AND lifecycle_state IN ('live','invalidating')",
                params![
                    timestamp(now),
                    installation_id,
                    dedupe_key,
                    proposal_event_id
                ],
            )? != 1
            {
                return Err(AppContributionError::SubstitutedReplay);
            }
        }
    }
    Ok(())
}

fn terminalize_expired_and_delivered(
    transaction: &Transaction<'_>,
    now: &DateTime<Utc>,
    limit: usize,
) -> Result<(), AppContributionError> {
    enqueue_expired_head_invalidations(transaction, now, limit)?;
    let contribution_rows = {
        let mut statement = transaction.prepare(
            "SELECT event_id, payload_digest, delivery_state
              FROM app_memory_contribution_outbox
              WHERE delivery_state='delivered'
              ORDER BY sequence ASC LIMIT ?1",
        )?;
        let mapped = statement.query_map([limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (event_id, payload_digest, _state) in contribution_rows {
        terminalize_live_contribution(transaction, &event_id, &payload_digest, "delivered", now)?;
    }
    let invalidation_rows = {
        let mut statement = transaction.prepare(
            "SELECT event_id, payload_digest FROM app_memory_invalidation_outbox
              WHERE delivery_state='delivered' ORDER BY sequence ASC LIMIT ?1",
        )?;
        let mapped = statement.query_map([limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (event_id, payload_digest) in invalidation_rows {
        terminalize_live_invalidation(transaction, &event_id, &payload_digest, now)?;
    }
    prune_recent_terminal_windows(transaction)?;
    Ok(())
}

fn enqueue_expired_head_invalidations(
    transaction: &Transaction<'_>,
    now: &DateTime<Utc>,
    limit: usize,
) -> Result<(), AppContributionError> {
    let heads = {
        let mut statement = transaction.prepare(
            "SELECT installation_id, dedupe_key, proposal_id, proposal_digest,
                    source_event_ref, source_event_revision, source_identity_digest,
                    scope_binding_ref, proposal_expires_at, proposal_json
               FROM app_memory_contribution_heads
              WHERE lifecycle_state='live' AND proposal_expires_at <= ?1
                AND proposal_json IS NOT NULL
              ORDER BY proposal_expires_at ASC, installation_id ASC, dedupe_key ASC
              LIMIT ?2",
        )?;
        let mapped = statement.query_map(params![timestamp(now), limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Vec<u8>>(9)?,
            ))
        })?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (
        installation_id,
        dedupe_key,
        proposal_id,
        proposal_digest,
        source_ref,
        source_revision,
        source_digest,
        scope_binding_ref,
        expires_at,
        proposal_json,
    ) in heads
    {
        let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&proposal_json)?;
        let prepared_proposal = PreparedMemoryContribution::new(proposal)?;
        let source = prepared_proposal
            .proposal
            .header
            .sources
            .first()
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "expired source head lost its source".to_owned(),
                )
            })?;
        if prepared_proposal.proposal.header.installation_id != installation_id
            || prepared_proposal.proposal.header.dedupe_key != dedupe_key
            || prepared_proposal.proposal.header.proposal_id != proposal_id
            || prepared_proposal.proposal.proposal_digest != proposal_digest
            || prepared_proposal.proposal.header.scope_binding_ref != scope_binding_ref
            || source.canonical_source_ref != source_ref
            || i64::try_from(source.record_revision).ok() != Some(source_revision)
            || source.canonical_source_digest != source_digest
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
        let source_event_revision = u64::try_from(source_revision)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane("expired source revision overflow".to_owned())
            })?;
        let issued_at_ms = DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|_| {
                AppRegistryError::InvalidControlPlane(
                    "expired source-head timestamp is invalid".to_owned(),
                )
            })?
            .timestamp_millis();
        let invalidation = AppMemoryInvalidationV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            invalidation_id: deterministic_expiration_invalidation_id(
                &proposal_digest,
                issued_at_ms,
            ),
            installation_id: installation_id.clone(),
            scope_binding_ref,
            proposal_id,
            proposal_digest: proposal_digest.clone(),
            source_event_ref: source_ref,
            source_event_revision,
            source_identity_digest: source_digest,
            dedupe_key: dedupe_key.clone(),
            reason: AppMemoryInvalidationReasonV1::ContributionExpired,
            issued_at_ms,
            invalidation_digest: String::new(),
        }
        .seal()?;
        let prepared = PreparedMemoryInvalidation::new(invalidation)?;
        let expiration_event_id = prepared.event_id.clone();
        let outcome = insert_prepared_invalidation(transaction, &installation_id, prepared, now)?;
        let retained_event_id = if outcome == AppMemoryOutboxAppendOutcome::Coalesced {
            transaction.query_row(
                "SELECT event_id FROM app_memory_invalidation_outbox
                  WHERE installation_id=?1 AND dedupe_key=?2 AND proposal_digest=?3
                    AND delivery_state IN ('pending','leased','dispatching')
                  ORDER BY sequence ASC LIMIT 1",
                params![installation_id, dedupe_key, proposal_digest],
                |row| row.get::<_, String>(0),
            )?
        } else {
            expiration_event_id
        };
        if transaction.execute(
            "UPDATE app_memory_contribution_heads
                SET lifecycle_state='invalidating', expiration_invalidation_event_id=?1,
                    updated_at=?2
              WHERE installation_id=?3 AND dedupe_key=?4 AND lifecycle_state='live'
                AND proposal_digest=?5",
            params![
                retained_event_id,
                timestamp(now),
                installation_id,
                dedupe_key,
                proposal_digest
            ],
        )? != 1
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
    }
    Ok(())
}

fn deterministic_expiration_invalidation_id(proposal_digest: &str, expires_at_ms: i64) -> String {
    let digest = magician_app_contract::contribution::content_digest(
        format!("expired\0{proposal_digest}\0{expires_at_ms}").as_bytes(),
    );
    format!(
        "memory-invalidation:{}",
        digest.trim_start_matches("blake3:")
    )
}

fn prune_recent_terminal_windows(
    transaction: &Transaction<'_>,
) -> Result<(), AppContributionError> {
    use super::contribution::APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND;
    transaction.execute(
        "DELETE FROM app_memory_contribution_terminal WHERE event_id IN (
             SELECT event_id FROM app_memory_contribution_terminal
              ORDER BY terminalized_at DESC, event_id DESC LIMIT -1 OFFSET ?1
         )",
        [APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND],
    )?;
    transaction.execute(
        "DELETE FROM app_memory_invalidation_terminal WHERE event_id IN (
             SELECT event_id FROM app_memory_invalidation_terminal
              ORDER BY terminalized_at DESC, event_id DESC LIMIT -1 OFFSET ?1
         )",
        [APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND],
    )?;
    validate_v17_storage_accounting(transaction)?;
    Ok(())
}

fn validate_v17_storage_accounting(
    transaction: &Transaction<'_>,
) -> Result<(), AppContributionError> {
    use super::contribution::{
        APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE, APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND,
        APP_MEMORY_OUTBOX_MAX_ROWS_PER_SCOPE, APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE,
    };
    let bounded = [
        (
            "app_memory_contribution_outbox",
            APP_MEMORY_OUTBOX_MAX_ROWS_PER_SCOPE,
            APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE,
            "proposal_json",
        ),
        (
            "app_memory_contribution_sources",
            APP_MEMORY_OUTBOX_MAX_ROWS_PER_SCOPE,
            APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE,
            "entity_name || record_id || canonical_source_ref",
        ),
        (
            "app_memory_invalidation_outbox",
            MAX_INVALIDATION_ROWS_PER_SCOPE,
            MAX_INVALIDATION_BYTES_PER_SCOPE,
            "invalidation_json || source_proposal_json",
        ),
        (
            "app_memory_contribution_heads",
            APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE,
            APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE,
            "COALESCE(proposal_json, X'')",
        ),
        (
            "app_memory_contribution_terminal",
            APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND,
            APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE,
            "event_id || proposal_digest || payload_digest",
        ),
        (
            "app_memory_invalidation_terminal",
            APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND,
            APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE,
            "event_id || invalidation_digest || payload_digest",
        ),
        (
            "app_memory_destination_ack_high_water",
            1,
            256,
            "destination_receipt_digest || updated_at",
        ),
    ];
    for (table, max_rows, max_bytes, bytes_expr) in bounded {
        let sql = format!("SELECT COUNT(*), COALESCE(SUM(length({bytes_expr})), 0) FROM {table}");
        let (rows, bytes): (i64, i64) =
            transaction.query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if rows > max_rows || bytes > max_bytes {
            return Err(AppRegistryError::InvalidControlPlane(format!(
                "bounded V17 journal accounting failed for {table}"
            ))
            .into());
        }
    }
    Ok(())
}

fn expected_destination_head(
    transaction: &Transaction<'_>,
) -> Result<Option<AppMemoryExpectedDestinationHead>, AppContributionError> {
    let head = transaction
        .query_row(
            "SELECT destination_generation, destination_receipt_digest
           FROM app_memory_destination_ack_high_water WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((generation, receipt_digest)) = head else {
        return Ok(None);
    };
    if generation <= 0 {
        return Err(AppRegistryError::InvalidControlPlane(
            "source acknowledgement destination high-water is inconsistent".to_owned(),
        )
        .into());
    }
    Ok(Some(AppMemoryExpectedDestinationHead {
        generation: u64::try_from(generation).map_err(|_| {
            AppRegistryError::InvalidControlPlane("destination generation is negative".to_owned())
        })?,
        receipt_digest,
    }))
}

/// Bounded recovery seam used by a future source-settlement transaction owner.
/// It never reconstructs candidate content from mutable records: only the
/// exact bytes atomically journaled beside the named settlement are returned.
#[allow(dead_code)] // Retained compatibility seam for historical settlement recovery.
pub(crate) fn memory_contributions_for_settlement_in_transaction(
    transaction: &Transaction<'_>,
    installation_id: &str,
    settlement_ref: &str,
) -> Result<Vec<AppMemoryCandidateProposalV1>, AppContributionError> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_memory_contribution_outbox
          WHERE installation_id=?1 AND settlement_ref=?2",
        params![installation_id, settlement_ref],
        |row| row.get(0),
    )?;
    if count > MAX_CLAIM_BATCH as i64 {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory settlement recovery exceeds its contribution bound".to_owned(),
        )
        .into());
    }
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT event_id, proposal_json, principal, workspace, scope_binding_ref
               FROM app_memory_contribution_outbox
              WHERE installation_id=?1 AND settlement_ref=?2
                AND length(proposal_json) BETWEEN 1 AND ?3
              ORDER BY sequence ASC LIMIT ?4",
        )?;
        let mapped = statement.query_map(
            params![
                installation_id,
                settlement_ref,
                APP_CONTRIBUTION_MAX_DOCUMENT_BYTES as i64,
                MAX_CLAIM_BATCH as i64,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut proposals = Vec::with_capacity(rows.len());
    for (event_id, bytes, principal, workspace, scope_binding_ref) in rows {
        let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&bytes)?;
        proposal.validate()?;
        let sealed_settlement = match &proposal.header.settlement {
            AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id,
                ..
            } => mutation_receipt_id,
            AppContributionSettlementRefV1::TypedResult { result_ref, .. } => result_ref,
        };
        if proposal.header.installation_id != installation_id
            || proposal.header.scope_binding_ref != scope_binding_ref
            || sealed_settlement != settlement_ref
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
        validate_normalized_proposal_source(transaction, &event_id, &proposal)?;
        validate_current_proposal_head(
            transaction,
            &event_id,
            &proposal,
            &bytes,
            &principal,
            &workspace,
            &scope_binding_ref,
        )?;
        proposals.push(proposal);
    }
    Ok(proposals)
}

impl AppRegistryService {
    pub(crate) async fn claim_memory_contribution_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: String,
        limit: usize,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppMemoryContributionLease>, AppContributionError> {
        validate_claim(&lease_owner, limit, lease_duration)?;
        let destination_scope = (
            authenticated.scope().principal.as_str().to_owned(),
            authenticated.scope().workspace.as_str().to_owned(),
            authenticated.scope_binding_ref().as_str().to_owned(),
        );
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            claim_contributions(
                connection,
                lease_owner,
                destination_scope,
                limit,
                lease_duration,
                &now,
            )
        })
        .await
    }

    pub(crate) async fn acknowledge_memory_contribution_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        permit: AppMemoryIngressAckPermit,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
        let (receipt, dispatch_ack) = permit
            .into_parts()
            .map_err(|message| AppRegistryError::InvalidControlPlane(message.to_owned()))?;
        ensure_authenticated_scope(
            authenticated,
            &dispatch_ack.principal,
            &dispatch_ack.workspace,
            &dispatch_ack.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            acknowledge_contribution(connection, dispatch_ack, receipt, &now)
        })
        .await
    }

    pub(crate) async fn begin_memory_contribution_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppMemoryContributionLease,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryContributionDispatchPermit, AppContributionError> {
        ensure_authenticated_scope(
            authenticated,
            &lease.principal,
            &lease.workspace,
            &lease.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            begin_contribution_dispatch(connection, lease, &now)
        })
        .await
    }

    #[allow(dead_code)] // Only safe before dispatch; live post-dispatch recovery uses lease expiry.
    pub(crate) async fn release_memory_contribution_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppMemoryContributionLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        validate_retry(retry_delay)?;
        ensure_authenticated_scope(
            authenticated,
            &lease.principal,
            &lease.workspace,
            &lease.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            release_row(
                connection,
                "app_memory_contribution_outbox",
                lease.sequence,
                &lease.event_id,
                &lease.lease_owner,
                &lease.lease_token,
                &lease.lease_expires_at,
                &lease.principal,
                &lease.workspace,
                &lease.scope_binding_ref,
                retry_delay,
                &now,
            )
        })
        .await
    }

    pub(crate) async fn claim_memory_invalidation_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: String,
        limit: usize,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppMemoryInvalidationLease>, AppContributionError> {
        validate_claim(&lease_owner, limit, lease_duration)?;
        let destination_scope = (
            authenticated.scope().principal.as_str().to_owned(),
            authenticated.scope().workspace.as_str().to_owned(),
            authenticated.scope_binding_ref().as_str().to_owned(),
        );
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            claim_invalidations(
                connection,
                lease_owner,
                destination_scope,
                limit,
                lease_duration,
                &now,
            )
        })
        .await
    }

    pub(crate) async fn acknowledge_memory_invalidation_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        permit: AppMemoryInvalidationAckPermit,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
        let (receipt, dispatch_ack) = permit
            .into_parts()
            .map_err(|message| AppRegistryError::InvalidControlPlane(message.to_owned()))?;
        ensure_authenticated_scope(
            authenticated,
            &dispatch_ack.principal,
            &dispatch_ack.workspace,
            &dispatch_ack.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            acknowledge_invalidation(connection, dispatch_ack, receipt, &now)
        })
        .await
    }

    pub(crate) async fn begin_memory_invalidation_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppMemoryInvalidationLease,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryInvalidationDispatchPermit, AppContributionError> {
        ensure_authenticated_scope(
            authenticated,
            &lease.principal,
            &lease.workspace,
            &lease.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            begin_invalidation_dispatch(connection, lease, &now)
        })
        .await
    }

    pub(crate) async fn compact_memory_contribution_journals(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            terminalize_expired_and_delivered(&transaction, &now, 64)?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    #[allow(dead_code)] // Only safe before dispatch; live post-dispatch recovery uses lease expiry.
    pub(crate) async fn release_memory_invalidation_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppMemoryInvalidationLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        validate_retry(retry_delay)?;
        ensure_authenticated_scope(
            authenticated,
            &lease.principal,
            &lease.workspace,
            &lease.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            release_row(
                connection,
                "app_memory_invalidation_outbox",
                lease.sequence,
                &lease.event_id,
                &lease.lease_owner,
                &lease.lease_token,
                &lease.lease_expires_at,
                &lease.principal,
                &lease.workspace,
                &lease.scope_binding_ref,
                retry_delay,
                &now,
            )
        })
        .await
    }
}

fn ensure_authenticated_scope(
    authenticated: &AuthenticatedAppScope,
    principal: &str,
    workspace: &str,
    scope_binding_ref: &str,
) -> Result<(), AppContributionError> {
    if authenticated.scope().principal.as_str() != principal
        || authenticated.scope().workspace.as_str() != workspace
        || authenticated.scope_binding_ref().as_str() != scope_binding_ref
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory dispatch scope differs from the authenticated app scope".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn validate_normalized_proposal_source(
    transaction: &Transaction<'_>,
    event_id: &str,
    proposal: &AppMemoryCandidateProposalV1,
) -> Result<(), AppContributionError> {
    let source = proposal.header.sources.first().ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("V1 proposal lost its exact source".to_owned())
    })?;
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT installation_id, entity_name, record_id, record_revision,
                    canonical_source_ref, source_identity_digest
               FROM app_memory_contribution_sources WHERE event_id=?1
              ORDER BY installation_id, entity_name, record_id",
        )?;
        let mapped = statement.query_map([event_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    if rows.len() != 1
        || rows[0].0 != source.installation_id
        || rows[0].1 != source.entity_name
        || rows[0].2 != source.record_id
        || u64::try_from(rows[0].3).ok() != Some(source.record_revision)
        || rows[0].4 != source.canonical_source_ref
        || rows[0].5 != source.canonical_source_digest
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    Ok(())
}

fn validate_current_proposal_head(
    transaction: &Transaction<'_>,
    event_id: &str,
    proposal: &AppMemoryCandidateProposalV1,
    proposal_json: &[u8],
    principal: &str,
    workspace: &str,
    scope_binding_ref: &str,
) -> Result<(), AppContributionError> {
    let source = proposal.header.sources.first().ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("V1 proposal lost its source".to_owned())
    })?;
    let exact: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM app_memory_contribution_heads
          WHERE installation_id=?1 AND dedupe_key=?2 AND event_id=?3
            AND proposal_id=?4 AND proposal_revision=?5 AND proposal_digest=?6
            AND payload_digest=?7 AND source_event_ref=?8
            AND source_event_revision=?9 AND source_identity_digest=?10
            AND source_entity_name=?11 AND source_record_id=?12
            AND principal=?13 AND workspace=?14 AND scope_binding_ref=?15
            AND proposal_json=?16 AND lifecycle_state='live'",
            params![
                proposal.header.installation_id,
                proposal.header.dedupe_key,
                event_id,
                proposal.header.proposal_id,
                i64::try_from(proposal.header.proposal_revision).map_err(|_| {
                    AppRegistryError::InvalidControlPlane("proposal revision overflow".to_owned())
                })?,
                proposal.proposal_digest,
                magician_app_contract::contribution::content_digest(proposal_json),
                source.canonical_source_ref,
                i64::try_from(source.record_revision).map_err(|_| {
                    AppRegistryError::InvalidControlPlane("source revision overflow".to_owned())
                })?,
                source.canonical_source_digest,
                source.entity_name,
                source.record_id,
                principal,
                workspace,
                scope_binding_ref,
                proposal_json,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if exact != Some(1) {
        return Err(AppContributionError::SubstitutedReplay);
    }
    Ok(())
}

fn validate_claim(
    owner: &str,
    limit: usize,
    duration: StdDuration,
) -> Result<(), AppContributionError> {
    if owner.is_empty()
        || owner.len() > 192
        || owner.chars().any(char::is_whitespace)
        || limit == 0
        || limit > MAX_CLAIM_BATCH
        || duration < StdDuration::from_secs(1)
        || duration > MAX_LEASE
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "invalid memory outbox claim".to_owned(),
        )
        .into());
    }
    Ok(())
}

#[allow(dead_code)] // Shared by the retained pre-dispatch release compatibility seam.
fn validate_retry(delay: StdDuration) -> Result<(), AppContributionError> {
    if delay > MAX_RETRY_DELAY {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory outbox retry exceeds one hour".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn claim_contributions(
    connection: &mut rusqlite::Connection,
    owner: String,
    destination_scope: (String, String, String),
    limit: usize,
    duration: StdDuration,
    now: &DateTime<Utc>,
) -> Result<Vec<AppMemoryContributionLease>, AppContributionError> {
    let expiry = lease_expiry(now, duration)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    terminalize_expired_and_delivered(&transaction, now, 64)?;
    let corrupt: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_memory_contribution_outbox
          WHERE delivered_at IS NULL AND available_at <= ?1 AND proposal_expires_at > ?1
            AND (delivery_state='pending' OR (delivery_state IN ('leased','dispatching') AND \
         lease_expires_at <= ?1))
            AND (length(event_id) NOT BETWEEN 1 AND 192
                 OR length(proposal_json) NOT BETWEEN 1 AND ?2)",
        params![timestamp(now), APP_CONTRIBUTION_MAX_DOCUMENT_BYTES as i64],
        |row| row.get(0),
    )?;
    if corrupt != 0 {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory contribution outbox contains a corrupt ready row".to_owned(),
        )
        .into());
    }
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT sequence, event_id, proposal_id, proposal_revision,
                    proposal_digest, payload_digest, source_event_ref,
                    source_event_revision, source_identity_digest, installation_id,
                    dedupe_key, settlement_ref, proposal_expires_at, proposal_json,
                    attempt_count, lease_epoch, principal, workspace, scope_binding_ref
               FROM app_memory_contribution_outbox
              WHERE delivered_at IS NULL AND available_at <= ?1 AND proposal_expires_at > ?1
                AND (delivery_state = 'pending'
                     OR (delivery_state IN ('leased','dispatching') AND lease_expires_at <= ?1))
                AND length(proposal_json) BETWEEN 1 AND ?2
              ORDER BY sequence ASC LIMIT ?3",
        )?;
        let mapped = statement.query_map(
            params![
                timestamp(now),
                APP_CONTRIBUTION_MAX_DOCUMENT_BYTES as i64,
                limit as i64
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Vec<u8>>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                ))
            },
        )?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut leases = Vec::with_capacity(rows.len());
    for (
        sequence,
        event_id,
        proposal_id,
        proposal_revision,
        proposal_digest,
        payload_digest,
        source_event_ref,
        source_event_revision,
        source_identity_digest,
        installation_id,
        dedupe_key,
        settlement_ref,
        stored_expiry,
        bytes,
        attempts,
        prior_epoch,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
    ) in rows
    {
        let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&bytes)?;
        let prepared = PreparedMemoryContribution::new(proposal)?;
        let sealed_settlement = match &prepared.proposal.header.settlement {
            AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id,
                ..
            } => mutation_receipt_id,
            AppContributionSettlementRefV1::TypedResult { result_ref, .. } => result_ref,
        };
        let exact_expiry =
            DateTime::<Utc>::from_timestamp_millis(prepared.proposal.header.expires_at_ms)
                .ok_or_else(|| {
                    AppRegistryError::InvalidControlPlane(
                        "proposal expiry is outside the supported timestamp range".to_owned(),
                    )
                })?
                .to_rfc3339_opts(SecondsFormat::Millis, true);
        let source = prepared.proposal.header.sources.first().ok_or_else(|| {
            AppRegistryError::InvalidControlPlane("V1 contribution has no source".to_owned())
        })?;
        if prepared.event_id != event_id
            || prepared.proposal.header.proposal_id != proposal_id
            || i64::try_from(prepared.proposal.header.proposal_revision).ok()
                != Some(proposal_revision)
            || prepared.proposal.proposal_digest != proposal_digest
            || prepared.payload_digest != payload_digest
            || source.canonical_source_ref != source_event_ref
            || i64::try_from(source.record_revision).ok() != Some(source_event_revision)
            || source.canonical_source_digest != source_identity_digest
            || prepared.proposal.header.installation_id != installation_id
            || prepared.proposal.header.dedupe_key != dedupe_key
            || sealed_settlement != &settlement_ref
            || exact_expiry != stored_expiry
            || prepared.proposal.header.scope_binding_ref != destination_scope.2
            || stored_principal != destination_scope.0
            || stored_workspace != destination_scope.1
            || stored_scope_binding != destination_scope.2
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "memory contribution row substituted its sealed payload".to_owned(),
            )
            .into());
        }
        validate_normalized_proposal_source(&transaction, &event_id, &prepared.proposal)?;
        validate_current_proposal_head(
            &transaction,
            &event_id,
            &prepared.proposal,
            &bytes,
            &stored_principal,
            &stored_workspace,
            &stored_scope_binding,
        )?;
        let lease_epoch = prior_epoch.checked_add(1).ok_or_else(|| {
            AppRegistryError::InvalidControlPlane("memory outbox lease epoch overflow".to_owned())
        })?;
        let token = format!("memory-outbox-lease:{}", Uuid::new_v4().simple());
        if transaction.execute(
            "UPDATE app_memory_contribution_outbox SET delivery_state='leased',
                    lease_owner=?1, lease_token=?2, lease_expires_at=?3,
                    attempt_count=attempt_count+1, lease_epoch=?4
              WHERE sequence=?5 AND event_id=?6 AND delivered_at IS NULL
                AND available_at <= ?7 AND (delivery_state='pending'
                     OR (delivery_state IN ('leased','dispatching') AND lease_expires_at <= ?7))",
            params![
                owner,
                token,
                timestamp(&expiry),
                lease_epoch,
                sequence,
                event_id,
                timestamp(now)
            ],
        )? != 1
        {
            return Err(AppRegistryError::OutboxLeaseStale.into());
        }
        leases.push(AppMemoryContributionLease {
            sequence,
            event_id,
            proposal: prepared.proposal,
            proposal_json: bytes,
            lease_owner: owner.clone(),
            lease_token: token,
            lease_expires_at: expiry.clone(),
            attempt_count: u32::try_from(attempts.saturating_add(1)).map_err(|_| {
                AppRegistryError::InvalidControlPlane("memory outbox attempts overflow".to_owned())
            })?,
            lease_epoch: u64::try_from(lease_epoch).map_err(|_| {
                AppRegistryError::InvalidControlPlane(
                    "memory outbox lease epoch is negative".to_owned(),
                )
            })?,
            principal: destination_scope.0.clone(),
            workspace: destination_scope.1.clone(),
            scope_binding_ref: destination_scope.2.clone(),
        });
    }
    transaction.commit()?;
    Ok(leases)
}

fn claim_invalidations(
    connection: &mut rusqlite::Connection,
    owner: String,
    destination_scope: (String, String, String),
    limit: usize,
    duration: StdDuration,
    now: &DateTime<Utc>,
) -> Result<Vec<AppMemoryInvalidationLease>, AppContributionError> {
    let expiry = lease_expiry(now, duration)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    terminalize_expired_and_delivered(&transaction, now, 64)?;
    let corrupt: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_memory_invalidation_outbox
          WHERE delivered_at IS NULL AND available_at <= ?1
            AND (delivery_state='pending' OR (delivery_state IN ('leased','dispatching') AND \
         lease_expires_at <= ?1))
            AND (length(event_id) NOT BETWEEN 1 AND 192
                 OR length(invalidation_json) NOT BETWEEN 1 AND ?2
                 OR length(source_proposal_json) NOT BETWEEN 1 AND ?3)",
        params![
            timestamp(now),
            APP_CONTRIBUTION_MAX_INVALIDATION_BYTES as i64,
            APP_CONTRIBUTION_MAX_DOCUMENT_BYTES as i64
        ],
        |row| row.get(0),
    )?;
    if corrupt != 0 {
        return Err(AppRegistryError::InvalidControlPlane(
            "memory invalidation outbox contains a corrupt ready row".to_owned(),
        )
        .into());
    }
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT sequence, event_id, invalidation_id, invalidation_digest,
                    payload_digest, proposal_id, proposal_digest, installation_id,
                    dedupe_key, source_event_ref, source_event_revision,
                    source_identity_digest, invalidation_json, attempt_count, lease_epoch,
                    principal, workspace, scope_binding_ref,
                    source_entity_name, source_record_id, source_proposal_json
               FROM app_memory_invalidation_outbox
              WHERE delivered_at IS NULL AND available_at <= ?1
                AND (delivery_state = 'pending'
                     OR (delivery_state IN ('leased','dispatching') AND lease_expires_at <= ?1))
                AND length(invalidation_json) BETWEEN 1 AND ?2
                AND length(source_proposal_json) BETWEEN 1 AND ?3
                AND sequence=(
                    SELECT MIN(prior.sequence) FROM app_memory_invalidation_outbox prior
                     WHERE prior.installation_id=app_memory_invalidation_outbox.installation_id
                       AND prior.dedupe_key=app_memory_invalidation_outbox.dedupe_key
                       AND prior.proposal_id=app_memory_invalidation_outbox.proposal_id
                       AND prior.proposal_digest=app_memory_invalidation_outbox.proposal_digest
                       AND \
             prior.source_identity_digest=app_memory_invalidation_outbox.source_identity_digest
                       AND prior.delivery_state IN ('pending','leased','dispatching')
                )
              ORDER BY sequence ASC LIMIT ?4",
        )?;
        let mapped = statement.query_map(
            params![
                timestamp(now),
                APP_CONTRIBUTION_MAX_INVALIDATION_BYTES as i64,
                APP_CONTRIBUTION_MAX_DOCUMENT_BYTES as i64,
                limit as i64
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Vec<u8>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, Vec<u8>>(20)?,
                ))
            },
        )?;
        let rows = mapped.collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut leases = Vec::with_capacity(rows.len());
    for (
        sequence,
        event_id,
        invalidation_id,
        invalidation_digest,
        payload_digest,
        proposal_id,
        proposal_digest,
        installation_id,
        dedupe_key,
        source_event_ref,
        source_event_revision,
        source_identity_digest,
        bytes,
        attempts,
        prior_epoch,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
        source_entity_name,
        source_record_id,
        source_proposal_json,
    ) in rows
    {
        let invalidation: AppMemoryInvalidationV1 = serde_json::from_slice(&bytes)?;
        let prepared = PreparedMemoryInvalidation::new(invalidation)?;
        validate_retained_invalidation_source(
            &prepared,
            &installation_id,
            &source_entity_name,
            &source_record_id,
            &source_proposal_json,
        )?;
        if prepared.event_id != event_id
            || prepared.invalidation.invalidation_id != invalidation_id
            || prepared.invalidation.invalidation_digest != invalidation_digest
            || prepared.payload_digest != payload_digest
            || prepared.invalidation.proposal_id != proposal_id
            || prepared.invalidation.proposal_digest != proposal_digest
            || prepared.invalidation.dedupe_key != dedupe_key
            || prepared.invalidation.source_event_ref != source_event_ref
            || i64::try_from(prepared.invalidation.source_event_revision).ok()
                != Some(source_event_revision)
            || prepared.invalidation.source_identity_digest != source_identity_digest
            || prepared.invalidation.installation_id != installation_id
            || prepared.invalidation.scope_binding_ref != destination_scope.2
            || stored_principal != destination_scope.0
            || stored_workspace != destination_scope.1
            || stored_scope_binding != destination_scope.2
            || destination_scope.2.is_empty()
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "memory invalidation row substituted its sealed payload".to_owned(),
            )
            .into());
        }
        let lease_epoch = prior_epoch.checked_add(1).ok_or_else(|| {
            AppRegistryError::InvalidControlPlane(
                "memory invalidation lease epoch overflow".to_owned(),
            )
        })?;
        let token = format!("memory-invalidation-lease:{}", Uuid::new_v4().simple());
        if transaction.execute(
            "UPDATE app_memory_invalidation_outbox SET delivery_state='leased',
                    lease_owner=?1, lease_token=?2, lease_expires_at=?3,
                    attempt_count=attempt_count+1, lease_epoch=?4
              WHERE sequence=?5 AND event_id=?6 AND delivered_at IS NULL
                AND available_at <= ?7 AND (delivery_state='pending'
                     OR (delivery_state IN ('leased','dispatching') AND lease_expires_at <= ?7))",
            params![
                owner,
                token,
                timestamp(&expiry),
                lease_epoch,
                sequence,
                event_id,
                timestamp(now)
            ],
        )? != 1
        {
            return Err(AppRegistryError::OutboxLeaseStale.into());
        }
        leases.push(AppMemoryInvalidationLease {
            sequence,
            event_id,
            installation_id,
            invalidation: prepared.invalidation,
            invalidation_json: bytes,
            source_proposal_json,
            lease_owner: owner.clone(),
            lease_token: token,
            lease_expires_at: expiry.clone(),
            attempt_count: u32::try_from(attempts.saturating_add(1)).map_err(|_| {
                AppRegistryError::InvalidControlPlane(
                    "memory invalidation attempts overflow".to_owned(),
                )
            })?,
            lease_epoch: u64::try_from(lease_epoch).map_err(|_| {
                AppRegistryError::InvalidControlPlane(
                    "memory invalidation lease epoch is negative".to_owned(),
                )
            })?,
            principal: destination_scope.0.clone(),
            workspace: destination_scope.1.clone(),
            scope_binding_ref: destination_scope.2.clone(),
        });
    }
    transaction.commit()?;
    Ok(leases)
}

fn begin_contribution_dispatch(
    connection: &mut rusqlite::Connection,
    lease: AppMemoryContributionLease,
    now: &DateTime<Utc>,
) -> Result<AppMemoryContributionDispatchPermit, AppContributionError> {
    if now >= &lease.lease_expires_at
        || now.timestamp_millis() >= lease.proposal.header.expires_at_ms
    {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let expected_head = expected_destination_head(&transaction)?;
    let stored = transaction
        .query_row(
            "SELECT proposal_json, proposal_id, proposal_revision, proposal_digest,
                payload_digest, installation_id, dedupe_key, lease_owner,
                lease_token, lease_expires_at, lease_epoch, delivery_state,
                source_event_ref, source_event_revision, source_identity_digest,
                settlement_ref, proposal_expires_at, principal, workspace, scope_binding_ref
           FROM app_memory_contribution_outbox WHERE sequence=?1 AND event_id=?2",
            params![lease.sequence, lease.event_id],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                ))
            },
        )
        .optional()?;
    let Some((
        payload,
        proposal_id,
        proposal_revision,
        proposal_digest,
        payload_digest,
        installation_id,
        dedupe_key,
        owner,
        token,
        expiry,
        epoch,
        state,
        source_event_ref,
        source_event_revision,
        source_identity_digest,
        settlement_ref,
        proposal_expires_at,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
    )) = stored
    else {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    };
    let prepared = PreparedMemoryContribution::new(lease.proposal.clone())?;
    let source = prepared.proposal.header.sources.first().ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("V1 contribution has no exact source".to_owned())
    })?;
    let sealed_settlement = match &prepared.proposal.header.settlement {
        AppContributionSettlementRefV1::Mutation {
            mutation_receipt_id,
            ..
        } => mutation_receipt_id,
        AppContributionSettlementRefV1::TypedResult { result_ref, .. } => result_ref,
    };
    let sealed_expiry =
        DateTime::<Utc>::from_timestamp_millis(prepared.proposal.header.expires_at_ms)
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "proposal expiry is outside the supported timestamp range".to_owned(),
                )
            })?
            .to_rfc3339_opts(SecondsFormat::Millis, true);
    if payload != lease.proposal_json
        || payload != prepared.proposal_json
        || proposal_id != prepared.proposal.header.proposal_id
        || i64::try_from(prepared.proposal.header.proposal_revision).ok() != Some(proposal_revision)
        || proposal_digest != prepared.proposal.proposal_digest
        || payload_digest != prepared.payload_digest
        || installation_id != prepared.proposal.header.installation_id
        || dedupe_key != prepared.proposal.header.dedupe_key
        || source_event_ref != source.canonical_source_ref
        || i64::try_from(source.record_revision).ok() != Some(source_event_revision)
        || source_identity_digest != source.canonical_source_digest
        || settlement_ref.as_str() != sealed_settlement.as_str()
        || proposal_expires_at != sealed_expiry
        || prepared.proposal.header.scope_binding_ref != lease.scope_binding_ref
        || stored_principal != lease.principal
        || stored_workspace != lease.workspace
        || stored_scope_binding != lease.scope_binding_ref
        || owner.as_deref() != Some(lease.lease_owner.as_str())
        || token.as_deref() != Some(lease.lease_token.as_str())
        || expiry.as_deref() != Some(timestamp(&lease.lease_expires_at).as_str())
        || u64::try_from(epoch).ok() != Some(lease.lease_epoch)
        || state != "leased"
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    validate_normalized_proposal_source(&transaction, &lease.event_id, &prepared.proposal)?;
    validate_current_proposal_head(
        &transaction,
        &lease.event_id,
        &prepared.proposal,
        &lease.proposal_json,
        &lease.principal,
        &lease.workspace,
        &lease.scope_binding_ref,
    )?;
    if transaction.execute(
        "UPDATE app_memory_contribution_outbox SET delivery_state='dispatching'
          WHERE sequence=?1 AND event_id=?2 AND delivery_state='leased'
            AND lease_owner=?3 AND lease_token=?4 AND lease_epoch=?5
            AND lease_expires_at=?6 AND lease_expires_at>?7
            AND proposal_expires_at>?7",
        params![
            lease.sequence,
            lease.event_id,
            lease.lease_owner,
            lease.lease_token,
            epoch,
            timestamp(&lease.lease_expires_at),
            timestamp(now)
        ],
    )? != 1
    {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    transaction.commit()?;
    let absolute_expires_at_ms = lease
        .lease_expires_at
        .timestamp_millis()
        .min(lease.proposal.header.expires_at_ms);
    Ok(AppMemoryContributionDispatchPermit {
        evidence: AppMemoryContributionDispatchEvidence {
            proposal: lease.proposal,
            dispatch_ack: AppMemoryContributionDispatchAck {
                sequence: lease.sequence,
                event_id: lease.event_id,
                payload_json: lease.proposal_json,
                lease_owner: lease.lease_owner,
                lease_token: lease.lease_token,
                lease_epoch: lease.lease_epoch,
                lease_expires_at: lease.lease_expires_at,
                principal: lease.principal.clone(),
                workspace: lease.workspace.clone(),
                scope_binding_ref: lease.scope_binding_ref.clone(),
            },
            principal: lease.principal,
            workspace: lease.workspace,
            scope_binding_ref: lease.scope_binding_ref,
            expected_destination_head: expected_head,
            absolute_expires_at_ms,
        },
    })
}

fn begin_invalidation_dispatch(
    connection: &mut rusqlite::Connection,
    lease: AppMemoryInvalidationLease,
    now: &DateTime<Utc>,
) -> Result<AppMemoryInvalidationDispatchPermit, AppContributionError> {
    if now >= &lease.lease_expires_at {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let expected_head = expected_destination_head(&transaction)?;
    let stored = transaction
        .query_row(
            "SELECT invalidation_json, invalidation_id, invalidation_digest,
                payload_digest, proposal_id, proposal_digest, lease_owner,
                lease_token, lease_expires_at, lease_epoch, delivery_state,
                installation_id, dedupe_key, source_event_ref,
                source_event_revision, source_identity_digest,
                principal, workspace, scope_binding_ref,
                source_entity_name, source_record_id, source_proposal_json
           FROM app_memory_invalidation_outbox WHERE sequence=?1 AND event_id=?2",
            params![lease.sequence, lease.event_id],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, Vec<u8>>(21)?,
                ))
            },
        )
        .optional()?;
    let Some((
        payload,
        invalidation_id,
        invalidation_digest,
        payload_digest,
        proposal_id,
        proposal_digest,
        owner,
        token,
        expiry,
        epoch,
        state,
        installation_id,
        dedupe_key,
        source_event_ref,
        source_event_revision,
        source_identity_digest,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
        source_entity_name,
        source_record_id,
        source_proposal_json,
    )) = stored
    else {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    };
    let prepared = PreparedMemoryInvalidation::new(lease.invalidation.clone())?;
    validate_retained_invalidation_source(
        &prepared,
        &lease.installation_id,
        &source_entity_name,
        &source_record_id,
        &source_proposal_json,
    )?;
    if payload != lease.invalidation_json
        || source_proposal_json != lease.source_proposal_json
        || payload != prepared.invalidation_json
        || invalidation_id != prepared.invalidation.invalidation_id
        || invalidation_digest != prepared.invalidation.invalidation_digest
        || payload_digest != prepared.payload_digest
        || proposal_id != prepared.invalidation.proposal_id
        || proposal_digest != prepared.invalidation.proposal_digest
        || installation_id != lease.installation_id
        || prepared.invalidation.installation_id != lease.installation_id
        || prepared.invalidation.scope_binding_ref != lease.scope_binding_ref
        || dedupe_key != prepared.invalidation.dedupe_key
        || source_event_ref != prepared.invalidation.source_event_ref
        || i64::try_from(prepared.invalidation.source_event_revision).ok()
            != Some(source_event_revision)
        || source_identity_digest != prepared.invalidation.source_identity_digest
        || stored_principal != lease.principal
        || stored_workspace != lease.workspace
        || stored_scope_binding != lease.scope_binding_ref
        || owner.as_deref() != Some(lease.lease_owner.as_str())
        || token.as_deref() != Some(lease.lease_token.as_str())
        || expiry.as_deref() != Some(timestamp(&lease.lease_expires_at).as_str())
        || u64::try_from(epoch).ok() != Some(lease.lease_epoch)
        || state != "leased"
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    if transaction.execute(
        "UPDATE app_memory_invalidation_outbox SET delivery_state='dispatching'
          WHERE sequence=?1 AND event_id=?2 AND delivery_state='leased'
            AND lease_owner=?3 AND lease_token=?4 AND lease_epoch=?5
            AND lease_expires_at=?6 AND lease_expires_at>?7",
        params![
            lease.sequence,
            lease.event_id,
            lease.lease_owner,
            lease.lease_token,
            epoch,
            timestamp(&lease.lease_expires_at),
            timestamp(now)
        ],
    )? != 1
    {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    transaction.commit()?;
    Ok(AppMemoryInvalidationDispatchPermit {
        evidence: AppMemoryInvalidationDispatchEvidence {
            invalidation: lease.invalidation,
            dispatch_ack: AppMemoryInvalidationDispatchAck {
                sequence: lease.sequence,
                event_id: lease.event_id,
                payload_json: lease.invalidation_json,
                lease_owner: lease.lease_owner,
                lease_token: lease.lease_token,
                lease_epoch: lease.lease_epoch,
                lease_expires_at: lease.lease_expires_at,
                principal: lease.principal.clone(),
                workspace: lease.workspace.clone(),
                scope_binding_ref: lease.scope_binding_ref.clone(),
            },
            principal: lease.principal,
            workspace: lease.workspace,
            scope_binding_ref: lease.scope_binding_ref,
            expected_destination_head: expected_head,
            absolute_expires_at_ms: lease.lease_expires_at.timestamp_millis(),
        },
    })
}

fn acknowledge_contribution(
    connection: &mut rusqlite::Connection,
    dispatch: AppMemoryContributionDispatchAck,
    receipt: AppMemoryIngressReceiptV1,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    receipt.validate()?;
    let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&dispatch.payload_json)?;
    let prepared = PreparedMemoryContribution::new(proposal)?;
    if receipt.proposal_id != prepared.proposal.header.proposal_id
        || receipt.proposal_digest != prepared.proposal.proposal_digest
        || prepared.proposal.header.scope_binding_ref != dispatch.scope_binding_ref
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    let receipt_json = serde_json::to_vec(&receipt)?;
    acknowledge_row(
        connection,
        "app_memory_contribution_outbox",
        "proposal_json",
        "ingress_receipt_json",
        "ingress_receipt_digest",
        "app_memory_contribution_terminal",
        dispatch.sequence,
        &dispatch.event_id,
        &dispatch.payload_json,
        &prepared.payload_digest,
        &dispatch.lease_owner,
        &dispatch.lease_token,
        dispatch.lease_epoch,
        &dispatch.lease_expires_at,
        &dispatch.principal,
        &dispatch.workspace,
        &dispatch.scope_binding_ref,
        &receipt_json,
        &receipt.receipt_digest,
        receipt.destination_generation,
        &receipt.destination_receipt_digest,
        now,
    )
}

fn acknowledge_invalidation(
    connection: &mut rusqlite::Connection,
    dispatch: AppMemoryInvalidationDispatchAck,
    receipt: AppMemoryInvalidationReceiptV1,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    receipt.validate()?;
    let invalidation: AppMemoryInvalidationV1 = serde_json::from_slice(&dispatch.payload_json)?;
    let prepared = PreparedMemoryInvalidation::new(invalidation)?;
    if receipt.invalidation_id != prepared.invalidation.invalidation_id
        || receipt.invalidation_digest != prepared.invalidation.invalidation_digest
        || receipt.proposal_id != prepared.invalidation.proposal_id
        || prepared.invalidation.scope_binding_ref != dispatch.scope_binding_ref
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    let receipt_json = serde_json::to_vec(&receipt)?;
    acknowledge_row(
        connection,
        "app_memory_invalidation_outbox",
        "invalidation_json",
        "invalidation_receipt_json",
        "invalidation_receipt_digest",
        "app_memory_invalidation_terminal",
        dispatch.sequence,
        &dispatch.event_id,
        &dispatch.payload_json,
        &prepared.payload_digest,
        &dispatch.lease_owner,
        &dispatch.lease_token,
        dispatch.lease_epoch,
        &dispatch.lease_expires_at,
        &dispatch.principal,
        &dispatch.workspace,
        &dispatch.scope_binding_ref,
        &receipt_json,
        &receipt.receipt_digest,
        receipt.destination_generation,
        &receipt.destination_receipt_digest,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn acknowledge_row(
    connection: &mut rusqlite::Connection,
    table: &'static str,
    payload_column: &'static str,
    receipt_column: &'static str,
    digest_column: &'static str,
    terminal_table: &'static str,
    sequence: i64,
    event_id: &str,
    expected_payload: &[u8],
    expected_payload_digest: &str,
    owner: &str,
    token: &str,
    lease_epoch: u64,
    expiry: &DateTime<Utc>,
    principal: &str,
    workspace: &str,
    scope_binding_ref: &str,
    receipt_json: &[u8],
    receipt_digest: &str,
    destination_generation: u64,
    destination_receipt_digest: &str,
    now: &DateTime<Utc>,
) -> Result<AppMemoryOutboxAppendOutcome, AppContributionError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let query = format!(
        "SELECT {payload_column}, delivery_state, {receipt_column}, {digest_column}, lease_owner, \
         lease_token, lease_expires_at, lease_epoch, principal, workspace, scope_binding_ref FROM \
         {table} WHERE sequence=?1 AND event_id=?2"
    );
    let row = transaction
        .query_row(&query, params![sequence, event_id], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
            ))
        })
        .optional()?;
    let Some((
        payload,
        state,
        prior_receipt,
        prior_digest,
        stored_owner,
        stored_token,
        stored_expiry,
        stored_epoch,
        stored_principal,
        stored_workspace,
        stored_scope_binding,
    )) = row
    else {
        let terminal_query = format!(
            "SELECT payload_digest, source_ack_receipt_digest, destination_generation, \
             destination_receipt_digest, principal, workspace, scope_binding_ref FROM \
             {terminal_table} WHERE event_id=?1"
        );
        let terminal = transaction
            .query_row(&terminal_query, [event_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .optional()?;
        return match terminal {
            Some((
                payload_digest,
                Some(source_ack_digest),
                Some(generation),
                Some(destination_digest),
                terminal_principal,
                terminal_workspace,
                terminal_scope_binding,
            )) if payload_digest == expected_payload_digest
                && source_ack_digest == receipt_digest
                && u64::try_from(generation).ok() == Some(destination_generation)
                && destination_digest == destination_receipt_digest
                && terminal_principal == principal
                && terminal_workspace == workspace
                && terminal_scope_binding == scope_binding_ref =>
            {
                Ok(AppMemoryOutboxAppendOutcome::ExactReplay)
            },
            Some(_) => Err(AppContributionError::SubstitutedReplay),
            None => Err(AppRegistryError::OutboxLeaseStale.into()),
        };
    };
    if payload != expected_payload {
        return Err(AppContributionError::SubstitutedReplay);
    }
    if stored_principal != principal
        || stored_workspace != workspace
        || stored_scope_binding != scope_binding_ref
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    if table == "app_memory_contribution_outbox" {
        let proposal: AppMemoryCandidateProposalV1 = serde_json::from_slice(&payload)?;
        let prepared = PreparedMemoryContribution::new(proposal)?;
        if prepared.event_id != event_id || prepared.payload_digest != expected_payload_digest {
            return Err(AppContributionError::SubstitutedReplay);
        }
        validate_normalized_proposal_source(&transaction, event_id, &prepared.proposal)?;
        validate_current_proposal_head(
            &transaction,
            event_id,
            &prepared.proposal,
            &payload,
            principal,
            workspace,
            scope_binding_ref,
        )?;
    } else {
        let invalidation: AppMemoryInvalidationV1 = serde_json::from_slice(&payload)?;
        let prepared = PreparedMemoryInvalidation::new(invalidation)?;
        let retained_source = transaction.query_row(
            "SELECT source_entity_name, source_record_id, source_proposal_json
               FROM app_memory_invalidation_outbox
              WHERE sequence=?1 AND event_id=?2",
            params![sequence, event_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        validate_retained_invalidation_source(
            &prepared,
            &prepared.invalidation.installation_id,
            &retained_source.0,
            &retained_source.1,
            &retained_source.2,
        )?;
        let head = transaction
            .query_row(
                "SELECT principal, workspace, scope_binding_ref, proposal_id,
                    proposal_digest, source_event_ref, source_identity_digest,
                    source_event_revision, latest_invalidation_id,
                    latest_invalidation_digest, latest_invalidation_revision
               FROM app_memory_contribution_heads
              WHERE installation_id=?1 AND dedupe_key=?2",
                params![
                    prepared.invalidation.installation_id,
                    prepared.invalidation.dedupe_key
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                    ))
                },
            )
            .optional()?;
        let Some(head) = head else {
            return Err(AppContributionError::SubstitutedReplay);
        };
        let incoming_revision = i64::try_from(prepared.invalidation.source_event_revision)
            .map_err(|_| {
                AppRegistryError::InvalidControlPlane("source event revision overflow".to_owned())
            })?;
        if head.0 != principal
            || head.1 != workspace
            || head.2 != scope_binding_ref
            || head.3 != prepared.invalidation.proposal_id
            || head.4 != prepared.invalidation.proposal_digest
            || head.5 != prepared.invalidation.source_event_ref
            || head.6 != prepared.invalidation.source_identity_digest
            || incoming_revision <= head.7
            || head.10 == Some(incoming_revision)
                && (head.8.as_deref() != Some(prepared.invalidation.invalidation_id.as_str())
                    || head.9.as_deref()
                        != Some(prepared.invalidation.invalidation_digest.as_str()))
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
    }
    if state == "delivered" {
        if prior_receipt.as_deref() == Some(receipt_json)
            && prior_digest.as_deref() == Some(receipt_digest)
        {
            return Ok(AppMemoryOutboxAppendOutcome::ExactReplay);
        }
        return Err(AppContributionError::SubstitutedReplay);
    }
    if now >= expiry
        || stored_owner.as_deref() != Some(owner)
        || stored_token.as_deref() != Some(token)
        || stored_expiry.as_deref() != Some(timestamp(expiry).as_str())
        || u64::try_from(stored_epoch).ok() != Some(lease_epoch)
        || state != "dispatching"
    {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    let destination_generation = i64::try_from(destination_generation).map_err(|_| {
        AppRegistryError::InvalidControlPlane("destination generation overflow".to_owned())
    })?;
    let update = format!(
        "UPDATE {table} SET delivery_state='delivered', {receipt_column}=?1, {digest_column}=?2, \
         destination_generation=?3, destination_receipt_digest=?4, delivered_at=?5, \
         lease_owner=NULL, lease_token=NULL, lease_expires_at=NULL WHERE sequence=?6 AND \
         event_id=?7 AND delivery_state='dispatching' AND lease_owner=?8 AND lease_token=?9 AND \
         lease_epoch=?10 AND lease_expires_at=?11 AND delivered_at IS NULL"
    );
    if transaction.execute(
        &update,
        params![
            receipt_json,
            receipt_digest,
            destination_generation,
            destination_receipt_digest,
            timestamp(now),
            sequence,
            event_id,
            owner,
            token,
            i64::try_from(lease_epoch).map_err(|_| {
                AppRegistryError::InvalidControlPlane("lease epoch overflow".to_owned())
            })?,
            timestamp(expiry)
        ],
    )? != 1
    {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    let existing_high_water: Option<(i64, String)> = transaction
        .query_row(
            "SELECT destination_generation, destination_receipt_digest
           FROM app_memory_destination_ack_high_water WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if existing_high_water
        .as_ref()
        .is_some_and(|(generation, digest)| {
            *generation == destination_generation && digest != destination_receipt_digest
        })
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    transaction.execute(
        "INSERT INTO app_memory_destination_ack_high_water(
             singleton, destination_generation, destination_receipt_digest, updated_at
         ) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(singleton) DO UPDATE SET
             destination_generation=excluded.destination_generation,
             destination_receipt_digest=excluded.destination_receipt_digest,
             updated_at=excluded.updated_at
         WHERE excluded.destination_generation > \
         app_memory_destination_ack_high_water.destination_generation",
        params![
            destination_generation,
            destination_receipt_digest,
            timestamp(now)
        ],
    )?;
    if table == "app_memory_contribution_outbox" {
        terminalize_live_contribution(
            &transaction,
            event_id,
            expected_payload_digest,
            "delivered",
            now,
        )?;
    } else {
        terminalize_live_invalidation(&transaction, event_id, expected_payload_digest, now)?;
    }
    prune_recent_terminal_windows(&transaction)?;
    transaction.commit()?;
    Ok(AppMemoryOutboxAppendOutcome::Appended)
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // Retained solely for the pre-dispatch release compatibility seam.
fn release_row(
    connection: &mut rusqlite::Connection,
    table: &'static str,
    sequence: i64,
    event_id: &str,
    owner: &str,
    token: &str,
    expiry: &DateTime<Utc>,
    principal: &str,
    workspace: &str,
    scope_binding_ref: &str,
    retry_delay: StdDuration,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    if now >= expiry {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    let available = lease_expiry(now, retry_delay)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let update = format!(
        "UPDATE {table} SET delivery_state='pending', available_at=?1, lease_owner=NULL, \
         lease_token=NULL, lease_expires_at=NULL WHERE sequence=?2 AND event_id=?3 AND \
         delivery_state='leased' AND lease_owner=?4 AND lease_token=?5 AND lease_expires_at=?6 \
         AND principal=?7 AND workspace=?8 AND scope_binding_ref=?9 AND delivered_at IS NULL"
    );
    if transaction.execute(
        &update,
        params![
            timestamp(&available),
            sequence,
            event_id,
            owner,
            token,
            timestamp(expiry),
            principal,
            workspace,
            scope_binding_ref
        ],
    )? != 1
    {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    transaction.commit()?;
    Ok(())
}

fn lease_expiry(
    now: &DateTime<Utc>,
    duration: StdDuration,
) -> Result<DateTime<Utc>, AppContributionError> {
    now.checked_add_signed(Duration::from_std(duration).map_err(|_| {
        AppRegistryError::InvalidControlPlane("memory outbox duration overflow".to_owned())
    })?)
    .ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("memory outbox time overflow".to_owned()).into()
    })
}

fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    static_assertions::assert_not_impl_any!(
        AppMemoryContributionLease: Clone, serde::Serialize, serde::de::DeserializeOwned
    );
    static_assertions::assert_not_impl_any!(
        AppMemoryInvalidationLease: Clone, serde::Serialize, serde::de::DeserializeOwned
    );
    static_assertions::assert_not_impl_any!(
        AppMemoryContributionDispatchPermit: Clone, serde::Serialize, serde::de::DeserializeOwned
    );
    static_assertions::assert_not_impl_any!(
        AppMemoryInvalidationDispatchPermit: Clone, serde::Serialize, serde::de::DeserializeOwned
    );

    #[test]
    fn claim_and_retry_limits_are_bounded() {
        assert!(validate_claim("owner:1", MAX_CLAIM_BATCH, MAX_LEASE).is_ok());
        assert!(validate_claim("owner:1", MAX_CLAIM_BATCH + 1, MAX_LEASE).is_err());
        assert!(validate_retry(MAX_RETRY_DELAY + StdDuration::from_secs(1)).is_err());
    }

    #[test]
    fn expiration_identity_and_all_v17_ceilings_are_fixed() {
        assert_ne!(
            deterministic_expiration_invalidation_id("blake3:one", 10),
            deterministic_expiration_invalidation_id("blake3:one", 11),
        );
        assert_eq!(
            MAX_INVALIDATION_ROWS_PER_SCOPE,
            APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE * 2,
        );
        assert_eq!(
            crate::magician_v2::apps::contribution::APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND,
            1_024,
        );
    }
}
