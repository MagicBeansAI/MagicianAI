//! Ordered source journal for personal-agent retrieval projections.
//!
//! This journal is deliberately separate from app memory. The source commits
//! the exact shared proposal/invalidation DTO in its existing registry
//! transaction; a later high-level worker owns the only path from a move-only
//! lease to the private retrieval destination. One ordered row is dispatched
//! at a time, so the source acknowledgement high-water is also the expected
//! destination predecessor and response-lost retries cannot fork the chain.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use magician_app_contract::contribution::{
    content_digest, AppMemoryInvalidationReasonV1, AppMemoryInvalidationV1,
    AppPersonalAgentRetrievalProjectionProposalV1, APP_CONTRIBUTION_MAX_DOCUMENT_BYTES,
    APP_CONTRIBUTION_MAX_INVALIDATION_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use uuid::Uuid;

use super::{
    authority::AuthenticatedAppScope,
    contribution::{invalidation_advances_exact_source, AppContributionError},
    contribution_frequency::{
        consume_contribution_frequency_in_transaction, AppReviewedContributionFrequencyV1,
    },
    records::AppScope,
    registry::{AppRegistryError, AppRegistryService},
};
use crate::magician_v2::agents::{
    PersonalAgentRetrievalExpectedHeadV1, PersonalAgentRetrievalReceiptV1,
};

const MAX_OUTBOX_ROWS: i64 = 10_000;
const MAX_OUTBOX_BYTES: i64 = 64 * 1024 * 1024;
const MAX_PENDING_PER_INSTALLATION: i64 = 1_024;
const MAX_SOURCE_HEADS: i64 = 4_096;
const MAX_RECENT_TERMINALS: i64 = 2_048;
const MAX_CLAIM_LEASE: StdDuration = StdDuration::from_secs(5 * 60);
const MAX_RETRY_DELAY: StdDuration = StdDuration::from_secs(60 * 60);
const MAX_EVENT_PAYLOAD_BYTES: usize =
    APP_CONTRIBUTION_MAX_DOCUMENT_BYTES + APP_CONTRIBUTION_MAX_INVALIDATION_BYTES + 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRetrievalOutboxAppendOutcome {
    Appended,
    ExactReplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppRetrievalDeliveryKind {
    Proposal,
    Invalidation,
    Expiration,
}

impl AppRetrievalDeliveryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Proposal => "proposal",
            Self::Invalidation => "invalidation",
            Self::Expiration => "expiration",
        }
    }

    fn parse(value: &str) -> Result<Self, AppContributionError> {
        match value {
            "proposal" => Ok(Self::Proposal),
            "invalidation" => Ok(Self::Invalidation),
            "expiration" => Ok(Self::Expiration),
            _ => Err(invalid_control("unknown retrieval outbox event kind")),
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct InvalidationPayload<'a> {
    proposal: &'a AppPersonalAgentRetrievalProjectionProposalV1,
    invalidation: &'a AppMemoryInvalidationV1,
}

#[derive(Debug)]
pub(crate) struct AppRetrievalDeliveryLease {
    sequence: i64,
    event_id: String,
    kind: AppRetrievalDeliveryKind,
    proposal: AppPersonalAgentRetrievalProjectionProposalV1,
    proposal_json: Vec<u8>,
    invalidation: Option<AppMemoryInvalidationV1>,
    invalidation_json: Option<Vec<u8>>,
    payload_digest: String,
    principal: String,
    workspace: String,
    scope_binding_ref: String,
    lease_owner: String,
    lease_token: String,
    lease_expires_at: DateTime<Utc>,
    lease_epoch: u64,
    attempt_count: u32,
    dispatch_started: bool,
    expected_destination_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
}

impl AppRetrievalDeliveryLease {
    pub(crate) fn proposal(&self) -> &AppPersonalAgentRetrievalProjectionProposalV1 {
        &self.proposal
    }

    pub(crate) fn invalidation(&self) -> Option<&AppMemoryInvalidationV1> {
        self.invalidation.as_ref()
    }

    pub(crate) fn kind(&self) -> AppRetrievalDeliveryKind {
        self.kind
    }

    #[allow(dead_code)] // Retained for worker telemetry/qualification consumers.
    pub(crate) fn attempt_count(&self) -> u32 {
        self.attempt_count
    }

    pub(crate) fn was_dispatched(&self) -> bool {
        self.dispatch_started
    }
}

#[derive(Debug)]
pub(crate) struct AppRetrievalDeliveryDispatchPermit {
    evidence: AppRetrievalDeliveryDispatchEvidence,
}

#[derive(Debug)]
pub(crate) struct AppRetrievalDeliveryDispatchEvidence {
    pub proposal: AppPersonalAgentRetrievalProjectionProposalV1,
    pub invalidation: Option<AppMemoryInvalidationV1>,
    pub kind: AppRetrievalDeliveryKind,
    pub expected_destination_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
    pub dispatch_ack: AppRetrievalDeliveryDispatchAck,
    pub absolute_expires_at_ms: i64,
    pub attempt_count: u32,
}

impl AppRetrievalDeliveryDispatchPermit {
    pub(crate) fn into_evidence(
        self,
        now_ms: i64,
    ) -> Result<AppRetrievalDeliveryDispatchEvidence, AppContributionError> {
        if now_ms < 0 || now_ms >= self.evidence.absolute_expires_at_ms {
            return Err(AppRegistryError::OutboxLeaseStale.into());
        }
        Ok(self.evidence)
    }
}

#[derive(Debug)]
pub(crate) struct AppRetrievalDeliveryDispatchAck {
    sequence: i64,
    event_id: String,
    kind: AppRetrievalDeliveryKind,
    proposal_json: Vec<u8>,
    invalidation_json: Option<Vec<u8>>,
    payload_digest: String,
    principal: String,
    workspace: String,
    scope_binding_ref: String,
    lease_owner: String,
    lease_token: String,
    lease_expires_at: DateTime<Utc>,
    lease_epoch: u64,
    expected_destination_head: Option<PersonalAgentRetrievalExpectedHeadV1>,
}

#[derive(Debug)]
pub(crate) struct AppRetrievalSourceAckPermit {
    receipt: PersonalAgentRetrievalReceiptV1,
    dispatch: AppRetrievalDeliveryDispatchAck,
}

impl AppRetrievalSourceAckPermit {
    pub(crate) fn from_destination(
        receipt: PersonalAgentRetrievalReceiptV1,
        dispatch: AppRetrievalDeliveryDispatchAck,
    ) -> Self {
        Self { receipt, dispatch }
    }
}

/// Append one exact retrieval proposal beside its source settlement.
///
/// The caller already owns `transaction`; this function never commits it and
/// therefore cannot publish a projection without the terminal source record.
pub(crate) fn append_retrieval_projection_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    proposal: AppPersonalAgentRetrievalProjectionProposalV1,
    reviewed_frequency: AppReviewedContributionFrequencyV1,
    now: &DateTime<Utc>,
) -> Result<AppRetrievalOutboxAppendOutcome, AppContributionError> {
    validate_v18_storage_accounting(transaction)?;
    proposal.validate()?;
    let source = exact_source(&proposal)?;
    validate_proposal_scope(scope, scope_binding_ref, &proposal, now)?;
    if proposal.header.expires_at_ms <= now.timestamp_millis() {
        return Err(invalid_control(
            "retrieval proposal expired before source publication",
        ));
    }
    let proposal_json = serde_json::to_vec(&proposal)?;
    let payload_digest = content_digest(&proposal_json);
    let event_id = proposal_event_id(&proposal);

    consume_contribution_frequency_in_transaction(
        transaction,
        scope,
        scope_binding_ref,
        &proposal.header.installation_id,
        &proposal.header.workflow_id,
        &proposal.header.action_id,
        &proposal.header.contribution_port_id,
        reviewed_frequency,
        &proposal.header.proposal_id,
        proposal.header.proposal_revision,
        &proposal.proposal_digest,
        proposal.header.issued_at_ms,
        now,
    )?;

    if let Some(outcome) = exact_replay(
        transaction,
        &event_id,
        AppRetrievalDeliveryKind::Proposal,
        &proposal,
        None,
        &proposal_json,
        None,
        &payload_digest,
        scope,
        scope_binding_ref,
    )? {
        return Ok(outcome);
    }

    let prior = load_head(
        transaction,
        &proposal.header.installation_id,
        &proposal.header.dedupe_key,
    )?;
    if let Some(prior) = prior.as_ref() {
        if prior.proposal_id == proposal.header.proposal_id
            && prior.proposal_revision == proposal.header.proposal_revision
        {
            if prior.proposal_digest == proposal.proposal_digest
                && prior.proposal_payload_digest == payload_digest
                && prior.proposal_json.as_deref() == Some(proposal_json.as_slice())
                && prior.matches_scope(scope, scope_binding_ref)
            {
                return Ok(AppRetrievalOutboxAppendOutcome::ExactReplay);
            }
            return Err(AppContributionError::SubstitutedReplay);
        }
        let replacement_invalidation_pending = prior.lifecycle_state == "invalidating"
            && transaction
                .query_row(
                    "SELECT 1 FROM app_retrieval_delivery_outbox
                      WHERE event_kind='invalidation' AND installation_id=?1
                        AND proposal_id=?2 AND proposal_digest=?3
                        AND source_event_ref=?4 AND source_event_revision=?5
                        AND source_identity_digest=?6
                        AND delivery_state IN ('pending','leased','dispatching')
                      ORDER BY sequence ASC LIMIT 1",
                    params![
                        proposal.header.installation_id,
                        prior.proposal_id,
                        prior.proposal_digest,
                        prior.source_event_ref,
                        to_i64(source.record_revision, "source revision")?,
                        prior.source_identity_digest,
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_some();
        let source_advance_is_owned = (source.record_revision > prior.source_event_revision
            && prior.lifecycle_state == "settled")
            || (source.record_revision == prior.source_event_revision
                && source.canonical_source_digest != prior.source_identity_digest
                && (prior.lifecycle_state == "settled" || replacement_invalidation_pending));
        if proposal.header.proposal_revision <= prior.proposal_revision
            || source.canonical_source_ref != prior.source_event_ref
            || !source_advance_is_owned
        {
            return Err(invalid_control(
                "retrieval proposal did not advance one settled exact source head",
            ));
        }
    }

    enforce_append_quota(
        transaction,
        &proposal.header.installation_id,
        proposal_json.len(),
    )?;
    let revision = to_i64(proposal.header.proposal_revision, "proposal revision")?;
    let source_revision = to_i64(source.record_revision, "source revision")?;
    let expires_at = millis_timestamp(proposal.header.expires_at_ms, "proposal expiry")?;
    let now_text = timestamp(now);
    transaction.execute(
        "INSERT INTO app_retrieval_delivery_outbox(
             event_id, event_kind, proposal_id, proposal_revision,
             proposal_digest, payload_digest, principal, workspace,
             scope_binding_ref, installation_id, dedupe_key, source_event_ref,
             source_event_revision, source_identity_digest, source_entity_name,
             source_record_id, proposal_expires_at, proposal_json,
             delivery_state, available_at, created_at
         ) VALUES (?1, 'proposal', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                   ?11, ?12, ?13, ?14, ?15, ?16, ?17, 'pending', ?18, ?18)",
        params![
            event_id,
            proposal.header.proposal_id,
            revision,
            proposal.proposal_digest,
            payload_digest,
            scope.principal.as_str(),
            scope.workspace.as_str(),
            scope_binding_ref,
            proposal.header.installation_id,
            proposal.header.dedupe_key,
            source.canonical_source_ref,
            source_revision,
            source.canonical_source_digest,
            source.entity_name,
            source.record_id,
            expires_at,
            proposal_json,
            now_text,
        ],
    )?;
    transaction.execute(
        "INSERT INTO app_retrieval_projection_heads(
             installation_id, dedupe_key, principal, workspace, scope_binding_ref,
             proposal_id, proposal_revision, proposal_digest,
             proposal_payload_digest, source_event_ref, source_event_revision,
             source_identity_digest, source_entity_name, source_record_id,
             proposal_expires_at, proposal_json, lifecycle_state,
             latest_event_id, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, 'pending', ?17, ?18)
         ON CONFLICT(installation_id, dedupe_key) DO UPDATE SET
             principal=excluded.principal, workspace=excluded.workspace,
             scope_binding_ref=excluded.scope_binding_ref,
             proposal_id=excluded.proposal_id,
             proposal_revision=excluded.proposal_revision,
             proposal_digest=excluded.proposal_digest,
             proposal_payload_digest=excluded.proposal_payload_digest,
             source_event_ref=excluded.source_event_ref,
             source_event_revision=excluded.source_event_revision,
             source_identity_digest=excluded.source_identity_digest,
             source_entity_name=excluded.source_entity_name,
             source_record_id=excluded.source_record_id,
             proposal_expires_at=excluded.proposal_expires_at,
             proposal_json=excluded.proposal_json,
             lifecycle_state='pending', latest_event_id=excluded.latest_event_id,
             latest_invalidation_id=NULL, latest_invalidation_digest=NULL,
             updated_at=excluded.updated_at",
        params![
            proposal.header.installation_id,
            proposal.header.dedupe_key,
            scope.principal.as_str(),
            scope.workspace.as_str(),
            scope_binding_ref,
            proposal.header.proposal_id,
            revision,
            proposal.proposal_digest,
            payload_digest,
            source.canonical_source_ref,
            source_revision,
            source.canonical_source_digest,
            source.entity_name,
            source.record_id,
            expires_at,
            proposal_json,
            event_id,
            now_text,
        ],
    )?;
    validate_v18_storage_accounting(transaction)?;
    Ok(AppRetrievalOutboxAppendOutcome::Appended)
}

/// Append an invalidation together with the exact proposal whose reviewed
/// grant fence must be reconstructed by the destination worker.
pub(crate) fn append_retrieval_invalidation_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    proposal: AppPersonalAgentRetrievalProjectionProposalV1,
    invalidation: AppMemoryInvalidationV1,
    now: &DateTime<Utc>,
) -> Result<AppRetrievalOutboxAppendOutcome, AppContributionError> {
    validate_v18_storage_accounting(transaction)?;
    proposal.validate()?;
    invalidation.validate()?;
    let source = exact_source(&proposal)?;
    validate_proposal_scope(scope, scope_binding_ref, &proposal, now)?;
    if invalidation.installation_id != proposal.header.installation_id
        || invalidation.scope_binding_ref != proposal.header.scope_binding_ref
        || invalidation.proposal_id != proposal.header.proposal_id
        || invalidation.proposal_digest != proposal.proposal_digest
        || invalidation.dedupe_key != proposal.header.dedupe_key
        || invalidation.source_event_ref != source.canonical_source_ref
        || invalidation.source_identity_digest != source.canonical_source_digest
        || !invalidation_advances_exact_source(
            invalidation.reason,
            invalidation.source_event_revision,
            source.record_revision,
        )
        || invalidation.issued_at_ms > now.timestamp_millis()
    {
        return Err(invalid_control(
            "retrieval invalidation is not bound to its exact proposal source",
        ));
    }
    let proposal_json = serde_json::to_vec(&proposal)?;
    let invalidation_json = serde_json::to_vec(&invalidation)?;
    let payload_json = serde_json::to_vec(&InvalidationPayload {
        proposal: &proposal,
        invalidation: &invalidation,
    })?;
    let payload_digest = content_digest(&payload_json);
    let event_id = invalidation_event_id(&invalidation);
    if let Some(outcome) = exact_replay(
        transaction,
        &event_id,
        AppRetrievalDeliveryKind::Invalidation,
        &proposal,
        Some(&invalidation),
        &proposal_json,
        Some(&invalidation_json),
        &payload_digest,
        scope,
        scope_binding_ref,
    )? {
        return Ok(outcome);
    }
    let head = load_head(
        transaction,
        &proposal.header.installation_id,
        &proposal.header.dedupe_key,
    )?
    .ok_or_else(|| invalid_control("retrieval invalidation has no source head"))?;
    if !head.matches_scope(scope, scope_binding_ref)
        || head.proposal_id != proposal.header.proposal_id
        || head.proposal_revision != proposal.header.proposal_revision
        || head.proposal_digest != proposal.proposal_digest
        || head.proposal_payload_digest != content_digest(&proposal_json)
        || head.proposal_json.as_deref() != Some(proposal_json.as_slice())
        || head.source_event_ref != invalidation.source_event_ref
        || head.source_identity_digest != invalidation.source_identity_digest
        || !invalidation_advances_exact_source(
            invalidation.reason,
            invalidation.source_event_revision,
            head.source_event_revision,
        )
        || !matches!(head.lifecycle_state.as_str(), "pending" | "live")
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    terminalize_pending_proposal_before_invalidation(
        transaction,
        &proposal_event_id(&proposal),
        now,
    )?;
    compact_terminals(transaction, now)?;
    enforce_append_quota(
        transaction,
        &proposal.header.installation_id,
        proposal_json.len().saturating_add(invalidation_json.len()),
    )?;
    let revision = to_i64(proposal.header.proposal_revision, "proposal revision")?;
    let source_revision = to_i64(
        invalidation.source_event_revision,
        "invalidation source revision",
    )?;
    let expires_at = millis_timestamp(proposal.header.expires_at_ms, "proposal expiry")?;
    let now_text = timestamp(now);
    transaction.execute(
        "INSERT INTO app_retrieval_delivery_outbox(
             event_id, event_kind, proposal_id, proposal_revision,
             proposal_digest, invalidation_id, invalidation_digest,
             payload_digest, principal, workspace, scope_binding_ref,
             installation_id, dedupe_key, source_event_ref,
             source_event_revision, source_identity_digest, source_entity_name,
             source_record_id, proposal_expires_at, proposal_json,
             invalidation_json, delivery_state, available_at, created_at
         ) VALUES (?1, 'invalidation', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                   ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                   'pending', ?21, ?21)",
        params![
            event_id,
            proposal.header.proposal_id,
            revision,
            proposal.proposal_digest,
            invalidation.invalidation_id,
            invalidation.invalidation_digest,
            payload_digest,
            scope.principal.as_str(),
            scope.workspace.as_str(),
            scope_binding_ref,
            proposal.header.installation_id,
            proposal.header.dedupe_key,
            invalidation.source_event_ref,
            source_revision,
            invalidation.source_identity_digest,
            source.entity_name,
            source.record_id,
            expires_at,
            proposal_json,
            invalidation_json,
            now_text,
        ],
    )?;
    transaction.execute(
        "UPDATE app_retrieval_projection_heads SET lifecycle_state='invalidating',
                latest_event_id=?1, latest_invalidation_id=?2,
                latest_invalidation_digest=?3, source_event_revision=?4,
                updated_at=?5
          WHERE installation_id=?6 AND dedupe_key=?7",
        params![
            event_id,
            invalidation.invalidation_id,
            invalidation.invalidation_digest,
            source_revision,
            now_text,
            proposal.header.installation_id,
            proposal.header.dedupe_key,
        ],
    )?;
    validate_v18_storage_accounting(transaction)?;
    Ok(AppRetrievalOutboxAppendOutcome::Appended)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_retrieval_source_invalidation_in_transaction(
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
) -> Result<Option<AppRetrievalOutboxAppendOutcome>, AppContributionError> {
    append_retrieval_record_invalidations_inner(
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

/// Atomically invalidate every personal-agent retrieval projection whose sole
/// retained source is the exact record being physically forgotten.
pub fn append_retrieval_record_forget_invalidations_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    installation_id: &str,
    entity_name: &str,
    record_id: &str,
    source_event_revision: u64,
    now: &DateTime<Utc>,
) -> Result<Option<AppRetrievalOutboxAppendOutcome>, AppContributionError> {
    append_retrieval_record_invalidations_inner(
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
fn append_retrieval_record_invalidations_inner(
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
) -> Result<Option<AppRetrievalOutboxAppendOutcome>, AppContributionError> {
    let (expected_workflow, expected_action, expected_port_id) = expected_port
        .map(|(workflow, action, port)| (Some(workflow), Some(action), Some(port)))
        .unwrap_or((None, None, None));
    let limit = MAX_SOURCE_HEADS.saturating_add(1);
    let mut statement = transaction.prepare(
        "SELECT proposal_json FROM app_retrieval_projection_heads
          WHERE installation_id=?1 AND principal=?2 AND workspace=?3
            AND scope_binding_ref=?4 AND source_entity_name=?5
            AND source_record_id=?6 AND proposal_json IS NOT NULL
            AND lifecycle_state IN ('pending','live','invalidating')
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
    if i64::try_from(candidates.len()).unwrap_or(i64::MAX) > MAX_SOURCE_HEADS {
        return Err(AppContributionError::Quota(
            "source invalidation retrieval heads",
        ));
    }
    let mut outcome = None;
    for proposal_json in candidates {
        let proposal: AppPersonalAgentRetrievalProjectionProposalV1 =
            serde_json::from_slice(&proposal_json)?;
        proposal.validate()?;
        if let Some((workflow_id, action_id, contribution_port_id)) = expected_port {
            if proposal.header.workflow_id != workflow_id
                || proposal.header.action_id != action_id
                || proposal.header.contribution_port_id != contribution_port_id
            {
                return Err(AppContributionError::SubstitutedReplay);
            }
        }
        let source = exact_source(&proposal)?;
        if source.entity_name != entity_name || source.record_id != record_id {
            return Err(invalid_control(
                "retrieval source invalidation did not advance its exact record",
            ));
        }
        if source_event_revision < source.record_revision {
            return Err(AppContributionError::HistoryCompacted);
        }
        if source_event_revision == source.record_revision {
            // Exact terminal publication replay now sees the newly published
            // head; the predecessor invalidation is already ordered before it.
            continue;
        }
        let invalidation = source_invalidation_for_retrieval_proposal(
            &proposal,
            scope_binding_ref,
            source_event_revision,
            reason,
            now,
        )?;
        outcome = Some(append_retrieval_invalidation_in_transaction(
            transaction,
            scope,
            scope_binding_ref,
            proposal,
            invalidation,
            now,
        )?);
    }
    Ok(outcome)
}

pub fn append_retrieval_installation_invalidations_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    installation_id: &str,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<usize, AppContributionError> {
    let limit = MAX_SOURCE_HEADS.saturating_add(1);
    let mut statement = transaction.prepare(
        "SELECT scope_binding_ref, proposal_json
           FROM app_retrieval_projection_heads
          WHERE installation_id=?1 AND principal=?2 AND workspace=?3
            AND proposal_json IS NOT NULL
            AND lifecycle_state IN ('pending','live')
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
    if i64::try_from(candidates.len()).unwrap_or(i64::MAX) > MAX_SOURCE_HEADS {
        return Err(AppContributionError::Quota(
            "installation retrieval invalidation source heads",
        ));
    }
    let mut appended = 0usize;
    for (scope_binding_ref, proposal_json) in candidates {
        let proposal: AppPersonalAgentRetrievalProjectionProposalV1 =
            serde_json::from_slice(&proposal_json)?;
        proposal.validate()?;
        if proposal.header.installation_id != installation_id
            || proposal.header.scope_binding_ref != scope_binding_ref
        {
            return Err(AppContributionError::SubstitutedReplay);
        }
        let source = exact_source(&proposal)?;
        let invalidation = source_invalidation_for_retrieval_proposal(
            &proposal,
            &scope_binding_ref,
            source.record_revision,
            reason,
            now,
        )?;
        append_retrieval_invalidation_in_transaction(
            transaction,
            scope,
            &scope_binding_ref,
            proposal,
            invalidation,
            now,
        )?;
        appended = appended.saturating_add(1);
    }
    Ok(appended)
}

fn source_invalidation_for_retrieval_proposal(
    proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
    scope_binding_ref: &str,
    source_event_revision: u64,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<AppMemoryInvalidationV1, AppContributionError> {
    let source = exact_source(proposal)?;
    let identity = serde_json::to_vec(&serde_json::json!({
        "schema": "magician.app-source-invalidation-id.v1",
        "destination": "personal_agent_retrieval",
        "proposal_digest": &proposal.proposal_digest,
        "source_event_revision": source_event_revision,
        "reason": reason,
    }))?;
    Ok(AppMemoryInvalidationV1 {
        contract_version: magician_app_contract::contribution::APP_CONTRIBUTION_CONTRACT_VERSION,
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

fn terminalize_pending_proposal_before_invalidation(
    transaction: &Transaction<'_>,
    proposal_event_id: &str,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let inserted = transaction.execute(
        "INSERT INTO app_retrieval_delivery_terminal(
             sequence, event_id, event_kind, proposal_id, proposal_revision,
             proposal_digest, invalidation_id, invalidation_digest,
             payload_digest, principal, workspace, scope_binding_ref,
             installation_id, dedupe_key, source_event_ref,
             source_event_revision, source_identity_digest, payload_json,
             terminal_state, terminalized_at
         )
         SELECT sequence, event_id, event_kind, proposal_id, proposal_revision,
                proposal_digest, invalidation_id, invalidation_digest,
                payload_digest, principal, workspace, scope_binding_ref,
                installation_id, dedupe_key, source_event_ref,
                source_event_revision, source_identity_digest, proposal_json,
                'invalidated_before_delivery', ?1
           FROM app_retrieval_delivery_outbox
          WHERE event_id=?2 AND event_kind='proposal' AND delivery_state='pending'
            AND dispatch_started=0",
        params![timestamp(now), proposal_event_id],
    )?;
    if inserted == 1 {
        let deleted = transaction.execute(
            "DELETE FROM app_retrieval_delivery_outbox
              WHERE event_id=?1 AND event_kind='proposal' AND delivery_state='pending'
                AND dispatch_started=0",
            [proposal_event_id],
        )?;
        if deleted != 1 {
            return Err(invalid_control(
                "retrieval pending proposal cancellation lost its exact row",
            ));
        }
    }
    Ok(())
}

impl AppRegistryService {
    pub(crate) async fn claim_retrieval_delivery_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: String,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Option<AppRetrievalDeliveryLease>, AppContributionError> {
        validate_lease_request(&lease_owner, lease_duration)?;
        let expected_scope = (
            authenticated.scope().principal.as_str().to_owned(),
            authenticated.scope().workspace.as_str().to_owned(),
            authenticated.scope_binding_ref().as_str().to_owned(),
        );
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            claim_one(
                connection,
                expected_scope,
                lease_owner,
                lease_duration,
                &now,
            )
        })
        .await
    }

    pub(crate) async fn begin_retrieval_delivery_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppRetrievalDeliveryLease,
        now: DateTime<Utc>,
    ) -> Result<AppRetrievalDeliveryDispatchPermit, AppContributionError> {
        ensure_scope(
            authenticated,
            &lease.principal,
            &lease.workspace,
            &lease.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            begin_dispatch(connection, lease, &now)
        })
        .await
    }

    pub(crate) async fn release_retrieval_delivery_lease(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppRetrievalDeliveryLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        validate_retry_delay(retry_delay)?;
        ensure_scope(
            authenticated,
            &lease.principal,
            &lease.workspace,
            &lease.scope_binding_ref,
        )?;
        let release = ReleaseIdentity::from_lease(lease);
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            release_delivery(connection, release, retry_delay, &now)
        })
        .await
    }

    pub(crate) async fn release_retrieval_delivery_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: AppRetrievalDeliveryDispatchAck,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        validate_retry_delay(retry_delay)?;
        ensure_scope(
            authenticated,
            &dispatch.principal,
            &dispatch.workspace,
            &dispatch.scope_binding_ref,
        )?;
        let release = ReleaseIdentity::from_dispatch(dispatch);
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            release_delivery(connection, release, retry_delay, &now)
        })
        .await
    }

    pub(crate) async fn acknowledge_retrieval_delivery_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        permit: AppRetrievalSourceAckPermit,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        ensure_scope(
            authenticated,
            &permit.dispatch.principal,
            &permit.dispatch.workspace,
            &permit.dispatch.scope_binding_ref,
        )?;
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            acknowledge_delivery(connection, permit, &now)
        })
        .await
    }

    pub(crate) async fn audit_retrieval_destination_head(
        &self,
        authenticated: &AuthenticatedAppScope,
        actual: Option<PersonalAgentRetrievalExpectedHeadV1>,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            audit_destination_head(connection, actual)
        })
        .await
    }

    pub(crate) async fn compact_retrieval_delivery_journal(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppContributionError> {
        self.execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            prepare_expiration_rows(&transaction, &now)?;
            expire_pending_proposals(&transaction, &now)?;
            compact_terminals(&transaction, &now)?;
            validate_v18_storage_accounting(&transaction)?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }
}

#[derive(Debug)]
struct SourceHead {
    principal: String,
    workspace: String,
    scope_binding_ref: String,
    proposal_id: String,
    proposal_revision: u64,
    proposal_digest: String,
    proposal_payload_digest: String,
    source_event_ref: String,
    source_event_revision: u64,
    source_identity_digest: String,
    lifecycle_state: String,
    proposal_json: Option<Vec<u8>>,
}

impl SourceHead {
    fn matches_scope(&self, scope: &AppScope, scope_binding_ref: &str) -> bool {
        self.principal == scope.principal.as_str()
            && self.workspace == scope.workspace.as_str()
            && self.scope_binding_ref == scope_binding_ref
    }
}

fn load_head(
    transaction: &Transaction<'_>,
    installation_id: &str,
    dedupe_key: &str,
) -> Result<Option<SourceHead>, AppContributionError> {
    transaction
        .query_row(
            "SELECT principal, workspace, scope_binding_ref, proposal_id,
                    proposal_revision, proposal_digest, proposal_payload_digest,
                    source_event_ref, source_event_revision, source_identity_digest,
                    lifecycle_state, proposal_json
               FROM app_retrieval_projection_heads
              WHERE installation_id=?1 AND dedupe_key=?2",
            params![installation_id, dedupe_key],
            |row| {
                let proposal_revision = row.get::<_, i64>(4)?;
                let source_event_revision = row.get::<_, i64>(8)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    proposal_revision,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    source_event_revision,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<Vec<u8>>>(11)?,
                ))
            },
        )
        .optional()?
        .map(|row| {
            Ok(SourceHead {
                principal: row.0,
                workspace: row.1,
                scope_binding_ref: row.2,
                proposal_id: row.3,
                proposal_revision: to_u64(row.4, "stored proposal revision")?,
                proposal_digest: row.5,
                proposal_payload_digest: row.6,
                source_event_ref: row.7,
                source_event_revision: to_u64(row.8, "stored source revision")?,
                source_identity_digest: row.9,
                lifecycle_state: row.10,
                proposal_json: row.11,
            })
        })
        .transpose()
}

#[allow(clippy::too_many_arguments)]
fn exact_replay(
    transaction: &Transaction<'_>,
    event_id: &str,
    kind: AppRetrievalDeliveryKind,
    proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
    invalidation: Option<&AppMemoryInvalidationV1>,
    proposal_json: &[u8],
    invalidation_json: Option<&[u8]>,
    payload_digest: &str,
    scope: &AppScope,
    scope_binding_ref: &str,
) -> Result<Option<AppRetrievalOutboxAppendOutcome>, AppContributionError> {
    let invalidation_id = invalidation.map(|value| value.invalidation_id.as_str());
    let pending = transaction
        .query_row(
            "SELECT event_id, event_kind, proposal_digest, invalidation_id,
                    invalidation_digest, payload_digest, principal, workspace,
                    scope_binding_ref, proposal_json, invalidation_json
               FROM app_retrieval_delivery_outbox
              WHERE event_id=?1 OR
                    (installation_id=?2 AND proposal_id=?3 AND proposal_revision=?4
                     AND event_kind=?5) OR
                    (?6 IS NOT NULL AND installation_id=?2 AND invalidation_id=?6)",
            params![
                event_id,
                proposal.header.installation_id,
                proposal.header.proposal_id,
                to_i64(proposal.header.proposal_revision, "proposal revision")?,
                kind.as_str(),
                invalidation_id,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                    row.get::<_, Option<Vec<u8>>>(10)?,
                ))
            },
        )
        .optional()?;
    if let Some(row) = pending {
        let exact = row.0 == event_id
            && row.1 == kind.as_str()
            && row.2 == proposal.proposal_digest
            && row.3.as_deref() == invalidation_id
            && row.4.as_deref() == invalidation.map(|value| value.invalidation_digest.as_str())
            && row.5 == payload_digest
            && row.6 == scope.principal.as_str()
            && row.7 == scope.workspace.as_str()
            && row.8 == scope_binding_ref
            && row.9 == proposal_json
            && row.10.as_deref() == invalidation_json;
        return if exact {
            Ok(Some(AppRetrievalOutboxAppendOutcome::ExactReplay))
        } else {
            Err(AppContributionError::SubstitutedReplay)
        };
    }
    let terminal = transaction
        .query_row(
            "SELECT event_id, event_kind, proposal_digest, invalidation_id,
                    invalidation_digest, payload_digest, principal, workspace,
                    scope_binding_ref, payload_json
               FROM app_retrieval_delivery_terminal
              WHERE event_id=?1 OR
                    (installation_id=?2 AND proposal_id=?3 AND proposal_revision=?4
                     AND event_kind=?5) OR
                    (?6 IS NOT NULL AND installation_id=?2 AND invalidation_id=?6)",
            params![
                event_id,
                proposal.header.installation_id,
                proposal.header.proposal_id,
                to_i64(proposal.header.proposal_revision, "proposal revision")?,
                kind.as_str(),
                invalidation_id,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                ))
            },
        )
        .optional()?;
    if let Some(row) = terminal {
        let expected_payload = event_payload(kind, proposal_json, invalidation_json)?;
        let exact = row.0 == event_id
            && row.1 == kind.as_str()
            && row.2 == proposal.proposal_digest
            && row.3.as_deref() == invalidation_id
            && row.4.as_deref() == invalidation.map(|value| value.invalidation_digest.as_str())
            && row.5 == payload_digest
            && row.6 == scope.principal.as_str()
            && row.7 == scope.workspace.as_str()
            && row.8 == scope_binding_ref;
        if !exact {
            return Err(AppContributionError::SubstitutedReplay);
        }
        return match row.9 {
            Some(bytes) if bytes == expected_payload => {
                Ok(Some(AppRetrievalOutboxAppendOutcome::ExactReplay))
            },
            Some(_) => Err(AppContributionError::SubstitutedReplay),
            None => Err(AppContributionError::HistoryCompacted),
        };
    }
    Ok(None)
}

fn claim_one(
    connection: &mut Connection,
    expected_scope: (String, String, String),
    lease_owner: String,
    lease_duration: StdDuration,
    now: &DateTime<Utc>,
) -> Result<Option<AppRetrievalDeliveryLease>, AppContributionError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    prepare_expiration_rows(&transaction, now)?;
    expire_pending_proposals(&transaction, now)?;
    compact_terminals(&transaction, now)?;
    validate_v18_storage_accounting(&transaction)?;
    let now_text = timestamp(now);
    let row = transaction
        .query_row(
            "SELECT sequence, event_id, event_kind, proposal_json,
                    invalidation_json, payload_digest, principal, workspace,
                    scope_binding_ref, attempt_count, lease_epoch,
                    expected_destination_generation,
                    expected_destination_receipt_digest, dispatch_started
               FROM app_retrieval_delivery_outbox
              WHERE sequence=(SELECT MIN(sequence) FROM app_retrieval_delivery_outbox)
                AND ((delivery_state='pending' AND available_at<=?1)
                 OR (delivery_state IN ('leased','dispatching') AND lease_expires_at<=?1))
              ORDER BY sequence ASC LIMIT 1",
            [&now_text],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        transaction.commit()?;
        return Ok(None);
    };
    if row.6 != expected_scope.0 || row.7 != expected_scope.1 || row.8 != expected_scope.2 {
        return Err(invalid_control(
            "retrieval outbox row escaped its registry scope",
        ));
    }
    let kind = AppRetrievalDeliveryKind::parse(&row.2)?;
    let proposal: AppPersonalAgentRetrievalProjectionProposalV1 = serde_json::from_slice(&row.3)?;
    proposal.validate()?;
    let invalidation = row
        .4
        .as_deref()
        .map(serde_json::from_slice::<AppMemoryInvalidationV1>)
        .transpose()?;
    if let Some(value) = invalidation.as_ref() {
        value.validate()?;
    }
    if (kind == AppRetrievalDeliveryKind::Invalidation) != invalidation.is_some()
        || content_digest(&event_payload(kind, &row.3, row.4.as_deref())?) != row.5
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    let lease_token = Uuid::new_v4().to_string();
    let lease_expires_at = *now
        + Duration::from_std(lease_duration)
            .map_err(|_| invalid_control("retrieval lease duration overflow"))?;
    let lease_epoch = to_u64(row.10, "lease epoch")?
        .checked_add(1)
        .ok_or_else(|| invalid_control("retrieval lease epoch overflow"))?;
    let changed = transaction.execute(
        "UPDATE app_retrieval_delivery_outbox
            SET delivery_state='leased', lease_owner=?1, lease_token=?2,
                lease_expires_at=?3, attempt_count=attempt_count+1,
                lease_epoch=?4
          WHERE sequence=?5 AND event_id=?6 AND
                ((delivery_state='pending' AND available_at<=?7) OR
                 (delivery_state IN ('leased','dispatching') AND lease_expires_at<=?7))",
        params![
            lease_owner,
            lease_token,
            timestamp(&lease_expires_at),
            to_i64(lease_epoch, "lease epoch")?,
            row.0,
            row.1,
            now_text,
        ],
    )?;
    if changed != 1 {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    transaction.commit()?;
    Ok(Some(AppRetrievalDeliveryLease {
        sequence: row.0,
        event_id: row.1,
        kind,
        proposal,
        proposal_json: row.3,
        invalidation,
        invalidation_json: row.4,
        payload_digest: row.5,
        principal: row.6,
        workspace: row.7,
        scope_binding_ref: row.8,
        lease_owner,
        lease_token,
        lease_expires_at,
        lease_epoch,
        attempt_count: to_u32(row.9.saturating_add(1), "attempt count")?,
        dispatch_started: row.13 == 1,
        expected_destination_head: paired_head(row.11, row.12)?,
    }))
}

fn begin_dispatch(
    connection: &mut Connection,
    mut lease: AppRetrievalDeliveryLease,
    now: &DateTime<Utc>,
) -> Result<AppRetrievalDeliveryDispatchPermit, AppContributionError> {
    if *now >= lease.lease_expires_at {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_high_water = destination_high_water(&transaction)?;
    if let Some(retained) = lease.expected_destination_head.as_ref() {
        if current_high_water.as_ref() != Some(retained) {
            return Err(invalid_control(
                "retrieval destination acknowledgement advanced past an unacknowledged dispatch",
            ));
        }
    } else {
        lease.expected_destination_head = current_high_water;
    }
    let (expected_generation, expected_digest) =
        split_head(lease.expected_destination_head.as_ref())?;
    let changed = transaction.execute(
        "UPDATE app_retrieval_delivery_outbox
            SET delivery_state='dispatching',
                dispatch_started=1,
                expected_destination_generation=?1,
                expected_destination_receipt_digest=?2
          WHERE sequence=?3 AND event_id=?4 AND delivery_state='leased'
            AND lease_owner=?5 AND lease_token=?6 AND lease_epoch=?7
            AND lease_expires_at=?8 AND lease_expires_at>?9
            AND (expected_destination_generation IS NULL OR
                 (expected_destination_generation=?1 AND
                  expected_destination_receipt_digest=?2))",
        params![
            expected_generation,
            expected_digest,
            lease.sequence,
            lease.event_id,
            lease.lease_owner,
            lease.lease_token,
            to_i64(lease.lease_epoch, "lease epoch")?,
            timestamp(&lease.lease_expires_at),
            timestamp(now),
        ],
    )?;
    if changed != 1 {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    transaction.commit()?;
    let absolute_expires_at_ms = if lease.kind == AppRetrievalDeliveryKind::Proposal {
        lease
            .lease_expires_at
            .timestamp_millis()
            .min(lease.proposal.header.expires_at_ms)
    } else {
        lease.lease_expires_at.timestamp_millis()
    };
    let dispatch_ack = AppRetrievalDeliveryDispatchAck {
        sequence: lease.sequence,
        event_id: lease.event_id,
        kind: lease.kind,
        proposal_json: lease.proposal_json,
        invalidation_json: lease.invalidation_json,
        payload_digest: lease.payload_digest,
        principal: lease.principal,
        workspace: lease.workspace,
        scope_binding_ref: lease.scope_binding_ref,
        lease_owner: lease.lease_owner,
        lease_token: lease.lease_token,
        lease_expires_at: lease.lease_expires_at,
        lease_epoch: lease.lease_epoch,
        expected_destination_head: lease.expected_destination_head.clone(),
    };
    Ok(AppRetrievalDeliveryDispatchPermit {
        evidence: AppRetrievalDeliveryDispatchEvidence {
            proposal: lease.proposal,
            invalidation: lease.invalidation,
            kind: lease.kind,
            expected_destination_head: lease.expected_destination_head,
            dispatch_ack,
            absolute_expires_at_ms,
            attempt_count: lease.attempt_count,
        },
    })
}

fn acknowledge_delivery(
    connection: &mut Connection,
    permit: AppRetrievalSourceAckPermit,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let AppRetrievalSourceAckPermit { receipt, dispatch } = permit;
    if receipt.recorded_at_ms <= 0
        || receipt.recorded_at_ms > now.timestamp_millis().saturating_add(60_000)
        || receipt.generation
            != dispatch
                .expected_destination_head
                .as_ref()
                .map_or(1, |head| head.generation.saturating_add(1))
        || receipt.previous_receipt_digest
            != dispatch
                .expected_destination_head
                .as_ref()
                .map(|head| head.receipt_digest.clone())
    {
        return Err(invalid_control(
            "retrieval destination receipt does not extend the dispatched predecessor",
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = transaction
        .query_row(
            "SELECT event_kind, proposal_id, proposal_revision, proposal_digest,
                    invalidation_id, invalidation_digest, payload_digest,
                    installation_id, dedupe_key, source_event_ref,
                    source_event_revision, source_identity_digest, proposal_json,
                    invalidation_json, principal, workspace, scope_binding_ref
               FROM app_retrieval_delivery_outbox
              WHERE sequence=?1 AND event_id=?2 AND delivery_state='dispatching'
                AND lease_owner=?3 AND lease_token=?4 AND lease_epoch=?5
                AND lease_expires_at=?6",
            params![
                dispatch.sequence,
                dispatch.event_id,
                dispatch.lease_owner,
                dispatch.lease_token,
                to_i64(dispatch.lease_epoch, "lease epoch")?,
                timestamp(&dispatch.lease_expires_at),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Vec<u8>>(12)?,
                    row.get::<_, Option<Vec<u8>>>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppRegistryError::OutboxLeaseStale)?;
    if row.0 != dispatch.kind.as_str()
        || row.6 != dispatch.payload_digest
        || row.12 != dispatch.proposal_json
        || row.13 != dispatch.invalidation_json
        || row.14 != dispatch.principal
        || row.15 != dispatch.workspace
        || row.16 != dispatch.scope_binding_ref
    {
        return Err(AppContributionError::SubstitutedReplay);
    }
    if destination_high_water(&transaction)? != dispatch.expected_destination_head {
        return Err(invalid_control(
            "retrieval source acknowledgement high-water changed during dispatch",
        ));
    }
    let payload_json = event_payload(dispatch.kind, &row.12, row.13.as_deref())?;
    let invalidation_disposition = receipt
        .invalidation_disposition
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    transaction.execute(
        "INSERT INTO app_retrieval_delivery_terminal(
             sequence, event_id, event_kind, proposal_id, proposal_revision,
             proposal_digest, invalidation_id, invalidation_digest,
             payload_digest, principal, workspace, scope_binding_ref,
             installation_id, dedupe_key, source_event_ref,
             source_event_revision, source_identity_digest, payload_json,
             terminal_state, source_ack_receipt_id,
             source_ack_owner_identity_digest, source_ack_operation_digest,
             destination_generation, destination_receipt_digest,
             resulting_projection_digest, destination_recorded_at_ms,
             invalidation_disposition,
             terminalized_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, ?17, ?18, 'delivered', ?19, ?20, ?21,
                   ?22, ?23, ?24, ?25, ?26, ?27)",
        params![
            dispatch.sequence,
            dispatch.event_id,
            row.0,
            row.1,
            row.2,
            row.3,
            row.4,
            row.5,
            row.6,
            row.14,
            row.15,
            row.16,
            row.7,
            row.8,
            row.9,
            row.10,
            row.11,
            payload_json,
            receipt.receipt_id,
            receipt.owner_identity_digest,
            receipt.operation_digest,
            to_i64(receipt.generation, "destination generation")?,
            receipt.receipt_digest,
            receipt.resulting_projection_digest,
            receipt.recorded_at_ms,
            invalidation_disposition,
            timestamp(now),
        ],
    )?;
    transaction.execute(
        "INSERT INTO app_retrieval_destination_ack_high_water(
             singleton, destination_generation, destination_receipt_digest, updated_at
         ) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(singleton) DO UPDATE SET
             destination_generation=excluded.destination_generation,
             destination_receipt_digest=excluded.destination_receipt_digest,
             updated_at=excluded.updated_at",
        params![
            to_i64(receipt.generation, "destination generation")?,
            receipt.receipt_digest,
            timestamp(now),
        ],
    )?;
    let lifecycle = if dispatch.kind == AppRetrievalDeliveryKind::Proposal {
        "live"
    } else {
        "settled"
    };
    transaction.execute(
        "UPDATE app_retrieval_projection_heads SET lifecycle_state=?1,
                destination_generation=?2, destination_receipt_digest=?3,
                updated_at=?4
          WHERE installation_id=?5 AND dedupe_key=?6 AND latest_event_id=?7",
        params![
            lifecycle,
            to_i64(receipt.generation, "destination generation")?,
            receipt.receipt_digest,
            timestamp(now),
            row.7,
            row.8,
            dispatch.event_id,
        ],
    )?;
    transaction.execute(
        "DELETE FROM app_retrieval_delivery_outbox WHERE sequence=?1 AND event_id=?2",
        params![dispatch.sequence, dispatch.event_id],
    )?;
    compact_terminals(&transaction, now)?;
    validate_v18_storage_accounting(&transaction)?;
    transaction.commit()?;
    Ok(())
}

#[derive(Debug)]
struct ReleaseIdentity {
    sequence: i64,
    event_id: String,
    lease_owner: String,
    lease_token: String,
    lease_expires_at: DateTime<Utc>,
    lease_epoch: u64,
}

impl ReleaseIdentity {
    fn from_lease(lease: AppRetrievalDeliveryLease) -> Self {
        Self {
            sequence: lease.sequence,
            event_id: lease.event_id,
            lease_owner: lease.lease_owner,
            lease_token: lease.lease_token,
            lease_expires_at: lease.lease_expires_at,
            lease_epoch: lease.lease_epoch,
        }
    }

    fn from_dispatch(dispatch: AppRetrievalDeliveryDispatchAck) -> Self {
        Self {
            sequence: dispatch.sequence,
            event_id: dispatch.event_id,
            lease_owner: dispatch.lease_owner,
            lease_token: dispatch.lease_token,
            lease_expires_at: dispatch.lease_expires_at,
            lease_epoch: dispatch.lease_epoch,
        }
    }
}

fn release_delivery(
    connection: &mut Connection,
    release: ReleaseIdentity,
    retry_delay: StdDuration,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let available_at = *now
        + Duration::from_std(retry_delay)
            .map_err(|_| invalid_control("retrieval retry delay overflow"))?;
    let changed = connection.execute(
        "UPDATE app_retrieval_delivery_outbox
            SET delivery_state='pending', available_at=?1, lease_owner=NULL,
                lease_token=NULL, lease_expires_at=NULL
          WHERE sequence=?2 AND event_id=?3
            AND delivery_state IN ('leased','dispatching')
            AND lease_owner=?4 AND lease_token=?5 AND lease_epoch=?6
            AND lease_expires_at=?7",
        params![
            timestamp(&available_at),
            release.sequence,
            release.event_id,
            release.lease_owner,
            release.lease_token,
            to_i64(release.lease_epoch, "lease epoch")?,
            timestamp(&release.lease_expires_at),
        ],
    )?;
    if changed != 1 {
        return Err(AppRegistryError::OutboxLeaseStale.into());
    }
    Ok(())
}

fn prepare_expiration_rows(
    transaction: &Transaction<'_>,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let now_text = timestamp(now);
    let mut statement = transaction.prepare(
        "SELECT installation_id, dedupe_key, principal, workspace,
                scope_binding_ref, proposal_id, proposal_revision,
                proposal_digest, source_event_ref, source_event_revision,
                source_identity_digest, source_entity_name, source_record_id,
                proposal_expires_at, proposal_json
           FROM app_retrieval_projection_heads
          WHERE lifecycle_state='live' AND proposal_expires_at<=?1
          ORDER BY updated_at, installation_id, dedupe_key LIMIT 64",
    )?;
    let rows = statement
        .query_map([&now_text], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, Option<Vec<u8>>>(14)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for row in rows {
        let proposal_json = row.14.ok_or(AppContributionError::HistoryCompacted)?;
        let proposal: AppPersonalAgentRetrievalProjectionProposalV1 =
            serde_json::from_slice(&proposal_json)?;
        proposal.validate()?;
        let event_id = expiration_event_id(&proposal);
        let payload_digest = content_digest(&event_payload(
            AppRetrievalDeliveryKind::Expiration,
            &proposal_json,
            None,
        )?);
        transaction.execute(
            "INSERT OR IGNORE INTO app_retrieval_delivery_outbox(
                 event_id, event_kind, proposal_id, proposal_revision,
                 proposal_digest, payload_digest, principal, workspace,
                 scope_binding_ref, installation_id, dedupe_key, source_event_ref,
                 source_event_revision, source_identity_digest, source_entity_name,
                 source_record_id, proposal_expires_at, proposal_json,
                 delivery_state, available_at, created_at
             ) VALUES (?1, 'expiration', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                       ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, 'pending', ?18, ?18)",
            params![
                event_id,
                row.5,
                row.6,
                row.7,
                payload_digest,
                row.2,
                row.3,
                row.4,
                row.0,
                row.1,
                row.8,
                row.9,
                row.10,
                row.11,
                row.12,
                row.13,
                proposal_json,
                now_text,
            ],
        )?;
        transaction.execute(
            "UPDATE app_retrieval_projection_heads
                SET lifecycle_state='expiring', latest_event_id=?1, updated_at=?2
              WHERE installation_id=?3 AND dedupe_key=?4 AND lifecycle_state='live'",
            params![event_id, now_text, row.0, row.1],
        )?;
    }
    Ok(())
}

fn expire_pending_proposals(
    transaction: &Transaction<'_>,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let now_text = timestamp(now);
    let mut statement = transaction.prepare(
        "SELECT sequence, event_id, event_kind, proposal_id,
                proposal_revision, proposal_digest, invalidation_id,
                invalidation_digest, payload_digest, principal, workspace,
                scope_binding_ref, installation_id, dedupe_key,
                source_event_ref, source_event_revision, source_identity_digest,
                proposal_json, invalidation_json
           FROM app_retrieval_delivery_outbox
          WHERE event_kind='proposal' AND delivery_state='pending'
            AND proposal_expires_at<=?1
          ORDER BY sequence LIMIT 64",
    )?;
    let rows = statement
        .query_map([&now_text], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, i64>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, Vec<u8>>(17)?,
                row.get::<_, Option<Vec<u8>>>(18)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for row in rows {
        let payload_json = event_payload(
            AppRetrievalDeliveryKind::Proposal,
            &row.17,
            row.18.as_deref(),
        )?;
        transaction.execute(
            "INSERT INTO app_retrieval_delivery_terminal(
                 sequence, event_id, event_kind, proposal_id, proposal_revision,
                 proposal_digest, invalidation_id, invalidation_digest,
                 payload_digest, principal, workspace, scope_binding_ref,
                 installation_id, dedupe_key, source_event_ref,
                 source_event_revision, source_identity_digest, payload_json,
                 terminal_state, terminalized_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       ?13, ?14, ?15, ?16, ?17, ?18, 'expired_before_delivery', ?19)",
            params![
                row.0,
                row.1,
                row.2,
                row.3,
                row.4,
                row.5,
                row.6,
                row.7,
                row.8,
                row.9,
                row.10,
                row.11,
                row.12,
                row.13,
                row.14,
                row.15,
                row.16,
                payload_json,
                now_text,
            ],
        )?;
        transaction.execute(
            "DELETE FROM app_retrieval_delivery_outbox WHERE sequence=?1 AND event_id=?2",
            params![row.0, row.1],
        )?;
        transaction.execute(
            "UPDATE app_retrieval_projection_heads
                SET lifecycle_state='settled', updated_at=?1
              WHERE installation_id=?2 AND dedupe_key=?3 AND latest_event_id=?4",
            params![now_text, row.12, row.13, row.1],
        )?;
    }
    Ok(())
}

fn compact_terminals(
    transaction: &Transaction<'_>,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let cutoff = transaction
        .query_row(
            "SELECT sequence FROM app_retrieval_delivery_terminal
              ORDER BY sequence DESC LIMIT 1 OFFSET ?1",
            [MAX_RECENT_TERMINALS],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(cutoff) = cutoff else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO app_retrieval_compaction_high_water(
             singleton, compacted_through_sequence, compacted_at
         ) VALUES (1, ?1, ?2)
         ON CONFLICT(singleton) DO UPDATE SET
             compacted_through_sequence=MAX(
                 compacted_through_sequence, excluded.compacted_through_sequence
             ), compacted_at=excluded.compacted_at",
        params![cutoff, timestamp(now)],
    )?;
    transaction.execute(
        "DELETE FROM app_retrieval_delivery_terminal WHERE sequence<=?1",
        [cutoff],
    )?;
    Ok(())
}

fn audit_destination_head(
    connection: &mut Connection,
    actual: Option<PersonalAgentRetrievalExpectedHeadV1>,
) -> Result<(), AppContributionError> {
    let expected = destination_high_water_connection(connection)?;
    if actual == expected {
        return Ok(());
    }
    if let (Some(actual), Some(expected)) = (actual.as_ref(), expected.as_ref()) {
        if actual.generation <= expected.generation {
            return Err(invalid_control(
                "personal-agent retrieval destination rolled back behind its acknowledged source \
                 high-water",
            ));
        }
    }
    let response_lost = connection
        .query_row(
            "SELECT expected_destination_generation,
                    expected_destination_receipt_digest
               FROM app_retrieval_delivery_outbox
              WHERE delivery_state='dispatching'
              ORDER BY sequence ASC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()?
        .map(|row| paired_head(row.0, row.1))
        .transpose()?
        .is_some_and(|predecessor| {
            predecessor == expected
                && actual.as_ref().is_some_and(|head| {
                    head.generation == predecessor.as_ref().map_or(1, |old| old.generation + 1)
                })
        });
    if response_lost {
        Ok(())
    } else {
        Err(invalid_control(
            "personal-agent retrieval destination head diverges from the source acknowledgement \
             chain",
        ))
    }
}

fn destination_high_water(
    transaction: &Transaction<'_>,
) -> Result<Option<PersonalAgentRetrievalExpectedHeadV1>, AppContributionError> {
    let row = transaction
        .query_row(
            "SELECT destination_generation, destination_receipt_digest
               FROM app_retrieval_destination_ack_high_water WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|row| {
        Ok(PersonalAgentRetrievalExpectedHeadV1 {
            generation: to_u64(row.0, "destination generation")?,
            receipt_digest: row.1,
        })
    })
    .transpose()
}

fn destination_high_water_connection(
    connection: &Connection,
) -> Result<Option<PersonalAgentRetrievalExpectedHeadV1>, AppContributionError> {
    let row = connection
        .query_row(
            "SELECT destination_generation, destination_receipt_digest
               FROM app_retrieval_destination_ack_high_water WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    row.map(|row| {
        Ok(PersonalAgentRetrievalExpectedHeadV1 {
            generation: to_u64(row.0, "destination generation")?,
            receipt_digest: row.1,
        })
    })
    .transpose()
}

fn paired_head(
    generation: Option<i64>,
    digest: Option<String>,
) -> Result<Option<PersonalAgentRetrievalExpectedHeadV1>, AppContributionError> {
    match (generation, digest) {
        (None, None) => Ok(None),
        (Some(generation), Some(receipt_digest)) => {
            Ok(Some(PersonalAgentRetrievalExpectedHeadV1 {
                generation: to_u64(generation, "expected destination generation")?,
                receipt_digest,
            }))
        },
        _ => Err(invalid_control("partial retrieval destination predecessor")),
    }
}

fn split_head(
    head: Option<&PersonalAgentRetrievalExpectedHeadV1>,
) -> Result<(Option<i64>, Option<&str>), AppContributionError> {
    match head {
        Some(head) => Ok((
            Some(to_i64(head.generation, "expected destination generation")?),
            Some(head.receipt_digest.as_str()),
        )),
        None => Ok((None, None)),
    }
}

fn validate_v18_storage_accounting(
    transaction: &Transaction<'_>,
) -> Result<(), AppContributionError> {
    for (table, ceiling) in [
        ("app_retrieval_delivery_outbox", MAX_OUTBOX_ROWS),
        ("app_retrieval_projection_heads", MAX_SOURCE_HEADS),
        ("app_retrieval_delivery_terminal", MAX_RECENT_TERMINALS),
    ] {
        let count: i64 =
            transaction.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        if count < 0 || count > ceiling {
            return Err(AppContributionError::Quota(
                "bounded V18 retrieval journal accounting",
            ));
        }
    }
    let bytes: i64 = transaction.query_row(
        "SELECT COALESCE(SUM(length(proposal_json) +
                    COALESCE(length(invalidation_json), 0)), 0)
           FROM app_retrieval_delivery_outbox",
        [],
        |row| row.get(0),
    )?;
    if bytes < 0 || bytes > MAX_OUTBOX_BYTES {
        return Err(AppContributionError::Quota(
            "bounded V18 retrieval journal bytes",
        ));
    }
    Ok(())
}

fn enforce_append_quota(
    transaction: &Transaction<'_>,
    installation_id: &str,
    incoming_bytes: usize,
) -> Result<(), AppContributionError> {
    let pending: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_retrieval_delivery_outbox WHERE installation_id=?1",
        [installation_id],
        |row| row.get(0),
    )?;
    if pending >= MAX_PENDING_PER_INSTALLATION {
        return Err(AppContributionError::Quota(
            "retrieval pending rows per installation",
        ));
    }
    let rows: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_retrieval_delivery_outbox",
        [],
        |row| row.get(0),
    )?;
    let bytes: i64 = transaction.query_row(
        "SELECT COALESCE(SUM(length(proposal_json) +
                    COALESCE(length(invalidation_json), 0)), 0)
           FROM app_retrieval_delivery_outbox",
        [],
        |row| row.get(0),
    )?;
    if rows >= MAX_OUTBOX_ROWS
        || bytes.saturating_add(i64::try_from(incoming_bytes).unwrap_or(i64::MAX))
            > MAX_OUTBOX_BYTES
    {
        return Err(AppContributionError::Quota(
            "retrieval source journal capacity",
        ));
    }
    Ok(())
}

fn event_payload(
    kind: AppRetrievalDeliveryKind,
    proposal_json: &[u8],
    invalidation_json: Option<&[u8]>,
) -> Result<Vec<u8>, AppContributionError> {
    let bytes = match kind {
        AppRetrievalDeliveryKind::Proposal => proposal_json.to_vec(),
        AppRetrievalDeliveryKind::Invalidation => {
            let proposal: AppPersonalAgentRetrievalProjectionProposalV1 =
                serde_json::from_slice(proposal_json)?;
            let invalidation: AppMemoryInvalidationV1 = serde_json::from_slice(
                invalidation_json
                    .ok_or_else(|| invalid_control("missing retrieval invalidation bytes"))?,
            )?;
            serde_json::to_vec(&InvalidationPayload {
                proposal: &proposal,
                invalidation: &invalidation,
            })?
        },
        AppRetrievalDeliveryKind::Expiration => {
            #[derive(Serialize)]
            struct ExpirationPayload<'a> {
                kind: &'static str,
                proposal_id: &'a str,
                proposal_digest: &'a str,
            }
            let proposal: AppPersonalAgentRetrievalProjectionProposalV1 =
                serde_json::from_slice(proposal_json)?;
            serde_json::to_vec(&ExpirationPayload {
                kind: "expiration",
                proposal_id: &proposal.header.proposal_id,
                proposal_digest: &proposal.proposal_digest,
            })?
        },
    };
    if bytes.len() > MAX_EVENT_PAYLOAD_BYTES {
        return Err(AppContributionError::Quota("retrieval event payload bytes"));
    }
    Ok(bytes)
}

fn exact_source(
    proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
) -> Result<&magician_app_contract::contribution::AppContributionSourceRefV1, AppContributionError>
{
    if proposal.header.sources.len() != 1 {
        return Err(invalid_control(
            "personal-agent retrieval V1 requires one exact source",
        ));
    }
    proposal
        .header
        .sources
        .first()
        .ok_or_else(|| invalid_control("retrieval proposal lost its exact source"))
}

fn validate_proposal_scope(
    scope: &AppScope,
    scope_binding_ref: &str,
    proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
    now: &DateTime<Utc>,
) -> Result<(), AppContributionError> {
    let source = exact_source(proposal)?;
    if scope.principal.as_str().is_empty()
        || scope.workspace.as_str().is_empty()
        || scope_binding_ref.is_empty()
        || proposal.header.scope_binding_ref != scope_binding_ref
        || source.installation_id != proposal.header.installation_id
        || proposal.header.issued_at_ms > now.timestamp_millis()
    {
        return Err(invalid_control("retrieval proposal scope is invalid"));
    }
    Ok(())
}

fn proposal_event_id(proposal: &AppPersonalAgentRetrievalProjectionProposalV1) -> String {
    digest_event_id(
        "proposal",
        &format!(
            "{}\0{}\0{}",
            proposal.header.proposal_id,
            proposal.header.proposal_revision,
            proposal.proposal_digest
        ),
    )
}

fn invalidation_event_id(invalidation: &AppMemoryInvalidationV1) -> String {
    digest_event_id(
        "invalidation",
        &format!(
            "{}\0{}",
            invalidation.invalidation_id, invalidation.invalidation_digest
        ),
    )
}

fn expiration_event_id(proposal: &AppPersonalAgentRetrievalProjectionProposalV1) -> String {
    digest_event_id("expiration", &proposal.proposal_digest)
}

fn digest_event_id(kind: &str, value: &str) -> String {
    let digest = content_digest(format!("retrieval-{kind}\0{value}").as_bytes());
    format!("retrieval-{kind}:{}", digest.trim_start_matches("blake3:"))
}

fn validate_lease_request(
    lease_owner: &str,
    lease_duration: StdDuration,
) -> Result<(), AppContributionError> {
    if lease_owner.is_empty()
        || lease_owner.len() > 192
        || lease_duration.is_zero()
        || lease_duration > MAX_CLAIM_LEASE
    {
        return Err(invalid_control("invalid retrieval outbox lease request"));
    }
    Ok(())
}

fn validate_retry_delay(retry_delay: StdDuration) -> Result<(), AppContributionError> {
    if retry_delay > MAX_RETRY_DELAY {
        return Err(invalid_control("retrieval retry delay exceeds its bound"));
    }
    Ok(())
}

fn ensure_scope(
    authenticated: &AuthenticatedAppScope,
    principal: &str,
    workspace: &str,
    scope_binding_ref: &str,
) -> Result<(), AppContributionError> {
    if authenticated.scope().principal.as_str() != principal
        || authenticated.scope().workspace.as_str() != workspace
        || authenticated.scope_binding_ref().as_str() != scope_binding_ref
    {
        return Err(invalid_control("retrieval lease scope substitution"));
    }
    Ok(())
}

fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn millis_timestamp(value: i64, label: &str) -> Result<String, AppContributionError> {
    DateTime::<Utc>::from_timestamp_millis(value)
        .map(|value| timestamp(&value))
        .ok_or_else(|| invalid_control(&format!("{label} is outside supported time")))
}

fn to_i64(value: u64, label: &str) -> Result<i64, AppContributionError> {
    i64::try_from(value).map_err(|_| invalid_control(&format!("{label} overflow")))
}

fn to_u64(value: i64, label: &str) -> Result<u64, AppContributionError> {
    u64::try_from(value).map_err(|_| invalid_control(&format!("{label} is negative")))
}

fn to_u32(value: i64, label: &str) -> Result<u32, AppContributionError> {
    u32::try_from(value).map_err(|_| invalid_control(&format!("{label} overflow")))
}

fn invalid_control(message: &str) -> AppContributionError {
    AppRegistryError::InvalidControlPlane(message.to_owned()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_identity_domains_do_not_alias_destinations_or_operations() {
        let proposal = digest_event_id("proposal", "same");
        let invalidation = digest_event_id("invalidation", "same");
        let expiration = digest_event_id("expiration", "same");
        assert_ne!(proposal, invalidation);
        assert_ne!(proposal, expiration);
        assert_ne!(invalidation, expiration);
        assert!(proposal.len() <= 192);
    }

    #[test]
    fn paired_destination_head_refuses_partial_persistence() {
        assert!(paired_head(Some(1), None).is_err());
        assert!(paired_head(None, Some(content_digest(b"receipt"))).is_err());
    }
}
