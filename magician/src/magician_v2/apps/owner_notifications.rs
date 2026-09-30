//! Durable, one-way owner notifications emitted by authenticated app workflows.
//!
//! The app never owns destination scope, correlation, idempotency, rate
//! accounting, delivery state, or a response channel. A workflow reopens its
//! exact task/manifest/grant and mints [`AppOwnerNotificationCommand`]; this
//! owner atomically admits one deterministic outbox row. A bounded system
//! worker claims rows and projects them into deterministic `UserRequest`s.
//!
//! Both lanes that worker drives — delivery and terminal-payload retention —
//! are owned unconditionally. The schedule/event boot master arms unattended
//! *behavior execution* and nothing in this module, so flipping it can neither
//! orphan notification debt a foreground workflow already accepted nor park a
//! retention window that has already closed. The single deliberate exception
//! is the owner's background-behavior pause: it fences delivery, because that
//! is the operator's one control over app-initiated attention, and it never
//! fences retention, because continuing to hold an expired payload is the harm
//! the pause was not asked to cause.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    lifecycle::AppInstallationStatus,
    manifest::{APP_NOTIFICATION_MAX_TTL_SECONDS, APP_NOTIFICATION_MIN_TTL_SECONDS},
    models::{
        decode_app_contract, AppContractError, AppContractLimits, AppDigest, AppInstallationId,
        AppName, AppReference, AppRevision,
    },
    records::{
        AppGrantRevision, AppInstallation, AppNotificationGrant, AppNotificationKindV1,
        AppNotificationSeverityV1, AppScope,
    },
    registry::{AppRegistryError, AppRegistryService},
};
use crate::magician_v2::user_requests::{
    RequestOption, UserRequest, UserRequestService, UserRequestSubmissionError,
};

const MAX_NOTIFICATION_TITLE_BYTES: usize = 160;
const MAX_NOTIFICATION_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 32 * 1024;
const MAX_NOTIFICATION_CLAIM_LIMIT: u16 = 32;
const MIN_NOTIFICATION_LEASE_SECONDS: i64 = 5;
const MAX_NOTIFICATION_LEASE_SECONDS: i64 = 5 * 60;
const MAX_NOTIFICATION_DELIVERY_ATTEMPTS: u32 = 64;
const MAX_NOTIFICATION_CAPACITY_BACKOFF_SECONDS: i64 = 60 * 60;
const NOTIFICATION_COMPACTION_LIMIT: i64 = 32;
const TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT: i64 = 64;

#[derive(Clone, Copy)]
enum TerminalCompactionRail {
    Forward,
    Revisit,
}

struct NotificationCompactionCursor {
    forward_after: Option<String>,
    forward_end: Option<String>,
    revisit_after: Option<String>,
    revisit_end: Option<String>,
    forward_pages_since_revisit: i64,
}

struct NotificationCompactionCandidate {
    correlation_id: String,
    installation_id: String,
    installation_generation: i64,
    workflow_id: String,
    port_id: String,
    reviewed_request_digest: String,
    period_seconds: i64,
    effect_ref: String,
    payload_digest: String,
    severity: String,
    state: String,
    expires_at: String,
    created_at: String,
    payload_json: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum AppOwnerNotificationError {
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error("app owner-notification request is invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("app owner-notification authority changed before acceptance")]
    AuthorityChanged,
    #[error("the notification effect identity was replayed with different bytes or authority")]
    IdempotencyConflict,
    #[error("the reviewed notification period volume is exhausted")]
    VolumeExceeded,
    #[error("the reviewed notification pending ceiling is exhausted")]
    PendingExceeded,
    #[error("the notification delivery lease is stale or no longer owned")]
    LeaseStale,
    #[error("app owner-notification time arithmetic overflowed")]
    TimeOverflow,
    #[error("app owner-notification worker identity is invalid")]
    InvalidWorker,
    #[error("the durable notification row is corrupt")]
    CorruptRow,
    #[error("durable owner-notification delivery is temporarily unavailable")]
    DeliveryUnavailable,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppOwnerNotificationAcceptanceStatus {
    Accepted,
    IdempotentReplay,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppOwnerNotificationAcceptanceReceipt {
    pub status: AppOwnerNotificationAcceptanceStatus,
    pub correlation_id: String,
    pub expires_at: DateTime<Utc>,
}

/// Non-deserializable authority produced only after the workflow owner has
/// reopened the exact task, immutable manifest and current grant.
pub(crate) struct AppOwnerNotificationCommand {
    authenticated: AuthenticatedAppScope,
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    grant_revision: AppRevision,
    schema_revision: AppRevision,
    workflow_id: AppName,
    port_id: AppName,
    grant: AppNotificationGrant,
    effect_ref: AppReference,
    task_id: String,
    execution_id: String,
    agent_id: String,
    title: Option<String>,
    message: String,
    severity: AppNotificationSeverityV1,
}

impl AppOwnerNotificationCommand {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_revalidated_workflow(
        authenticated: AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        grant_revision: AppRevision,
        schema_revision: AppRevision,
        workflow_id: AppName,
        port_id: AppName,
        grant: AppNotificationGrant,
        effect_ref: AppReference,
        task_id: String,
        execution_id: String,
        agent_id: String,
        title: Option<String>,
        message: String,
        severity: AppNotificationSeverityV1,
    ) -> Result<Self, AppOwnerNotificationError> {
        validate_notification_text(title.as_deref(), &message)?;
        if grant.workflow_id != workflow_id
            || grant.port_id != port_id
            || severity_rank(severity) > severity_rank(grant.severity_ceiling)
            || severity == AppNotificationSeverityV1::Critical
            || installation_generation == 0
            || task_id.trim().is_empty()
            || execution_id.trim().is_empty()
            || agent_id.trim().is_empty()
        {
            return Err(AppOwnerNotificationError::InvalidRequest(
                "notification command does not match its reviewed port",
            ));
        }
        Ok(Self {
            authenticated,
            installation_id,
            installation_generation,
            package_revision_ref,
            grant_revision,
            schema_revision,
            workflow_id,
            port_id,
            grant,
            effect_ref,
            task_id,
            execution_id,
            agent_id,
            title,
            message,
            severity,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredOwnerNotificationPayload {
    task_id: String,
    execution_id: String,
    agent_id: String,
    title: Option<String>,
    message: String,
    purpose: String,
    kind: AppNotificationKindV1,
    severity: AppNotificationSeverityV1,
}

#[derive(Clone)]
pub struct AppOwnerNotificationService {
    registry: AppRegistryService,
}

struct CapacityRefundAuthority {
    authenticated: AuthenticatedAppScope,
    principal: String,
    workspace: String,
    correlation_id: String,
    revision: u64,
    fence: u64,
    lease_owner: String,
    lease_token: AppDigest,
}

impl CapacityRefundAuthority {
    fn from_claim(
        authenticated: &AuthenticatedAppScope,
        claim: &AppOwnerNotificationClaim,
    ) -> Self {
        Self {
            authenticated: authenticated.clone(),
            principal: claim.principal.clone(),
            workspace: claim.workspace.clone(),
            correlation_id: claim.correlation_id.clone(),
            revision: claim.revision,
            fence: claim.fence,
            lease_owner: claim.lease_owner.clone(),
            lease_token: claim.lease_token.clone(),
        }
    }
}

/// Once an attempt has been durably charged, cancellation while waiting for
/// bounded UserRequest admission must not consume the delivery budget. The
/// guard owns enough exact fenced authority to continue the refund in a
/// detached runtime task; normal classified outcomes disarm it synchronously.
/// This relies on `submit_nonblocking_durable -> accept_request` having its
/// only await at pending-lock acquisition, before any mutation: persistence,
/// timeout/queue installation, and the Ready result all complete in the same
/// poll after that lock is acquired. Therefore Drop can only observe a
/// pre-commit cancellation; it never guesses about an unobserved durable
/// acceptance.
struct CapacityRefundOnCancel {
    registry: AppRegistryService,
    authority: Option<CapacityRefundAuthority>,
}

impl CapacityRefundOnCancel {
    fn new(registry: AppRegistryService, authority: CapacityRefundAuthority) -> Self {
        Self {
            registry,
            authority: Some(authority),
        }
    }

    fn disarm(&mut self) {
        self.authority = None;
    }
}

impl Drop for CapacityRefundOnCancel {
    fn drop(&mut self) {
        let Some(authority) = self.authority.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let registry = self.registry.clone();
        let _ = runtime.spawn(async move {
            let _ = refund_capacity_limited_delivery_attempt_task(registry, authority, Utc::now())
                .await;
        });
    }
}

impl std::fmt::Debug for AppOwnerNotificationService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppOwnerNotificationService")
            .finish_non_exhaustive()
    }
}

impl AppOwnerNotificationService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    pub(crate) async fn accept(
        &self,
        command: AppOwnerNotificationCommand,
        now: DateTime<Utc>,
    ) -> Result<AppOwnerNotificationAcceptanceReceipt, AppOwnerNotificationError> {
        let authenticated = command.authenticated.clone();
        self.registry
            .execute_scoped_typed_write(&authenticated, &now, move |connection, scope| {
                accept_notification_blocking(connection, scope, command, now)
            })
            .await
    }

    /// Claim a bounded page of due delivery debt for one authenticated scope.
    /// Expired leases are fenced before selection and each returned claim is a
    /// move-only CAS capability, not transport-deserializable authority.
    /// `now` is the caller's initial admission boundary; the write transaction
    /// re-samples the host clock and authentication after all waits.
    ///
    /// The owner's background-behavior pause fences this lane; the
    /// unattended-execution boot master does not. Debt a foreground workflow
    /// accepted is the owner's attention, not the scheduler's work, so it
    /// drains whether or not schedule/event execution is armed. Pinned by
    /// `behavior_pause_stands_down_delivery_maintenance_but_never_retention`.
    pub async fn claim_due(
        &self,
        authenticated: &AuthenticatedAppScope,
        worker_id: &str,
        limit: u16,
        lease_seconds: u64,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppOwnerNotificationClaim>, AppOwnerNotificationError> {
        if worker_id.trim() != worker_id
            || worker_id.is_empty()
            || worker_id.len() > 192
            || worker_id.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AppOwnerNotificationError::InvalidWorker);
        }
        if limit == 0 || limit > MAX_NOTIFICATION_CLAIM_LIMIT {
            return Err(AppOwnerNotificationError::InvalidRequest(
                "claim limit is outside the worker ceiling",
            ));
        }
        let lease_seconds =
            i64::try_from(lease_seconds).map_err(|_| AppOwnerNotificationError::TimeOverflow)?;
        if !(MIN_NOTIFICATION_LEASE_SECONDS..=MAX_NOTIFICATION_LEASE_SECONDS)
            .contains(&lease_seconds)
        {
            return Err(AppOwnerNotificationError::InvalidRequest(
                "delivery lease is outside the worker ceiling",
            ));
        }
        let worker_id = worker_id.to_owned();
        let authenticated_boundary = authenticated.clone();
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, scope| {
                claim_notifications_blocking(
                    connection,
                    scope,
                    &authenticated_boundary,
                    &worker_id,
                    limit,
                    lease_seconds,
                )
            })
            .await
    }

    /// Compact a bounded page of terminal notification payloads only after the
    /// stored TTL and the full reviewed volume window have both elapsed.
    /// Digest-only tombstones remain as immutable replay authority.
    ///
    /// This lane reads no execution posture at all — neither the boot master
    /// nor the pause. Retention is an obligation on bytes whose protected
    /// window has closed, and an operator who turned unattended execution off
    /// asked for less app work, not for indefinite retention of a payload the
    /// review already time-boxed. Pinned by
    /// `terminal_payload_retention_is_owned_independently_of_the_behavior_posture`.
    pub async fn compact_terminal(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<usize, AppOwnerNotificationError> {
        let authenticated_boundary = authenticated.clone();
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, scope| {
                if notification_compaction_is_idle_and_empty(connection)? {
                    return Ok(0);
                }
                let mut transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                // Admission and the SQLite write wait precede this boundary.
                // A credential that expired while queued cannot authorize
                // tombstone publication, payload deletion, or cursor writes.
                let boundary_now = Utc::now();
                if authenticated_boundary.scope() != scope
                    || authenticated_boundary.ensure_live_at(&boundary_now).is_err()
                {
                    return Err(AppOwnerNotificationError::AuthorityChanged);
                }
                if notification_compaction_raw_table_is_empty(&transaction)? {
                    reset_all_notification_compaction_rails(&transaction, boundary_now)?;
                    let commit_now = Utc::now();
                    if authenticated_boundary.scope() != scope
                        || authenticated_boundary.ensure_live_at(&commit_now).is_err()
                    {
                        return Err(AppOwnerNotificationError::AuthorityChanged);
                    }
                    transaction.commit()?;
                    return Ok(0);
                }
                let (rail, mut cursor, candidates) =
                    load_notification_compaction_page(&transaction, boundary_now)?;
                if candidates.is_empty() {
                    reset_notification_compaction_rail(&mut cursor, rail);
                    store_notification_compaction_cursor(&transaction, &cursor, boundary_now)?;
                    let commit_now = Utc::now();
                    if authenticated_boundary.scope() != scope
                        || authenticated_boundary.ensure_live_at(&commit_now).is_err()
                    {
                        return Err(AppOwnerNotificationError::AuthorityChanged);
                    }
                    transaction.commit()?;
                    return Ok(0);
                }
                let mut compacted = 0usize;
                for candidate in &candidates {
                    let correlation_id = &candidate.correlation_id;
                    let installation_id = &candidate.installation_id;
                    let installation_generation = candidate.installation_generation;
                    let workflow_id = &candidate.workflow_id;
                    let port_id = &candidate.port_id;
                    let reviewed_request_digest = &candidate.reviewed_request_digest;
                    let period_seconds = candidate.period_seconds;
                    let effect_ref = &candidate.effect_ref;
                    let payload_digest = &candidate.payload_digest;
                    let severity = &candidate.severity;
                    let expires_at = &candidate.expires_at;
                    let created_at = &candidate.created_at;
                    let payload_json = &candidate.payload_json;
                    if notification_compaction_candidate_is_quarantined(
                        &transaction,
                        installation_id,
                        correlation_id,
                    )? {
                        continue;
                    }
                    let validation = (|| {
                        let parsed_installation_id =
                            AppInstallationId::parse(installation_id.clone()).ok()?;
                        let parsed_workflow_id = AppName::parse(workflow_id.clone()).ok()?;
                        let parsed_port_id = AppName::parse(port_id.clone()).ok()?;
                        let parsed_effect_ref = AppReference::parse(effect_ref.clone()).ok()?;
                        let parsed_review_digest =
                            AppDigest::parse(reviewed_request_digest.clone()).ok()?;
                        let parsed_payload_digest = AppDigest::parse(payload_digest.clone()).ok()?;
                        let expires_at_value = parse_timestamp(expires_at).ok()?;
                        let created_at_value = parse_timestamp(created_at).ok()?;
                        let generation = u64::try_from(installation_generation).ok()?;
                        let period = u64::try_from(period_seconds).ok()?;
                        let period_seconds = i64::try_from(period).ok()?;
                        let lifetime_nanos = expires_at_value
                            .signed_duration_since(created_at_value)
                            .num_nanoseconds()?;
                        let lifetime_is_integral = lifetime_nanos % 1_000_000_000 == 0;
                        let lifetime_seconds = u64::try_from(lifetime_nanos / 1_000_000_000).ok()?;
                        let period_end = created_at_value
                            .checked_add_signed(Duration::seconds(period_seconds))?;
                        let payload: StoredOwnerNotificationPayload =
                            serde_json::from_slice(payload_json).ok()?;
                        validate_notification_text(payload.title.as_deref(), &payload.message)
                            .ok()?;
                        let expected_correlation = notification_correlation_id_for_parts(
                            scope,
                            &parsed_installation_id,
                            generation,
                            &parsed_workflow_id,
                            &parsed_port_id,
                            &parsed_review_digest,
                            &parsed_effect_ref,
                            &payload.task_id,
                            &payload.execution_id,
                        )
                        .ok()?;
                        (generation > 0
                            && (60..=2_678_400).contains(&period)
                            && created_at_value <= boundary_now
                            && lifetime_is_integral
                            && (APP_NOTIFICATION_MIN_TTL_SECONDS
                                ..=APP_NOTIFICATION_MAX_TTL_SECONDS)
                                .contains(&lifetime_seconds)
                            && matches!(severity.as_str(), "info" | "warning")
                            && AppDigest::blake3(payload_json) == parsed_payload_digest
                            && severity_text(payload.severity) == severity
                            && expected_correlation == *correlation_id)
                            .then_some((expires_at_value, period_end))
                    })();
                    let Some((expires_at_value, period_end)) = validation else {
                        quarantine_notification_compaction_candidate(
                            &transaction,
                            installation_id,
                            correlation_id,
                            "integrity_verification_failed",
                            boundary_now,
                        )?;
                        continue;
                    };
                    if !matches!(candidate.state.as_str(), "delivered" | "dead_letter") {
                        continue;
                    }
                    if !notification_terminal_retention_elapsed(
                        &expires_at_value,
                        &period_end,
                        &boundary_now,
                    ) {
                        continue;
                    }
                    let mut savepoint = transaction.savepoint()?;
                    let row_result = (|| -> Result<(), AppOwnerNotificationError> {
                        savepoint.execute(
                            "INSERT OR IGNORE INTO app_owner_notification_tombstones (
                                correlation_id, installation_id, installation_generation,
                                workflow_id, port_id, reviewed_request_digest, period_seconds,
                                effect_ref, payload_digest, severity, expires_at, created_at,
                                compacted_at
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                            params![
                                correlation_id, installation_id, installation_generation,
                                workflow_id, port_id, reviewed_request_digest, period_seconds,
                                effect_ref, payload_digest, severity, expires_at, created_at,
                                timestamp(boundary_now),
                            ],
                        )?;
                        let retained = savepoint
                            .query_row(
                                "SELECT installation_id, installation_generation, workflow_id,
                                        port_id, reviewed_request_digest, period_seconds, effect_ref,
                                        payload_digest, severity, expires_at, created_at,
                                        compacted_at
                                   FROM app_owner_notification_tombstones
                                  WHERE correlation_id = ?1",
                                params![correlation_id],
                                |row| {
                                    Ok((
                                        row.get::<_, String>(0)?,
                                        row.get::<_, i64>(1)?,
                                        row.get::<_, String>(2)?,
                                        row.get::<_, String>(3)?,
                                        row.get::<_, String>(4)?,
                                        row.get::<_, i64>(5)?,
                                        row.get::<_, String>(6)?,
                                        row.get::<_, String>(7)?,
                                        row.get::<_, String>(8)?,
                                        row.get::<_, String>(9)?,
                                        row.get::<_, String>(10)?,
                                        row.get::<_, String>(11)?,
                                    ))
                                },
                            )
                            .optional()?
                            .ok_or(AppOwnerNotificationError::IdempotencyConflict)?;
                        let retained_created_at = parse_timestamp(&retained.10).ok();
                        let retained_compacted_at = parse_timestamp(&retained.11).ok();
                        let retained_matches = retained.0.as_str() == installation_id.as_str()
                            && retained.1 == installation_generation
                            && retained.2.as_str() == workflow_id.as_str()
                            && retained.3.as_str() == port_id.as_str()
                            && retained.4.as_str() == reviewed_request_digest.as_str()
                            && retained.5 == period_seconds
                            && retained.6.as_str() == effect_ref.as_str()
                            && retained.7.as_str() == payload_digest.as_str()
                            && retained.8.as_str() == severity.as_str()
                            && retained.9.as_str() == expires_at.as_str()
                            && retained.10.as_str() == created_at.as_str()
                            && retained_compacted_at
                                .zip(retained_created_at)
                                .is_some_and(|(compacted, created)| {
                                    compacted >= created && compacted <= boundary_now
                                });
                        if !retained_matches {
                            return Err(AppOwnerNotificationError::IdempotencyConflict);
                        }
                        let deleted = savepoint.execute(
                            "DELETE FROM app_owner_notification_outbox
                              WHERE correlation_id = ?1
                                AND installation_id = ?2 AND installation_generation = ?3
                                AND workflow_id = ?4 AND port_id = ?5
                                AND reviewed_request_digest = ?6 AND period_seconds = ?7
                                AND effect_ref = ?8 AND payload_digest = ?9 AND severity = ?10
                                AND payload_json = ?11 AND expires_at = ?12 AND created_at = ?13
                                AND state = ?14",
                            params![
                                correlation_id, installation_id, installation_generation,
                                workflow_id, port_id, reviewed_request_digest, period_seconds,
                                effect_ref, payload_digest, severity, payload_json, expires_at,
                                created_at, candidate.state.as_str(),
                            ],
                        )?;
                        if deleted != 1 {
                            return Err(AppOwnerNotificationError::LeaseStale);
                        }
                        Ok(())
                    })();
                    match row_result {
                        Ok(()) => {
                            savepoint.commit()?;
                            compacted = compacted.saturating_add(1);
                        },
                        Err(AppOwnerNotificationError::Sqlite(error))
                            if is_compaction_integrity_sqlite_error(&error) =>
                        {
                            savepoint.rollback()?;
                            savepoint.commit()?;
                            quarantine_notification_compaction_candidate(
                                &transaction,
                                &installation_id,
                                &correlation_id,
                                "compaction_state_conflict",
                                boundary_now,
                            )?;
                        },
                        Err(AppOwnerNotificationError::Sqlite(error)) => {
                            savepoint.rollback()?;
                            savepoint.commit()?;
                            return Err(AppOwnerNotificationError::Sqlite(error));
                        },
                        Err(_) => {
                            savepoint.rollback()?;
                            savepoint.commit()?;
                            quarantine_notification_compaction_candidate(
                                &transaction,
                                &installation_id,
                                &correlation_id,
                                "compaction_state_conflict",
                                boundary_now,
                            )?;
                        },
                    }
                }
                advance_notification_compaction_cursor(&mut cursor, rail, &candidates)?;
                store_notification_compaction_cursor(&transaction, &cursor, boundary_now)?;
                // Payload validation and up to 32 savepoint operations can
                // outlive a short credential even though the transaction-open
                // check was current. Revalidate at the actual commit boundary
                // so tombstones, deletion, and cursor movement roll back
                // together when authority expires mid-page.
                let commit_now = Utc::now();
                if authenticated_boundary.scope() != scope
                    || authenticated_boundary.ensure_live_at(&commit_now).is_err()
                {
                    return Err(AppOwnerNotificationError::AuthorityChanged);
                }
                transaction.commit()?;
                Ok(compacted)
            })
            .await
    }

    /// Re-open live owner authority and submit the exact deterministic
    /// `UserRequest` for one claimed row. This consumes the claim into an
    /// opaque settlement attempt; no owner response is observable by the app
    /// or returned through this API. A transient revalidation error leaves the
    /// durable lease available for normal expiry/recovery.
    pub async fn deliver_claim(
        &self,
        authenticated: &AuthenticatedAppScope,
        user_requests: &UserRequestService,
        mut claim: AppOwnerNotificationClaim,
        now: DateTime<Utc>,
    ) -> Result<AppOwnerNotificationDeliveryAttempt, AppOwnerNotificationError> {
        if authenticated.scope().principal.as_str() != claim.principal.as_str()
            || authenticated.scope().workspace.as_str() != claim.workspace.as_str()
        {
            return Err(AppOwnerNotificationError::LeaseStale);
        }
        if now >= claim.expires_at || now >= claim.lease_expires_at {
            return Err(AppOwnerNotificationError::LeaseStale);
        }
        let Some((live_ttl_seconds, live_expires_at)) = self
            .begin_delivery_attempt(authenticated, &mut claim)
            .await?
        else {
            return Ok(AppOwnerNotificationDeliveryAttempt {
                claim,
                disposition: DeliveryDisposition::Expired,
            });
        };
        claim.expires_at = live_expires_at;
        claim.timeout_secs = claim.timeout_secs.min(live_ttl_seconds);
        let mut cancellation_refund = CapacityRefundOnCancel::new(
            self.registry.clone(),
            CapacityRefundAuthority::from_claim(authenticated, &claim),
        );
        let request = owner_notification_user_request(&claim);
        let disposition = match user_requests.submit_nonblocking_durable(request).await {
            Ok(receipt) if receipt.request_id() == claim.correlation_id.as_str() => {
                cancellation_refund.disarm();
                DeliveryDisposition::Submitted {
                    user_request_id: claim.correlation_id.clone(),
                }
            },
            Err(UserRequestSubmissionError::ScopeCapacityExceeded) => {
                // Bounded UserRequest capacity is ordinary backpressure, not
                // a failed delivery. Refund the attempt through the exact
                // fenced lease before returning it to lease-expiry recovery;
                // otherwise sustained pressure can consume the terminal
                // delivery-attempt ceiling without ever submitting a request.
                // `refund_capacity_limited_delivery_attempt` detaches its own
                // task before yielding, so ownership may transfer safely from
                // the cancellation guard without opening a cancellation gap.
                cancellation_refund.disarm();
                self.refund_capacity_limited_delivery_attempt(
                    authenticated,
                    &mut claim,
                    Utc::now(),
                )
                .await?;
                return Err(AppOwnerNotificationError::DeliveryUnavailable);
            },
            Err(UserRequestSubmissionError::PersistenceUnavailable) => {
                // Persistence outage remains an attempt-consuming delivery
                // failure by policy; only aggregate capacity is refundable.
                cancellation_refund.disarm();
                return Err(AppOwnerNotificationError::DeliveryUnavailable);
            },
            Ok(_)
            | Err(UserRequestSubmissionError::InvalidRequestId)
            | Err(UserRequestSubmissionError::IdempotencyConflict { .. }) => {
                cancellation_refund.disarm();
                DeliveryDisposition::PermanentFailure
            },
        };
        Ok(AppOwnerNotificationDeliveryAttempt { claim, disposition })
    }

    async fn begin_delivery_attempt(
        &self,
        authenticated: &AuthenticatedAppScope,
        claim: &mut AppOwnerNotificationClaim,
    ) -> Result<Option<(u64, DateTime<Utc>)>, AppOwnerNotificationError> {
        let authenticated_boundary = authenticated.clone();
        let correlation_id = claim.correlation_id.clone();
        let installation_id = claim.installation_id.clone();
        let installation_generation = claim.installation_generation;
        let workflow_id = claim.workflow_id.clone();
        let port_id = claim.port_id.clone();
        let reviewed_request_digest = claim.reviewed_request_digest.clone();
        let payload_kind = claim.payload.kind;
        let payload_purpose = claim.payload.purpose.clone();
        let payload_severity = claim.payload.severity;
        let revision = claim.revision;
        let fence = claim.fence;
        let lease_owner = claim.lease_owner.clone();
        let lease_token = claim.lease_token.clone();
        let lease_expires_at = claim.lease_expires_at;
        let stored_expires_at = claim.expires_at;
        let created_at = claim.created_at;
        let next_revision = revision
            .checked_add(1)
            .ok_or(AppOwnerNotificationError::CorruptRow)?;
        let admission_now = Utc::now();
        let admitted = self
            .registry
            .execute_scoped_typed_background_write(
                authenticated,
                &admission_now,
                move |connection, scope| {
                    let transaction =
                        connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    // This timestamp is sampled after the write transaction is
                    // established. No lease, TTL or worker credential checked
                    // before a blocking registry hop can authorize delivery.
                    let boundary_now = Utc::now();
                    if authenticated_boundary.scope() != scope
                        || authenticated_boundary
                            .ensure_live_at(&boundary_now)
                            .is_err()
                        || boundary_now >= lease_expires_at
                    {
                        return Err(AppOwnerNotificationError::LeaseStale);
                    }
                    let (_, grant) = load_live_installation_grant(
                        &transaction,
                        scope,
                        &installation_id,
                        Some(installation_generation),
                        None,
                        None,
                        None,
                    )?;
                    let live_port = grant
                        .granted_notification_grants
                        .iter()
                        .find(|grant| grant.workflow_id == workflow_id && grant.port_id == port_id)
                        .ok_or(AppOwnerNotificationError::AuthorityChanged)?;
                    if live_port.reviewed_request_digest != reviewed_request_digest
                        || live_port.kind != payload_kind
                        || live_port.purpose != payload_purpose
                        || severity_rank(payload_severity)
                            > severity_rank(live_port.severity_ceiling)
                    {
                        return Err(AppOwnerNotificationError::AuthorityChanged);
                    }
                    let live_ttl_seconds = live_port.ttl_seconds;
                    let live_expires_at = created_at
                        .checked_add_signed(Duration::seconds(
                            i64::try_from(live_ttl_seconds)
                                .map_err(|_| AppOwnerNotificationError::CorruptRow)?,
                        ))
                        .ok_or(AppOwnerNotificationError::CorruptRow)?
                        .min(stored_expires_at);
                    if boundary_now >= live_expires_at {
                        transaction.commit()?;
                        return Ok(None);
                    }
                    let boundary_now_text = timestamp(boundary_now);
                    let pending_refunds =
                        pending_capacity_refund_count(&transaction, correlation_id.as_str())?;
                    let changed = transaction.execute(
                        "UPDATE app_owner_notification_outbox
                            SET revision = ?1,
                                attempt_count = attempt_count + 1 - ?9,
                                updated_at = ?2
                          WHERE correlation_id = ?3 AND state = 'leased'
                            AND revision = ?4 AND fence = ?5
                            AND lease_owner = ?6 AND lease_token = ?7
                            AND lease_expires_at = ?8 AND lease_expires_at > ?2
                            AND expires_at > ?2 AND attempt_count >= ?9
                            AND attempt_count - ?9 < ?10
                            AND installation_id = ?11 AND installation_generation = ?12
                            AND workflow_id = ?13 AND port_id = ?14
                            AND reviewed_request_digest = ?15
                            AND COALESCE((SELECT paused FROM app_behavior_scope_policy
                                         WHERE singleton = 1), 0) = 0",
                        params![
                            sqlite_i64(next_revision)?,
                            boundary_now_text,
                            correlation_id,
                            sqlite_i64(revision)?,
                            sqlite_i64(fence)?,
                            lease_owner,
                            lease_token.as_str(),
                            timestamp(lease_expires_at),
                            i64::from(pending_refunds),
                            i64::from(MAX_NOTIFICATION_DELIVERY_ATTEMPTS),
                            installation_id.as_str(),
                            sqlite_i64(installation_generation)?,
                            workflow_id.as_str(),
                            port_id.as_str(),
                            reviewed_request_digest.as_str(),
                        ],
                    )?;
                    if changed != 1 {
                        return Err(AppOwnerNotificationError::LeaseStale);
                    }
                    mark_capacity_refunds_applied(
                        &transaction,
                        correlation_id.as_str(),
                        pending_refunds,
                        &boundary_now_text,
                    )?;
                    transaction.commit()?;
                    Ok(Some((live_ttl_seconds, live_expires_at, pending_refunds)))
                },
            )
            .await?;
        let Some(admitted) = admitted else {
            return Ok(None);
        };
        claim.revision = next_revision;
        claim.attempt_count = claim
            .attempt_count
            .checked_add(1)
            .and_then(|count| count.checked_sub(admitted.2))
            .ok_or(AppOwnerNotificationError::CorruptRow)?;
        Ok(Some((admitted.0, admitted.1)))
    }

    /// Refund the attempt consumed by [`Self::begin_delivery_attempt`] when
    /// the downstream owner is healthy but at its bounded per-scope capacity.
    ///
    /// A unique durable receipt owns the debit before it is applied. Exact,
    /// pending, expired, and terminal states can apply it immediately; an
    /// active successor consumes it atomically inside begin/settle rather than
    /// being revoked and made stale. Applied receipts remain as idempotency
    /// evidence until the parent outbox row is compacted. The refund owns an
    /// independent task before it awaits registry admission so the worker's
    /// bounded scope timeout cannot turn downstream capacity into a consumed
    /// delivery attempt. A whole-process/runtime shutdown before receipt
    /// commit may still spend one attempt, matching existing mid-delivery crash
    /// accounting; an ordinarily completed capacity refusal spends none.
    async fn refund_capacity_limited_delivery_attempt(
        &self,
        authenticated: &AuthenticatedAppScope,
        claim: &mut AppOwnerNotificationClaim,
        now: DateTime<Utc>,
    ) -> Result<(), AppOwnerNotificationError> {
        if authenticated.scope().principal.as_str() != claim.principal.as_str()
            || authenticated.scope().workspace.as_str() != claim.workspace.as_str()
        {
            return Err(AppOwnerNotificationError::LeaseStale);
        }
        let authority = CapacityRefundAuthority::from_claim(authenticated, claim);
        let registry = self.registry.clone();
        // Spawn before the first await so cancellation transfers ownership to
        // this detached task rather than dropping the charged attempt.
        let refund = tokio::spawn(refund_capacity_limited_delivery_attempt_task(
            registry, authority, now,
        ));
        let (next_revision, refunded_attempt_count) = refund.await.map_err(|error| {
            AppOwnerNotificationError::Registry(AppRegistryError::WorkerTerminated(
                error.to_string(),
            ))
        })??;
        claim.revision = next_revision;
        claim.attempt_count = refunded_attempt_count;
        Ok(())
    }

    /// Settle one consumed delivery attempt through exact lease/revision/fence
    /// CAS. A submitted UserRequest becomes `delivered`; expiry or a permanent
    /// deterministic-ID conflict becomes terminal debt, never an app response.
    /// Caller `now` gates initial admission only; settlement re-samples after
    /// acquiring SQLite write ownership.
    pub async fn settle_delivery(
        &self,
        authenticated: &AuthenticatedAppScope,
        attempt: AppOwnerNotificationDeliveryAttempt,
        now: DateTime<Utc>,
    ) -> Result<AppOwnerNotificationSettlementReceipt, AppOwnerNotificationError> {
        let authenticated_boundary = authenticated.clone();
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, scope| {
                settle_notification_blocking(connection, scope, &authenticated_boundary, attempt)
            })
            .await
    }
}

async fn refund_capacity_limited_delivery_attempt_task(
    registry: AppRegistryService,
    authority: CapacityRefundAuthority,
    admission_now: DateTime<Utc>,
) -> Result<(u64, u32), AppOwnerNotificationError> {
    if authority.authenticated.scope().principal.as_str() != authority.principal.as_str()
        || authority.authenticated.scope().workspace.as_str() != authority.workspace.as_str()
    {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    let authenticated_boundary = authority.authenticated.clone();
    let operation_boundary = authenticated_boundary.clone();
    let correlation_id = authority.correlation_id;
    let revision = authority.revision;
    let fence = authority.fence;
    let lease_owner = authority.lease_owner;
    let lease_token = authority.lease_token;
    registry
        .execute_scoped_typed_background_write(
            &authenticated_boundary,
            &admission_now,
            move |connection, scope| {
                // First commit a unique, content-free receipt. Once this
                // transaction commits, every claim/begin/settle path either
                // consumes the debit in its own CAS or leaves the row fenced
                // from a new claim. This is deliberately separate from the
                // application transaction so a later fault cannot forget the
                // already-owned refund.
                let receipt_transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let boundary_now = Utc::now();
                if operation_boundary.scope() != scope
                    || operation_boundary.ensure_live_at(&boundary_now).is_err()
                {
                    return Err(AppOwnerNotificationError::LeaseStale);
                }
                let boundary_now_text = timestamp(boundary_now);
                publish_capacity_refund_receipt(
                    &receipt_transaction,
                    correlation_id.as_str(),
                    revision,
                    fence,
                    lease_owner.as_str(),
                    lease_token.as_str(),
                    &boundary_now_text,
                )?;
                receipt_transaction.commit()?;

                // An active successor lease is never revoked here: doing so
                // after it charged or submitted would strand that charge and
                // make its settlement stale. Its begin/settle CAS consumes
                // this receipt instead. Every other applicable state can be
                // repaired immediately, including an attempt-ceiling terminal
                // transition that raced receipt publication.
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let apply_now = Utc::now();
                if operation_boundary.scope() != scope
                    || operation_boundary.ensure_live_at(&apply_now).is_err()
                {
                    return Err(AppOwnerNotificationError::LeaseStale);
                }
                let applied = apply_capacity_refund_receipt(
                    &transaction,
                    correlation_id.as_str(),
                    revision,
                    fence,
                    apply_now,
                )?;
                let current = match applied {
                    Some(value) => value,
                    None => {
                        current_notification_attempt_state(&transaction, correlation_id.as_str())?
                    },
                };
                transaction.commit()?;
                Ok(current)
            },
        )
        .await
}

fn publish_capacity_refund_receipt(
    transaction: &Transaction<'_>,
    correlation_id: &str,
    charged_revision: u64,
    charged_fence: u64,
    lease_owner: &str,
    lease_token: &str,
    created_at: &str,
) -> Result<(), AppOwnerNotificationError> {
    transaction.execute(
        "INSERT OR IGNORE INTO app_owner_notification_attempt_refunds (
             correlation_id, charged_revision, charged_fence,
             charged_lease_owner, charged_lease_token, created_at, applied_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        params![
            correlation_id,
            sqlite_i64(charged_revision)?,
            sqlite_i64(charged_fence)?,
            lease_owner,
            lease_token,
            created_at,
        ],
    )?;
    let stored: (String, String) = transaction.query_row(
        "SELECT charged_lease_owner, charged_lease_token
           FROM app_owner_notification_attempt_refunds
          WHERE correlation_id = ?1
            AND charged_revision = ?2 AND charged_fence = ?3",
        params![
            correlation_id,
            sqlite_i64(charged_revision)?,
            sqlite_i64(charged_fence)?,
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if stored.0 != lease_owner || stored.1 != lease_token {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    Ok(())
}

/// Apply one durable capacity debit if the current row can be changed without
/// invalidating an active successor. `None` means a live successor owns the
/// lease; its begin/settle transaction must consume the receipt instead.
fn apply_capacity_refund_receipt(
    transaction: &Transaction<'_>,
    correlation_id: &str,
    charged_revision: u64,
    charged_fence: u64,
    now: DateTime<Utc>,
) -> Result<Option<(u64, u32)>, AppOwnerNotificationError> {
    let receipt = transaction
        .query_row(
            "SELECT charged_lease_owner, charged_lease_token, applied_at
               FROM app_owner_notification_attempt_refunds
              WHERE correlation_id = ?1 AND charged_revision = ?2 AND charged_fence = ?3",
            params![
                correlation_id,
                sqlite_i64(charged_revision)?,
                sqlite_i64(charged_fence)?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppOwnerNotificationError::LeaseStale)?;
    if receipt.2.is_some() {
        return current_notification_attempt_state(transaction, correlation_id).map(Some);
    }
    let current = transaction
        .query_row(
            "SELECT state, revision, fence, attempt_count, lease_owner,
                    lease_token, lease_expires_at, last_error, expires_at
               FROM app_owner_notification_outbox WHERE correlation_id = ?1",
            params![correlation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppOwnerNotificationError::LeaseStale)?;
    if current.3 <= 0 {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let exact_charged_lease = current.0 == "leased"
        && current.1 == sqlite_i64(charged_revision)?
        && current.2 == sqlite_i64(charged_fence)?
        && current.4.as_deref() == Some(receipt.0.as_str())
        && current.5.as_deref() == Some(receipt.1.as_str());
    let expired_lease = if current.0 == "leased" {
        current
            .6
            .as_deref()
            .map(parse_timestamp)
            .transpose()?
            .is_some_and(|lease_expires_at| lease_expires_at <= now)
    } else {
        false
    };
    if current.0 == "leased" && !exact_charged_lease && !expired_lease {
        return Ok(None);
    }
    let expires_at = parse_timestamp(&current.8)?;
    let retry_at = capacity_refund_retry_at(transaction, correlation_id, now, expires_at)?;
    let retry_at_text = timestamp(retry_at);
    let now_text = timestamp(now);
    let revive_ceiling = current.0 == "dead_letter"
        && current.7.as_deref() == Some("delivery_attempt_ceiling_exhausted");
    let changed = if exact_charged_lease {
        transaction.execute(
            "UPDATE app_owner_notification_outbox
                SET state = 'pending', revision = revision + 1, fence = fence + 1,
                    attempt_count = attempt_count - 1, available_at = ?1,
                    lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                    updated_at = ?2
              WHERE correlation_id = ?3 AND state = 'leased'
                AND revision = ?4 AND fence = ?5
                AND lease_owner = ?6 AND lease_token = ?7 AND attempt_count > 0",
            params![
                &retry_at_text,
                &now_text,
                correlation_id,
                current.1,
                current.2,
                receipt.0.as_str(),
                receipt.1.as_str(),
            ],
        )?
    } else if expired_lease {
        transaction.execute(
            "UPDATE app_owner_notification_outbox
                SET state = 'pending', revision = revision + 1, fence = fence + 1,
                    attempt_count = attempt_count - 1,
                    available_at = CASE WHEN available_at > ?1 THEN available_at ELSE ?1 END,
                    lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL,
                    updated_at = ?2
              WHERE correlation_id = ?3 AND state = 'leased'
                AND revision = ?4 AND fence = ?5 AND lease_expires_at <= ?2
                AND attempt_count > 0",
            params![
                &retry_at_text,
                &now_text,
                correlation_id,
                current.1,
                current.2,
            ],
        )?
    } else if revive_ceiling {
        transaction.execute(
            "UPDATE app_owner_notification_outbox
                SET state = 'pending', revision = revision + 1, fence = fence + 1,
                    attempt_count = attempt_count - 1,
                    available_at = CASE WHEN available_at > ?1 THEN available_at ELSE ?1 END,
                    last_error = NULL, updated_at = ?2
              WHERE correlation_id = ?3 AND state = 'dead_letter'
                AND revision = ?4 AND fence = ?5
                AND last_error = 'delivery_attempt_ceiling_exhausted'
                AND attempt_count > 0",
            params![
                &retry_at_text,
                &now_text,
                correlation_id,
                current.1,
                current.2,
            ],
        )?
    } else {
        transaction.execute(
            "UPDATE app_owner_notification_outbox
                SET revision = revision + 1, attempt_count = attempt_count - 1,
                    available_at = CASE
                        WHEN state = 'pending' AND available_at < ?1 THEN ?1
                        ELSE available_at END,
                    updated_at = ?2
              WHERE correlation_id = ?3 AND state = ?4
                AND revision = ?5 AND fence = ?6 AND attempt_count > 0",
            params![
                &retry_at_text,
                &now_text,
                correlation_id,
                current.0.as_str(),
                current.1,
                current.2,
            ],
        )?
    };
    if changed != 1 {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    let receipt_changed = transaction.execute(
        "UPDATE app_owner_notification_attempt_refunds SET applied_at = ?1
          WHERE correlation_id = ?2 AND charged_revision = ?3 AND charged_fence = ?4
            AND applied_at IS NULL",
        params![
            &now_text,
            correlation_id,
            sqlite_i64(charged_revision)?,
            sqlite_i64(charged_fence)?,
        ],
    )?;
    if receipt_changed != 1 {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    current_notification_attempt_state(transaction, correlation_id).map(Some)
}

/// Derive a deterministic pressure retry from the durable receipt ordinal.
/// The exponential prefix prevents a full UserRequest scope from generating a
/// receipt every worker tick; the one-hour cap preserves recovery
/// responsiveness. The absolute notification deadline remains the final
/// authority and is never extended by backpressure.
fn capacity_refund_retry_at(
    transaction: &Transaction<'_>,
    correlation_id: &str,
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<DateTime<Utc>, AppOwnerNotificationError> {
    let receipt_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_owner_notification_attempt_refunds
          WHERE correlation_id = ?1",
        params![correlation_id],
        |row| row.get(0),
    )?;
    if receipt_count <= 0 {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let delay_seconds = capacity_refund_backoff_seconds(receipt_count)?;
    Ok(now
        .checked_add_signed(Duration::seconds(delay_seconds))
        .unwrap_or(expires_at)
        .min(expires_at))
}

fn capacity_refund_backoff_seconds(receipt_count: i64) -> Result<i64, AppOwnerNotificationError> {
    if receipt_count <= 0 {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let exponent = u32::try_from(receipt_count.saturating_sub(1))
        .map_err(|_| AppOwnerNotificationError::CorruptRow)?
        .min(30);
    let exponential_seconds = MIN_NOTIFICATION_LEASE_SECONDS
        .checked_mul(1_i64.checked_shl(exponent).unwrap_or(i64::MAX))
        .unwrap_or(i64::MAX);
    Ok(exponential_seconds.min(MAX_NOTIFICATION_CAPACITY_BACKOFF_SECONDS))
}

fn current_notification_attempt_state(
    transaction: &Transaction<'_>,
    correlation_id: &str,
) -> Result<(u64, u32), AppOwnerNotificationError> {
    transaction
        .query_row(
            "SELECT revision, attempt_count FROM app_owner_notification_outbox
              WHERE correlation_id = ?1",
            params![correlation_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or(AppOwnerNotificationError::LeaseStale)
        .and_then(|(revision, attempt_count)| {
            Ok((
                u64::try_from(revision).map_err(|_| AppOwnerNotificationError::CorruptRow)?,
                u32::try_from(attempt_count).map_err(|_| AppOwnerNotificationError::CorruptRow)?,
            ))
        })
}

fn pending_capacity_refund_count(
    transaction: &Transaction<'_>,
    correlation_id: &str,
) -> Result<u32, AppOwnerNotificationError> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_owner_notification_attempt_refunds
          WHERE correlation_id = ?1 AND applied_at IS NULL",
        params![correlation_id],
        |row| row.get(0),
    )?;
    let count = u32::try_from(count).map_err(|_| AppOwnerNotificationError::CorruptRow)?;
    if count > MAX_NOTIFICATION_DELIVERY_ATTEMPTS {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    Ok(count)
}

fn mark_capacity_refunds_applied(
    transaction: &Transaction<'_>,
    correlation_id: &str,
    expected: u32,
    now: &str,
) -> Result<(), AppOwnerNotificationError> {
    if expected == 0 {
        return Ok(());
    }
    let changed = transaction.execute(
        "UPDATE app_owner_notification_attempt_refunds SET applied_at = ?1
          WHERE correlation_id = ?2 AND applied_at IS NULL",
        params![now, correlation_id],
    )?;
    if changed != usize::try_from(expected).map_err(|_| AppOwnerNotificationError::CorruptRow)? {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    Ok(())
}

fn apply_capacity_refund_page(
    transaction: &Transaction<'_>,
    now: DateTime<Utc>,
    limit: u16,
) -> Result<(), AppOwnerNotificationError> {
    let now_text = timestamp(now);
    let receipts = {
        let mut statement = transaction.prepare(
            "SELECT refunds.correlation_id, refunds.charged_revision,
                    refunds.charged_fence
               FROM app_owner_notification_attempt_refunds AS refunds
               JOIN app_owner_notification_outbox AS outbox
                 ON outbox.correlation_id = refunds.correlation_id
              WHERE refunds.applied_at IS NULL
                AND (outbox.state <> 'leased'
                  OR outbox.lease_expires_at <= ?1
                  OR (outbox.revision = refunds.charged_revision
                    AND outbox.fence = refunds.charged_fence
                    AND outbox.lease_owner = refunds.charged_lease_owner
                    AND outbox.lease_token = refunds.charged_lease_token))
              ORDER BY refunds.created_at, refunds.correlation_id,
                       refunds.charged_revision, refunds.charged_fence
              LIMIT ?2",
        )?;
        let rows = statement.query_map(params![&now_text, i64::from(limit)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (correlation_id, revision, fence) in receipts {
        let revision =
            u64::try_from(revision).map_err(|_| AppOwnerNotificationError::CorruptRow)?;
        let fence = u64::try_from(fence).map_err(|_| AppOwnerNotificationError::CorruptRow)?;
        if apply_capacity_refund_receipt(
            transaction,
            correlation_id.as_str(),
            revision,
            fence,
            now.clone(),
        )?
        .is_none()
        {
            return Err(AppOwnerNotificationError::LeaseStale);
        }
    }
    Ok(())
}

fn owner_notification_user_request(claim: &AppOwnerNotificationClaim) -> UserRequest {
    UserRequest {
        id: claim.correlation_id.clone(),
        request_type: format!("app.notify.v1.{}", claim.correlation_id),
        question: render_notification(&claim.payload),
        options: vec![RequestOption {
            id: "acknowledge".to_owned(),
            label: "Acknowledge".to_owned(),
            requires_input: false,
        }],
        principal: claim.principal.clone(),
        workspace: claim.workspace.clone(),
        context: serde_json::json!({
            "input_type": "choice",
            "app_owner_notification": true,
            "correlation_id": &claim.correlation_id,
            "installation_id": claim.installation_id.as_str(),
            "installation_generation": claim.installation_generation,
            "workflow_id": claim.workflow_id.as_str(),
            "port_id": claim.port_id.as_str(),
            "task_id": &claim.payload.task_id,
            "execution_id": &claim.payload.execution_id,
            "app_agent_id": &claim.payload.agent_id,
            "kind": notification_kind_text(claim.payload.kind),
            "severity": severity_text(claim.payload.severity),
            "purpose": &claim.payload.purpose,
            "one_way": true,
            "absolute_expires_at_ms": claim.expires_at.timestamp_millis(),
        }),
        source: "app_owner_notification".to_owned(),
        // Deliberately omit live execution routing. `HitlResolved` must never
        // become an app-workflow response or resume signal; the immutable app
        // identities above are display/audit context only.
        execution_id: None,
        task_id: None,
        // Sealed from immutable `expires_at - created_at`, rather than
        // recomputed at delivery time, so a crash-after-submit replay is
        // byte-exact for UserRequest deterministic-ID admission.
        timeout_secs: claim.timeout_secs,
        default_on_timeout: "acknowledge".to_owned(),
        created_at: 0,
        sensitive: None,
    }
}

pub struct AppOwnerNotificationClaim {
    principal: String,
    workspace: String,
    correlation_id: String,
    installation_id: AppInstallationId,
    installation_generation: u64,
    workflow_id: AppName,
    port_id: AppName,
    reviewed_request_digest: AppDigest,
    payload: StoredOwnerNotificationPayload,
    revision: u64,
    fence: u64,
    attempt_count: u32,
    lease_owner: String,
    lease_token: AppDigest,
    lease_expires_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    timeout_secs: u64,
}

impl std::fmt::Debug for AppOwnerNotificationClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppOwnerNotificationClaim")
            .field("correlation_id", &self.correlation_id)
            .field("installation_id", &self.installation_id)
            .field("workflow_id", &self.workflow_id)
            .field("port_id", &self.port_id)
            .field("revision", &self.revision)
            .field("fence", &self.fence)
            .field("attempt_count", &self.attempt_count)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

enum DeliveryDisposition {
    Submitted { user_request_id: String },
    Expired,
    PermanentFailure,
}

pub struct AppOwnerNotificationDeliveryAttempt {
    claim: AppOwnerNotificationClaim,
    disposition: DeliveryDisposition,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppOwnerNotificationSettlementStatus {
    Delivered,
    DeadLetter,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppOwnerNotificationSettlementReceipt {
    pub correlation_id: String,
    pub status: AppOwnerNotificationSettlementStatus,
}

fn accept_notification_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    command: AppOwnerNotificationCommand,
    now: DateTime<Utc>,
) -> Result<AppOwnerNotificationAcceptanceReceipt, AppOwnerNotificationError> {
    if command.authenticated.scope() != scope {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    let correlation_id = notification_correlation_id(scope, &command)?;
    let payload = StoredOwnerNotificationPayload {
        task_id: command.task_id,
        execution_id: command.execution_id,
        agent_id: command.agent_id,
        title: command.title,
        message: command.message,
        purpose: command.grant.purpose.clone(),
        kind: command.grant.kind,
        severity: command.severity,
    };
    let payload_json = serde_json::to_vec(&payload)?;
    if payload_json.len() > MAX_NOTIFICATION_PAYLOAD_BYTES {
        return Err(AppOwnerNotificationError::InvalidRequest(
            "notification payload exceeds the byte ceiling",
        ));
    }
    let payload_digest = AppDigest::blake3(&payload_json);
    let expires_at = now
        .checked_add_signed(Duration::seconds(
            i64::try_from(command.grant.ttl_seconds)
                .map_err(|_| AppOwnerNotificationError::TimeOverflow)?,
        ))
        .ok_or(AppOwnerNotificationError::TimeOverflow)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (_, current_grant) = load_live_installation_grant(
        &transaction,
        scope,
        &command.installation_id,
        Some(command.installation_generation),
        Some(&command.package_revision_ref),
        Some(command.grant_revision),
        Some(command.schema_revision),
    )?;
    let live_port = current_grant
        .granted_notification_grants
        .iter()
        .find(|grant| grant.workflow_id == command.workflow_id && grant.port_id == command.port_id)
        .ok_or(AppOwnerNotificationError::AuthorityChanged)?;
    if live_port != &command.grant {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }

    let existing = transaction
        .query_row(
            "SELECT correlation_id, port_id, installation_generation, reviewed_request_digest, \
                    payload_digest, payload_json, severity, expires_at \
             FROM app_owner_notification_outbox \
             WHERE installation_id = ?1 AND workflow_id = ?2
               AND port_id = ?3 AND effect_ref = ?4",
            params![
                command.installation_id.as_str(),
                command.workflow_id.as_str(),
                command.port_id.as_str(),
                command.effect_ref.as_str(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;
    if let Some((
        existing_correlation,
        existing_port,
        existing_generation,
        existing_review_digest,
        existing_payload_digest,
        existing_payload_json,
        existing_severity,
        existing_expires_at,
    )) = existing
    {
        if existing_correlation != correlation_id
            || existing_port != command.port_id.as_str()
            || existing_generation != sqlite_i64(command.installation_generation)?
            || existing_review_digest != command.grant.reviewed_request_digest.as_str()
            || existing_payload_digest != payload_digest.as_str()
            || existing_payload_json != payload_json
            || existing_severity != severity_text(command.severity)
        {
            return Err(AppOwnerNotificationError::IdempotencyConflict);
        }
        let expires_at = parse_timestamp(&existing_expires_at)?;
        transaction.commit()?;
        return Ok(AppOwnerNotificationAcceptanceReceipt {
            status: AppOwnerNotificationAcceptanceStatus::IdempotentReplay,
            correlation_id,
            expires_at,
        });
    }

    // Terminal payload rows may already have been compacted. The immutable
    // digest tombstone remains the authority for this host-derived effect
    // identity, so an exact replay stays idempotent while any authority or
    // payload change remains a conflict.
    let retained = transaction
        .query_row(
            "SELECT correlation_id, port_id, installation_generation,
                    reviewed_request_digest, payload_digest, severity, expires_at
               FROM app_owner_notification_tombstones
              WHERE installation_id = ?1 AND workflow_id = ?2
                AND port_id = ?3 AND effect_ref = ?4",
            params![
                command.installation_id.as_str(),
                command.workflow_id.as_str(),
                command.port_id.as_str(),
                command.effect_ref.as_str(),
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
                ))
            },
        )
        .optional()?;
    if let Some((
        retained_correlation,
        retained_port,
        retained_generation,
        retained_review_digest,
        retained_payload_digest,
        retained_severity,
        retained_expires_at,
    )) = retained
    {
        if retained_correlation != correlation_id
            || retained_port != command.port_id.as_str()
            || retained_generation != sqlite_i64(command.installation_generation)?
            || retained_review_digest != command.grant.reviewed_request_digest.as_str()
            || retained_payload_digest != payload_digest.as_str()
            || retained_severity != severity_text(command.severity)
        {
            return Err(AppOwnerNotificationError::IdempotencyConflict);
        }
        let expires_at = parse_timestamp(&retained_expires_at)?;
        transaction.commit()?;
        return Ok(AppOwnerNotificationAcceptanceReceipt {
            status: AppOwnerNotificationAcceptanceStatus::IdempotentReplay,
            correlation_id,
            expires_at,
        });
    }

    let active_prior_period: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(period_seconds), 0)
           FROM app_owner_notification_outbox
          WHERE installation_id = ?1 AND workflow_id = ?2 AND port_id = ?3
            AND julianday(created_at) + (CAST(period_seconds AS REAL) / 86400.0)
                > julianday(?4)",
        params![
            command.installation_id.as_str(),
            command.workflow_id.as_str(),
            command.port_id.as_str(),
            timestamp(now),
        ],
        |row| row.get(0),
    )?;
    let effective_period_seconds = command.grant.period_seconds.max(
        u64::try_from(active_prior_period).map_err(|_| AppOwnerNotificationError::CorruptRow)?,
    );
    let period_start = now
        .checked_sub_signed(Duration::seconds(
            i64::try_from(effective_period_seconds)
                .map_err(|_| AppOwnerNotificationError::TimeOverflow)?,
        ))
        .ok_or(AppOwnerNotificationError::TimeOverflow)?;
    let used: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_owner_notification_outbox \
         WHERE installation_id = ?1 AND workflow_id = ?2 AND port_id = ?3 \
           AND created_at > ?4",
        params![
            command.installation_id.as_str(),
            command.workflow_id.as_str(),
            command.port_id.as_str(),
            timestamp(period_start),
        ],
        |row| row.get(0),
    )?;
    if used >= i64::from(command.grant.max_notifications_per_period) {
        return Err(AppOwnerNotificationError::VolumeExceeded);
    }
    let pending: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_owner_notification_outbox \
         WHERE installation_id = ?1 AND workflow_id = ?2 AND port_id = ?3 \
           AND state IN ('pending', 'leased', 'delivered') AND expires_at > ?4",
        params![
            command.installation_id.as_str(),
            command.workflow_id.as_str(),
            command.port_id.as_str(),
            timestamp(now),
        ],
        |row| row.get(0),
    )?;
    if pending >= i64::from(command.grant.max_pending) {
        return Err(AppOwnerNotificationError::PendingExceeded);
    }
    let now_text = timestamp(now);
    transaction.execute(
        "INSERT INTO app_owner_notification_outbox (\
            correlation_id, installation_id, installation_generation, workflow_id, port_id, \
            reviewed_request_digest, period_seconds, effect_ref, payload_digest, payload_json, severity, state, \
            revision, fence, attempt_count, available_at, expires_at, lease_owner, lease_token, \
            lease_expires_at, user_request_id, last_error, created_at, delivered_at, updated_at\
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', \
                   1, 0, 0, ?12, ?13, NULL, NULL, NULL, NULL, NULL, ?12, NULL, ?12)",
        params![
            correlation_id.as_str(),
            command.installation_id.as_str(),
            sqlite_i64(command.installation_generation)?,
            command.workflow_id.as_str(),
            command.port_id.as_str(),
            command.grant.reviewed_request_digest.as_str(),
            // Carry the longest still-active reviewed period forward. This
            // prevents a package/grant update from shortening an already
            // charged window and minting a fresh notification allowance.
            sqlite_i64(effective_period_seconds)?,
            command.effect_ref.as_str(),
            payload_digest.as_str(),
            payload_json,
            severity_text(command.severity),
            now_text,
            timestamp(expires_at),
        ],
    )?;
    transaction.commit()?;
    Ok(AppOwnerNotificationAcceptanceReceipt {
        status: AppOwnerNotificationAcceptanceStatus::Accepted,
        correlation_id,
        expires_at,
    })
}

struct ClaimRow {
    correlation_id: String,
    installation_id: String,
    installation_generation: i64,
    workflow_id: String,
    port_id: String,
    reviewed_request_digest: String,
    effect_ref: String,
    payload_digest: String,
    payload_json: Vec<u8>,
    severity: String,
    revision: i64,
    fence: i64,
    attempt_count: i64,
    expires_at: String,
    created_at: String,
}

fn reclaim_expired_notification_lease_page(
    transaction: &Transaction<'_>,
    now: &str,
    limit: i64,
) -> Result<usize, AppOwnerNotificationError> {
    transaction
        .execute(
            "UPDATE app_owner_notification_outbox \
             SET state = 'pending', revision = revision + 1, fence = fence + 1, \
                 available_at = CASE WHEN available_at > ?1 THEN available_at ELSE ?1 END, \
                 lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, \
                 updated_at = ?1 \
             WHERE correlation_id IN ( \
                       SELECT correlation_id FROM app_owner_notification_outbox \
                        WHERE state = 'leased' AND lease_expires_at <= ?1 AND expires_at > ?1 \
                        ORDER BY lease_expires_at, installation_id, workflow_id, port_id, \
                                 correlation_id LIMIT ?2 \
                   ) \
               AND state = 'leased' AND lease_expires_at <= ?1 AND expires_at > ?1 \
               AND COALESCE((SELECT paused FROM app_behavior_scope_policy \
                             WHERE singleton = 1), 0) = 0",
            params![now, limit],
        )
        .map_err(Into::into)
}

fn claim_notifications_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    authenticated_boundary: &AuthenticatedAppScope,
    worker_id: &str,
    limit: u16,
    lease_seconds: i64,
) -> Result<Vec<AppOwnerNotificationClaim>, AppOwnerNotificationError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Admission and SQLite busy waits happen before this point. Sample and
    // revalidate only after write ownership is established so returned leases
    // can never already be expired due to queue/lock delay.
    let now = Utc::now();
    if authenticated_boundary.scope() != scope
        || authenticated_boundary.ensure_live_at(&now).is_err()
    {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    let now_text = timestamp(now);
    let maintenance_limit = i64::from(limit);
    // Durable refund receipts are drained before terminal-attempt maintenance.
    // Any remaining receipt belongs to a live successor lease and fences that
    // correlation from a new claim until begin/settle consumes it or the lease
    // expires into a later bounded drain.
    apply_capacity_refund_page(&transaction, now.clone(), limit)?;
    reclaim_expired_notification_lease_page(&transaction, &now_text, maintenance_limit)?;
    transaction.execute(
        "UPDATE app_owner_notification_outbox \
         SET state = 'dead_letter', revision = revision + 1, fence = fence + 1, \
             lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, \
             last_error = 'notification_expired', updated_at = ?1 \
         WHERE correlation_id IN ( \
                   SELECT correlation_id FROM app_owner_notification_outbox \
                    WHERE state IN ('pending', 'leased') AND expires_at <= ?1 \
                    ORDER BY expires_at, fence, correlation_id LIMIT ?2 \
               ) \
           AND state IN ('pending', 'leased') AND expires_at <= ?1 \
           AND COALESCE((SELECT paused FROM app_behavior_scope_policy \
                         WHERE singleton = 1), 0) = 0",
        params![&now_text, maintenance_limit],
    )?;
    transaction.execute(
        "UPDATE app_owner_notification_outbox \
         SET state = 'dead_letter', revision = revision + 1, fence = fence + 1, \
             lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, \
             last_error = 'delivery_attempt_ceiling_exhausted', updated_at = ?1 \
         WHERE correlation_id IN ( \
                   SELECT correlation_id FROM app_owner_notification_outbox \
                    WHERE state = 'pending' AND available_at <= ?1 \
                      AND attempt_count >= ?2 \
                      AND NOT EXISTS ( \
                          SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds \
                           WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id \
                             AND refunds.applied_at IS NULL \
                      ) \
                    ORDER BY attempt_count, fence, available_at, created_at, correlation_id \
                    LIMIT ?3 \
               ) \
           AND state = 'pending' AND available_at <= ?1 AND attempt_count >= ?2 \
           AND NOT EXISTS ( \
               SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds \
                WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id \
                  AND refunds.applied_at IS NULL \
           ) \
           AND COALESCE((SELECT paused FROM app_behavior_scope_policy \
                         WHERE singleton = 1), 0) = 0",
        params![
            &now_text,
            i64::from(MAX_NOTIFICATION_DELIVERY_ATTEMPTS),
            maintenance_limit,
        ],
    )?;

    let rows = {
        let mut statement = transaction.prepare(
            "SELECT correlation_id, installation_id, installation_generation, workflow_id, \
                    port_id, reviewed_request_digest, effect_ref, payload_digest, payload_json, \
                    severity, revision, fence, attempt_count, expires_at, created_at \
             FROM app_owner_notification_outbox \
             WHERE state = 'pending' AND available_at <= ?1 AND expires_at > ?1 \
               AND attempt_count < ?2 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds \
                    WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id \
                      AND refunds.applied_at IS NULL \
               ) \
               AND COALESCE((SELECT paused FROM app_behavior_scope_policy \
                             WHERE singleton = 1), 0) = 0 \
             ORDER BY attempt_count ASC, fence ASC, available_at ASC,
                      created_at ASC, correlation_id ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                &now_text,
                i64::from(MAX_NOTIFICATION_DELIVERY_ATTEMPTS),
                i64::from(limit),
            ],
            |row| {
                Ok(ClaimRow {
                    correlation_id: row.get(0)?,
                    installation_id: row.get(1)?,
                    installation_generation: row.get(2)?,
                    workflow_id: row.get(3)?,
                    port_id: row.get(4)?,
                    reviewed_request_digest: row.get(5)?,
                    effect_ref: row.get(6)?,
                    payload_digest: row.get(7)?,
                    payload_json: row.get(8)?,
                    severity: row.get(9)?,
                    revision: row.get(10)?,
                    fence: row.get(11)?,
                    attempt_count: row.get(12)?,
                    expires_at: row.get(13)?,
                    created_at: row.get(14)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut prepared = Vec::with_capacity(rows.len());
    for row in rows {
        let claim = match parse_claim_row(scope, &transaction, &row) {
            Ok(value) => value,
            Err(AppOwnerNotificationError::AuthorityChanged) => {
                dead_letter_unclaimable_row(
                    &transaction,
                    &row,
                    "notification_authority_changed",
                    &now_text,
                )?;
                continue;
            },
            Err(AppOwnerNotificationError::CorruptRow)
            | Err(AppOwnerNotificationError::InvalidRequest(_))
            | Err(AppOwnerNotificationError::Contract(_))
            | Err(AppOwnerNotificationError::Json(_)) => {
                dead_letter_unclaimable_row(
                    &transaction,
                    &row,
                    "corrupt_notification_row",
                    &now_text,
                )?;
                continue;
            },
            Err(error) => return Err(error),
        };
        prepared.push((row, claim));
    }

    // Reopening every row's installation/grant authority precedes the lease
    // clock. The fixed page therefore receives one near-commit lease window;
    // early rows do not lose time while later payloads are parsed.
    let lease_now = Utc::now();
    if authenticated_boundary.scope() != scope
        || authenticated_boundary.ensure_live_at(&lease_now).is_err()
    {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    let lease_now_text = timestamp(lease_now);
    let requested_lease_expires_at = lease_now
        .checked_add_signed(Duration::seconds(lease_seconds))
        .ok_or(AppOwnerNotificationError::TimeOverflow)?;
    let mut claims = Vec::with_capacity(prepared.len());
    for (row, claim) in prepared {
        if claim.expires_at <= lease_now {
            dead_letter_unclaimable_row(
                &transaction,
                &row,
                "notification_expired",
                &lease_now_text,
            )?;
            continue;
        }
        let lease_expires_at = requested_lease_expires_at.min(claim.expires_at);
        let lease_expires_text = timestamp(lease_expires_at);
        let next_revision = row
            .revision
            .checked_add(1)
            .ok_or(AppOwnerNotificationError::CorruptRow)?;
        let next_fence = row
            .fence
            .checked_add(1)
            .ok_or(AppOwnerNotificationError::CorruptRow)?;
        let lease_token = lease_token(
            &row.correlation_id,
            worker_id,
            u64::try_from(next_fence).map_err(|_| AppOwnerNotificationError::CorruptRow)?,
            lease_now,
        )?;
        let changed = transaction.execute(
            "UPDATE app_owner_notification_outbox \
             SET state = 'leased', revision = ?1, fence = ?2, \
                 lease_owner = ?3, lease_token = ?4, lease_expires_at = ?5, updated_at = ?6 \
             WHERE correlation_id = ?7 AND state = 'pending' AND revision = ?8 AND fence = ?9 \
               AND available_at <= ?6 AND expires_at > ?6 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds \
                    WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id \
                      AND refunds.applied_at IS NULL \
               ) \
               AND COALESCE((SELECT paused FROM app_behavior_scope_policy \
                             WHERE singleton = 1), 0) = 0",
            params![
                next_revision,
                next_fence,
                worker_id,
                lease_token.as_str(),
                &lease_expires_text,
                &lease_now_text,
                &row.correlation_id,
                row.revision,
                row.fence,
            ],
        )?;
        if changed != 1 {
            continue;
        }
        claims.push(AppOwnerNotificationClaim {
            principal: scope.principal.as_str().to_owned(),
            workspace: scope.workspace.as_str().to_owned(),
            correlation_id: row.correlation_id,
            installation_id: claim.installation_id,
            installation_generation: claim.installation_generation,
            workflow_id: claim.workflow_id,
            port_id: claim.port_id,
            reviewed_request_digest: claim.reviewed_request_digest,
            payload: claim.payload,
            revision: u64::try_from(next_revision)
                .map_err(|_| AppOwnerNotificationError::CorruptRow)?,
            fence: u64::try_from(next_fence).map_err(|_| AppOwnerNotificationError::CorruptRow)?,
            attempt_count: u32::try_from(row.attempt_count)
                .map_err(|_| AppOwnerNotificationError::CorruptRow)?,
            lease_owner: worker_id.to_owned(),
            lease_token,
            lease_expires_at,
            expires_at: claim.expires_at,
            created_at: claim.created_at,
            timeout_secs: claim.timeout_secs,
        });
    }
    // The transaction can still spend time on up to 32 bounded CAS writes.
    // Revalidate both worker authority and every returned lease immediately
    // before commit; failure rolls the whole page back instead of returning a
    // lease that expired inside this transaction.
    let commit_now = Utc::now();
    if authenticated_boundary.scope() != scope
        || authenticated_boundary.ensure_live_at(&commit_now).is_err()
    {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    if !notification_claim_page_is_live(&claims, &commit_now) {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    transaction.commit()?;
    Ok(claims)
}

fn notification_claim_page_is_live(
    claims: &[AppOwnerNotificationClaim],
    now: &DateTime<Utc>,
) -> bool {
    claims.iter().all(|claim| &claim.lease_expires_at > now)
}

struct ParsedClaim {
    installation_id: AppInstallationId,
    installation_generation: u64,
    workflow_id: AppName,
    port_id: AppName,
    reviewed_request_digest: AppDigest,
    payload: StoredOwnerNotificationPayload,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    timeout_secs: u64,
}

fn parse_claim_row(
    scope: &AppScope,
    transaction: &Transaction<'_>,
    row: &ClaimRow,
) -> Result<ParsedClaim, AppOwnerNotificationError> {
    let installation_id = AppInstallationId::parse(row.installation_id.clone())?;
    let installation_generation = u64::try_from(row.installation_generation)
        .map_err(|_| AppOwnerNotificationError::CorruptRow)?;
    let workflow_id = AppName::parse(row.workflow_id.clone())?;
    let port_id = AppName::parse(row.port_id.clone())?;
    let reviewed_request_digest = AppDigest::parse(row.reviewed_request_digest.clone())?;
    let effect_ref = AppReference::parse(row.effect_ref.clone())?;
    let payload_digest = AppDigest::parse(row.payload_digest.clone())?;
    let payload: StoredOwnerNotificationPayload = serde_json::from_slice(&row.payload_json)?;
    validate_notification_text(payload.title.as_deref(), &payload.message)?;
    if AppDigest::blake3(&row.payload_json) != payload_digest
        || severity_text(payload.severity) != row.severity
        || payload.severity == AppNotificationSeverityV1::Critical
    {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let (_, grant) = load_live_installation_grant(
        transaction,
        scope,
        &installation_id,
        Some(installation_generation),
        None,
        None,
        None,
    )?;
    let live_port = grant
        .granted_notification_grants
        .iter()
        .find(|grant| grant.workflow_id == workflow_id && grant.port_id == port_id)
        .ok_or(AppOwnerNotificationError::AuthorityChanged)?;
    if live_port.reviewed_request_digest != reviewed_request_digest
        || live_port.kind != payload.kind
        || live_port.purpose != payload.purpose
        || severity_rank(payload.severity) > severity_rank(live_port.severity_ceiling)
    {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    let expected_correlation = notification_correlation_id_for_parts(
        scope,
        &installation_id,
        installation_generation,
        &workflow_id,
        &port_id,
        &reviewed_request_digest,
        &effect_ref,
        &payload.task_id,
        &payload.execution_id,
    )?;
    if expected_correlation != row.correlation_id {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let created_at = parse_timestamp(&row.created_at)?;
    let stored_expires_at = parse_timestamp(&row.expires_at)?;
    let current_grant_expires_at = created_at
        .checked_add_signed(Duration::seconds(
            i64::try_from(live_port.ttl_seconds)
                .map_err(|_| AppOwnerNotificationError::CorruptRow)?,
        ))
        .ok_or(AppOwnerNotificationError::CorruptRow)?;
    let expires_at = stored_expires_at.min(current_grant_expires_at);
    let lifetime_ms = (expires_at - created_at).num_milliseconds();
    if lifetime_ms <= 0 {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let timeout_secs = u64::try_from(lifetime_ms.saturating_add(999) / 1000)
        .map_err(|_| AppOwnerNotificationError::CorruptRow)?;
    Ok(ParsedClaim {
        installation_id,
        installation_generation,
        workflow_id,
        port_id,
        reviewed_request_digest,
        payload,
        expires_at,
        created_at,
        timeout_secs,
    })
}

fn dead_letter_unclaimable_row(
    transaction: &Transaction<'_>,
    row: &ClaimRow,
    reason: &'static str,
    now: &str,
) -> Result<(), AppOwnerNotificationError> {
    let changed = transaction.execute(
        "UPDATE app_owner_notification_outbox \
         SET state = 'dead_letter', revision = revision + 1, fence = fence + 1, \
             lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL, \
             last_error = ?1, updated_at = ?2 \
         WHERE correlation_id = ?3 AND state = 'pending' AND revision = ?4 AND fence = ?5",
        params![reason, now, &row.correlation_id, row.revision, row.fence,],
    )?;
    if changed != 1 {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    Ok(())
}

fn settle_notification_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    authenticated_boundary: &AuthenticatedAppScope,
    attempt: AppOwnerNotificationDeliveryAttempt,
) -> Result<AppOwnerNotificationSettlementReceipt, AppOwnerNotificationError> {
    if attempt.claim.principal != scope.principal.as_str()
        || attempt.claim.workspace != scope.workspace.as_str()
    {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Re-open wall-clock and authentication only after BEGIN IMMEDIATE has
    // crossed every admission/SQLite wait. The following lease predicate is
    // therefore evaluated at the actual commit boundary, not caller time.
    let now = Utc::now();
    if authenticated_boundary.scope() != scope
        || authenticated_boundary.ensure_live_at(&now).is_err()
    {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    let now_text = timestamp(now);
    let pending_refunds =
        pending_capacity_refund_count(&transaction, attempt.claim.correlation_id.as_str())?;
    let next_revision = attempt
        .claim
        .revision
        .checked_add(1)
        .ok_or(AppOwnerNotificationError::CorruptRow)?;
    let (state, status, available_at, user_request_id, last_error, delivered_at) =
        match attempt.disposition {
            DeliveryDisposition::Submitted { user_request_id } => (
                "delivered",
                AppOwnerNotificationSettlementStatus::Delivered,
                now,
                Some(user_request_id),
                None,
                Some(now_text.clone()),
            ),
            DeliveryDisposition::Expired => (
                "dead_letter",
                AppOwnerNotificationSettlementStatus::DeadLetter,
                now,
                None,
                Some("notification_expired".to_owned()),
                None,
            ),
            DeliveryDisposition::PermanentFailure => (
                "dead_letter",
                AppOwnerNotificationSettlementStatus::DeadLetter,
                now,
                None,
                Some("user_request_submission_refused".to_owned()),
                None,
            ),
        };
    let changed = transaction.execute(
        "UPDATE app_owner_notification_outbox \
         SET state = ?1, revision = ?2, attempt_count = attempt_count - ?8, \
             available_at = ?3, lease_owner = NULL, \
             lease_token = NULL, lease_expires_at = NULL, user_request_id = ?4, \
             last_error = ?5, delivered_at = ?6, updated_at = ?7 \
         WHERE correlation_id = ?9 AND state = 'leased' AND revision = ?10 AND fence = ?11 \
           AND lease_owner = ?12 AND lease_token = ?13 AND lease_expires_at > ?7 \
           AND attempt_count >= ?8",
        params![
            state,
            sqlite_i64(next_revision)?,
            timestamp(available_at),
            user_request_id,
            last_error,
            delivered_at,
            &now_text,
            i64::from(pending_refunds),
            &attempt.claim.correlation_id,
            sqlite_i64(attempt.claim.revision)?,
            sqlite_i64(attempt.claim.fence)?,
            &attempt.claim.lease_owner,
            attempt.claim.lease_token.as_str(),
        ],
    )?;
    if changed != 1 {
        return Err(AppOwnerNotificationError::LeaseStale);
    }
    mark_capacity_refunds_applied(
        &transaction,
        attempt.claim.correlation_id.as_str(),
        pending_refunds,
        &now_text,
    )?;
    transaction.commit()?;
    Ok(AppOwnerNotificationSettlementReceipt {
        correlation_id: attempt.claim.correlation_id,
        status,
    })
}

fn validate_notification_text(
    title: Option<&str>,
    message: &str,
) -> Result<(), AppOwnerNotificationError> {
    if message.trim().is_empty()
        || message.len() > MAX_NOTIFICATION_MESSAGE_BYTES
        || message.bytes().any(|byte| byte == 0)
    {
        return Err(AppOwnerNotificationError::InvalidRequest(
            "notification message is empty or outside its byte bound",
        ));
    }
    if title.is_some_and(|title| {
        title.trim().is_empty()
            || title.len() > MAX_NOTIFICATION_TITLE_BYTES
            || title.bytes().any(|byte| byte == 0)
    }) {
        return Err(AppOwnerNotificationError::InvalidRequest(
            "notification title is empty or outside its byte bound",
        ));
    }
    Ok(())
}

fn severity_rank(severity: AppNotificationSeverityV1) -> u8 {
    match severity {
        AppNotificationSeverityV1::Info => 0,
        AppNotificationSeverityV1::Warning => 1,
        AppNotificationSeverityV1::Critical => 2,
    }
}

fn severity_text(severity: AppNotificationSeverityV1) -> &'static str {
    match severity {
        AppNotificationSeverityV1::Info => "info",
        AppNotificationSeverityV1::Warning => "warning",
        AppNotificationSeverityV1::Critical => "critical",
    }
}

fn notification_kind_text(kind: AppNotificationKindV1) -> &'static str {
    match kind {
        AppNotificationKindV1::Briefing => "briefing",
        AppNotificationKindV1::Escalation => "escalation",
    }
}

fn render_notification(payload: &StoredOwnerNotificationPayload) -> String {
    match payload.title.as_deref() {
        Some(title) => format!("{title}\n\n{}", payload.message),
        None => payload.message.clone(),
    }
}

fn notification_terminal_retention_elapsed(
    expires_at: &DateTime<Utc>,
    period_end: &DateTime<Utc>,
    now: &DateTime<Utc>,
) -> bool {
    expires_at <= now && period_end <= now
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, AppOwnerNotificationError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AppOwnerNotificationError::CorruptRow)
}

fn sqlite_i64(value: u64) -> Result<i64, AppOwnerNotificationError> {
    i64::try_from(value).map_err(|_| AppOwnerNotificationError::TimeOverflow)
}

fn load_notification_compaction_page(
    transaction: &Transaction<'_>,
    now: DateTime<Utc>,
) -> Result<
    (
        TerminalCompactionRail,
        NotificationCompactionCursor,
        Vec<NotificationCompactionCandidate>,
    ),
    AppOwnerNotificationError,
> {
    transaction.execute(
        "INSERT OR IGNORE INTO app_terminal_compaction_cursors (
             candidate_kind, forward_pages_since_revisit, updated_at
         ) VALUES ('owner_notification', 0, ?1)",
        params![timestamp(now)],
    )?;
    let mut cursor = transaction.query_row(
        "SELECT forward_after_candidate_ref, forward_epoch_end_candidate_ref,
                revisit_after_candidate_ref, revisit_epoch_end_candidate_ref,
                forward_pages_since_revisit
           FROM app_terminal_compaction_cursors
          WHERE candidate_kind = 'owner_notification'",
        [],
        |row| {
            Ok(NotificationCompactionCursor {
                forward_after: row.get(0)?,
                forward_end: row.get(1)?,
                revisit_after: row.get(2)?,
                revisit_end: row.get(3)?,
                forward_pages_since_revisit: row.get(4)?,
            })
        },
    )?;
    if !(0..=TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT)
        .contains(&cursor.forward_pages_since_revisit)
    {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    let rail =
        if cursor.forward_pages_since_revisit >= TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT {
            TerminalCompactionRail::Revisit
        } else {
            TerminalCompactionRail::Forward
        };
    let end = match rail {
        TerminalCompactionRail::Forward => &mut cursor.forward_end,
        TerminalCompactionRail::Revisit => &mut cursor.revisit_end,
    };
    if end.is_none() {
        *end = transaction
            .query_row(
                "SELECT correlation_id
                   FROM app_owner_notification_outbox
                  ORDER BY correlation_id DESC
                  LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
    }
    let Some(end_correlation_id) = end.clone() else {
        return Ok((rail, cursor, Vec::new()));
    };
    let after = match rail {
        TerminalCompactionRail::Forward => cursor.forward_after.as_deref(),
        TerminalCompactionRail::Revisit => cursor.revisit_after.as_deref(),
    };
    let candidates = if let Some(after_correlation_id) = after {
        let mut statement = transaction.prepare(
            "SELECT correlation_id, installation_id, installation_generation,
                    workflow_id, port_id, reviewed_request_digest, period_seconds,
                    effect_ref, payload_digest, severity, state, expires_at,
                    created_at, payload_json
               FROM app_owner_notification_outbox
              WHERE correlation_id > ?1 AND correlation_id <= ?2
                AND NOT EXISTS (
                    SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds
                     WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id
                       AND refunds.applied_at IS NULL
                )
              ORDER BY correlation_id
              LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                after_correlation_id,
                end_correlation_id,
                NOTIFICATION_COMPACTION_LIMIT,
            ],
            |row| {
                Ok(NotificationCompactionCandidate {
                    correlation_id: row.get(0)?,
                    installation_id: row.get(1)?,
                    installation_generation: row.get(2)?,
                    workflow_id: row.get(3)?,
                    port_id: row.get(4)?,
                    reviewed_request_digest: row.get(5)?,
                    period_seconds: row.get(6)?,
                    effect_ref: row.get(7)?,
                    payload_digest: row.get(8)?,
                    severity: row.get(9)?,
                    state: row.get(10)?,
                    expires_at: row.get(11)?,
                    created_at: row.get(12)?,
                    payload_json: row.get(13)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        let mut statement = transaction.prepare(
            "SELECT correlation_id, installation_id, installation_generation,
                    workflow_id, port_id, reviewed_request_digest, period_seconds,
                    effect_ref, payload_digest, severity, state, expires_at,
                    created_at, payload_json
               FROM app_owner_notification_outbox
              WHERE correlation_id <= ?1
                AND NOT EXISTS (
                    SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds
                     WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id
                       AND refunds.applied_at IS NULL
                )
              ORDER BY correlation_id
              LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![end_correlation_id, NOTIFICATION_COMPACTION_LIMIT],
            |row| {
                Ok(NotificationCompactionCandidate {
                    correlation_id: row.get(0)?,
                    installation_id: row.get(1)?,
                    installation_generation: row.get(2)?,
                    workflow_id: row.get(3)?,
                    port_id: row.get(4)?,
                    reviewed_request_digest: row.get(5)?,
                    period_seconds: row.get(6)?,
                    effect_ref: row.get(7)?,
                    payload_digest: row.get(8)?,
                    severity: row.get(9)?,
                    state: row.get(10)?,
                    expires_at: row.get(11)?,
                    created_at: row.get(12)?,
                    payload_json: row.get(13)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    Ok((rail, cursor, candidates))
}

fn notification_compaction_is_idle_and_empty(
    connection: &rusqlite::Connection,
) -> Result<bool, AppOwnerNotificationError> {
    let idle_and_empty: i64 = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM app_owner_notification_outbox LIMIT 1)
                AND NOT EXISTS(
                    SELECT 1 FROM app_terminal_compaction_cursors
                     WHERE candidate_kind = 'owner_notification'
                       AND (forward_after_installation_id IS NOT NULL
                         OR forward_after_candidate_ref IS NOT NULL
                         OR forward_epoch_end_installation_id IS NOT NULL
                         OR forward_epoch_end_candidate_ref IS NOT NULL
                         OR revisit_after_installation_id IS NOT NULL
                         OR revisit_after_candidate_ref IS NOT NULL
                         OR revisit_epoch_end_installation_id IS NOT NULL
                         OR revisit_epoch_end_candidate_ref IS NOT NULL
                         OR forward_pages_since_revisit != 0)
                )",
        [],
        |row| row.get(0),
    )?;
    Ok(idle_and_empty == 1)
}

fn notification_compaction_raw_table_is_empty(
    connection: &rusqlite::Connection,
) -> Result<bool, AppOwnerNotificationError> {
    let empty: i64 = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM app_owner_notification_outbox LIMIT 1)",
        [],
        |row| row.get(0),
    )?;
    Ok(empty == 1)
}

fn reset_all_notification_compaction_rails(
    connection: &rusqlite::Connection,
    now: DateTime<Utc>,
) -> Result<(), AppOwnerNotificationError> {
    let changed = connection.execute(
        "UPDATE app_terminal_compaction_cursors
            SET forward_after_installation_id = NULL,
                forward_after_candidate_ref = NULL,
                forward_epoch_end_installation_id = NULL,
                forward_epoch_end_candidate_ref = NULL,
                revisit_after_installation_id = NULL,
                revisit_after_candidate_ref = NULL,
                revisit_epoch_end_installation_id = NULL,
                revisit_epoch_end_candidate_ref = NULL,
                forward_pages_since_revisit = 0, updated_at = ?1
          WHERE candidate_kind = 'owner_notification'
            AND (forward_after_installation_id IS NOT NULL
              OR forward_after_candidate_ref IS NOT NULL
              OR forward_epoch_end_installation_id IS NOT NULL
              OR forward_epoch_end_candidate_ref IS NOT NULL
              OR revisit_after_installation_id IS NOT NULL
              OR revisit_after_candidate_ref IS NOT NULL
              OR revisit_epoch_end_installation_id IS NOT NULL
              OR revisit_epoch_end_candidate_ref IS NOT NULL
              OR forward_pages_since_revisit != 0)",
        params![timestamp(now)],
    )?;
    if changed > 1 {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    Ok(())
}

fn reset_notification_compaction_rail(
    cursor: &mut NotificationCompactionCursor,
    rail: TerminalCompactionRail,
) {
    match rail {
        TerminalCompactionRail::Forward => {
            cursor.forward_after = None;
            cursor.forward_end = None;
        },
        TerminalCompactionRail::Revisit => {
            cursor.revisit_after = None;
            cursor.revisit_end = None;
            cursor.forward_pages_since_revisit = 0;
        },
    }
}

fn advance_notification_compaction_cursor(
    cursor: &mut NotificationCompactionCursor,
    rail: TerminalCompactionRail,
    candidates: &[NotificationCompactionCandidate],
) -> Result<(), AppOwnerNotificationError> {
    let key = candidates
        .last()
        .map(|candidate| candidate.correlation_id.clone())
        .ok_or(AppOwnerNotificationError::CorruptRow)?;
    match rail {
        TerminalCompactionRail::Forward => {
            cursor.forward_after = Some(key);
            cursor.forward_pages_since_revisit = cursor
                .forward_pages_since_revisit
                .checked_add(1)
                .ok_or(AppOwnerNotificationError::CorruptRow)?;
        },
        TerminalCompactionRail::Revisit => {
            cursor.revisit_after = Some(key);
            cursor.forward_pages_since_revisit = 0;
        },
    }
    Ok(())
}

fn store_notification_compaction_cursor(
    transaction: &Transaction<'_>,
    cursor: &NotificationCompactionCursor,
    now: DateTime<Utc>,
) -> Result<(), AppOwnerNotificationError> {
    let changed = transaction.execute(
        "UPDATE app_terminal_compaction_cursors
            SET forward_after_installation_id = NULL,
                forward_after_candidate_ref = ?1,
                forward_epoch_end_installation_id = NULL,
                forward_epoch_end_candidate_ref = ?2,
                revisit_after_installation_id = NULL,
                revisit_after_candidate_ref = ?3,
                revisit_epoch_end_installation_id = NULL,
                revisit_epoch_end_candidate_ref = ?4,
                forward_pages_since_revisit = ?5, updated_at = ?6
          WHERE candidate_kind = 'owner_notification'",
        params![
            cursor.forward_after.as_deref(),
            cursor.forward_end.as_deref(),
            cursor.revisit_after.as_deref(),
            cursor.revisit_end.as_deref(),
            cursor.forward_pages_since_revisit,
            timestamp(now),
        ],
    )?;
    if changed != 1 {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    Ok(())
}

fn notification_compaction_candidate_is_quarantined(
    transaction: &Transaction<'_>,
    installation_id: &str,
    correlation_id: &str,
) -> Result<bool, AppOwnerNotificationError> {
    transaction
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM app_terminal_compaction_quarantine
                  WHERE candidate_kind = 'owner_notification'
                    AND installation_id = ?1 AND candidate_ref = ?2
             )",
            params![installation_id, correlation_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn quarantine_notification_compaction_candidate(
    connection: &rusqlite::Connection,
    installation_id: &str,
    correlation_id: &str,
    reason_code: &'static str,
    now: DateTime<Utc>,
) -> Result<(), AppOwnerNotificationError> {
    connection.execute(
        "INSERT OR IGNORE INTO app_terminal_compaction_quarantine (
             candidate_kind, installation_id, candidate_ref, reason_code, quarantined_at
         ) VALUES ('owner_notification', ?1, ?2, ?3, ?4)",
        params![installation_id, correlation_id, reason_code, timestamp(now),],
    )?;
    let retained: Option<String> = connection
        .query_row(
            "SELECT reason_code FROM app_terminal_compaction_quarantine
              WHERE candidate_kind = 'owner_notification'
                AND installation_id = ?1 AND candidate_ref = ?2",
            params![installation_id, correlation_id],
            |row| row.get(0),
        )
        .optional()?;
    if retained.is_none() {
        return Err(AppOwnerNotificationError::CorruptRow);
    }
    Ok(())
}

fn is_compaction_integrity_sqlite_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn notification_correlation_id(
    scope: &AppScope,
    command: &AppOwnerNotificationCommand,
) -> Result<String, AppOwnerNotificationError> {
    notification_correlation_id_for_parts(
        scope,
        &command.installation_id,
        command.installation_generation,
        &command.workflow_id,
        &command.port_id,
        &command.grant.reviewed_request_digest,
        &command.effect_ref,
        &command.task_id,
        &command.execution_id,
    )
}

#[allow(clippy::too_many_arguments)]
fn notification_correlation_id_for_parts(
    scope: &AppScope,
    installation_id: &AppInstallationId,
    installation_generation: u64,
    workflow_id: &AppName,
    port_id: &AppName,
    reviewed_request_digest: &AppDigest,
    effect_ref: &AppReference,
    task_id: &str,
    execution_id: &str,
) -> Result<String, AppOwnerNotificationError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-owner-notification-correlation.v1",
        "principal": &scope.principal,
        "workspace": &scope.workspace,
        "installation_id": installation_id,
        "installation_generation": installation_generation,
        "workflow_id": workflow_id,
        "port_id": port_id,
        "reviewed_request_digest": reviewed_request_digest,
        "effect_ref": effect_ref,
        "task_id": task_id,
        "execution_id": execution_id,
    }))?
    .as_str()
    .to_owned())
}

fn lease_token(
    correlation_id: &str,
    worker_id: &str,
    fence: u64,
    now: DateTime<Utc>,
) -> Result<AppDigest, AppOwnerNotificationError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-owner-notification-lease.v1",
        "correlation_id": correlation_id,
        "worker_id": worker_id,
        "fence": fence,
        "claimed_at": timestamp(now),
        "nonce": uuid::Uuid::new_v4().to_string(),
    }))?)
}

fn load_live_installation_grant(
    connection: &rusqlite::Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    expected_generation: Option<u64>,
    expected_package_revision_ref: Option<&AppReference>,
    expected_grant_revision: Option<AppRevision>,
    expected_schema_revision: Option<AppRevision>,
) -> Result<(AppInstallation, AppGrantRevision), AppOwnerNotificationError> {
    let limits = AppContractLimits::default();
    let installation_bytes = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppOwnerNotificationError::AuthorityChanged)?;
    let installation: AppInstallation = decode_app_contract(&installation_bytes, &limits)?;
    let grant_revision = installation
        .grant_revision
        .ok_or(AppOwnerNotificationError::AuthorityChanged)?;
    if installation.installation_id != *installation_id
        || installation.scope != *scope
        || installation.lifecycle.status != AppInstallationStatus::Enabled
        || expected_generation
            .is_some_and(|generation| installation.lifecycle.generation != generation)
        || expected_package_revision_ref
            .is_some_and(|reference| &installation.package_revision_ref != reference)
        || expected_grant_revision.is_some_and(|revision| grant_revision != revision)
        || expected_schema_revision
            .is_some_and(|revision| installation.active_schema_revision != Some(revision))
    {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    let grant_bytes = connection
        .query_row(
            "SELECT record_json FROM app_grant_revisions \
             WHERE installation_id = ?1 AND revision = ?2 AND revoked_at IS NULL",
            params![installation_id.as_str(), sqlite_i64(grant_revision.get())?],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppOwnerNotificationError::AuthorityChanged)?;
    let grant: AppGrantRevision = decode_app_contract(&grant_bytes, &limits)?;
    if grant.installation_id != *installation_id
        || grant.revision != grant_revision
        || grant.package_revision_ref != installation.package_revision_ref
        || grant.revoked_at.is_some()
    {
        return Err(AppOwnerNotificationError::AuthorityChanged);
    }
    Ok((installation, grant))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn correlation_is_stable_and_effect_scoped() {
        let scope = AppScope {
            principal: AppReference::parse("owner").unwrap(),
            workspace: AppReference::parse("default").unwrap(),
        };
        let installation_id = AppInstallationId::parse("installation-1").unwrap();
        let workflow_id = AppName::parse("digest").unwrap();
        let port_id = AppName::parse("daily").unwrap();
        let review = AppDigest::blake3(b"review");
        let first_effect = AppReference::parse("effect:first").unwrap();
        let second_effect = AppReference::parse("effect:second").unwrap();
        let correlation = |effect| {
            notification_correlation_id_for_parts(
                &scope,
                &installation_id,
                4,
                &workflow_id,
                &port_id,
                &review,
                effect,
                "task-1",
                "execution-1",
            )
            .unwrap()
        };
        assert_eq!(correlation(&first_effect), correlation(&first_effect));
        assert_ne!(correlation(&first_effect), correlation(&second_effect));
        assert_ne!(
            correlation(&first_effect),
            notification_correlation_id_for_parts(
                &scope,
                &installation_id,
                5,
                &workflow_id,
                &port_id,
                &review,
                &first_effect,
                "task-1",
                "execution-1",
            )
            .unwrap(),
            "installation generation must be part of replay identity"
        );
        assert_ne!(
            correlation(&first_effect),
            notification_correlation_id_for_parts(
                &scope,
                &installation_id,
                4,
                &workflow_id,
                &port_id,
                &review,
                &first_effect,
                "task-1",
                "execution-2",
            )
            .unwrap(),
            "the sealed execution identity must be part of replay identity"
        );
    }

    #[test]
    fn user_request_is_absolute_ttl_bound_and_detached_from_workflow_response_routing() {
        let created_at = Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).single().unwrap();
        let expires_at = created_at + Duration::minutes(15);
        let claim = AppOwnerNotificationClaim {
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            correlation_id: "notification-correlation-1".to_owned(),
            installation_id: AppInstallationId::parse("installation-1").unwrap(),
            installation_generation: 7,
            workflow_id: AppName::parse("daily_digest").unwrap(),
            port_id: AppName::parse("briefing").unwrap(),
            reviewed_request_digest: AppDigest::blake3(b"review"),
            payload: StoredOwnerNotificationPayload {
                task_id: "task-1".to_owned(),
                execution_id: "execution-1".to_owned(),
                agent_id: "agent-1".to_owned(),
                title: Some("Daily briefing".to_owned()),
                message: "One new item.".to_owned(),
                purpose: "Keep the owner informed.".to_owned(),
                kind: AppNotificationKindV1::Briefing,
                severity: AppNotificationSeverityV1::Warning,
            },
            revision: 1,
            fence: 1,
            attempt_count: 0,
            lease_owner: "worker-1".to_owned(),
            lease_token: AppDigest::blake3(b"lease"),
            lease_expires_at: created_at + Duration::minutes(1),
            expires_at: expires_at.clone(),
            created_at: created_at.clone(),
            timeout_secs: 900,
        };

        let request = owner_notification_user_request(&claim);
        assert_eq!(request.id, claim.correlation_id);
        assert_eq!(request.source, "app_owner_notification");
        assert_eq!(request.timeout_secs, 900);
        assert_eq!(request.task_id, None);
        assert_eq!(request.execution_id, None);
        assert_eq!(request.context["app_owner_notification"], true);
        assert_eq!(request.context["one_way"], true);
        assert_eq!(request.context["task_id"], "task-1");
        assert_eq!(request.context["execution_id"], "execution-1");
        assert_eq!(
            request.context["absolute_expires_at_ms"],
            expires_at.timestamp_millis()
        );
    }

    #[test]
    fn terminal_payload_retention_waits_for_both_absolute_ttl_and_rate_period() {
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).single().unwrap();
        assert!(notification_terminal_retention_elapsed(&now, &now, &now));
        assert!(!notification_terminal_retention_elapsed(
            &(now + Duration::seconds(1)),
            &now,
            &now,
        ));
        assert!(!notification_terminal_retention_elapsed(
            &now,
            &(now + Duration::seconds(1)),
            &now,
        ));
    }

    /// Exactly the tables the retention rail and the delivery-maintenance rail
    /// are allowed to read. `paused` installs the owner's background-behavior
    /// policy row; passing `None` omits that table entirely, so a lane that
    /// grows a dependency on the unattended-execution posture fails here as a
    /// missing table instead of passing unnoticed.
    fn notification_maintenance_fixture(paused: Option<bool>) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, installation_id TEXT NOT NULL,
                     installation_generation INTEGER NOT NULL, workflow_id TEXT NOT NULL,
                     port_id TEXT NOT NULL, reviewed_request_digest TEXT NOT NULL,
                     period_seconds INTEGER NOT NULL, effect_ref TEXT NOT NULL,
                     payload_digest TEXT NOT NULL, severity TEXT NOT NULL,
                     state TEXT NOT NULL, revision INTEGER NOT NULL, fence INTEGER NOT NULL,
                     available_at TEXT NOT NULL, expires_at TEXT NOT NULL,
                     created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                     lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT,
                     payload_json BLOB NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 CREATE TABLE app_terminal_compaction_cursors (
                     candidate_kind TEXT PRIMARY KEY,
                     forward_after_installation_id TEXT,
                     forward_after_candidate_ref TEXT,
                     forward_epoch_end_installation_id TEXT,
                     forward_epoch_end_candidate_ref TEXT,
                     revisit_after_installation_id TEXT,
                     revisit_after_candidate_ref TEXT,
                     revisit_epoch_end_installation_id TEXT,
                     revisit_epoch_end_candidate_ref TEXT,
                     forward_pages_since_revisit INTEGER NOT NULL DEFAULT 0,
                     updated_at TEXT NOT NULL
                 );",
            )
            .unwrap();
        if let Some(paused) = paused {
            connection
                .execute_batch(
                    "CREATE TABLE app_behavior_scope_policy (
                         singleton INTEGER PRIMARY KEY, paused INTEGER NOT NULL
                     );",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO app_behavior_scope_policy VALUES (1, ?1)",
                    params![i64::from(paused)],
                )
                .unwrap();
        }
        connection
    }

    fn insert_maintenance_row(
        connection: &rusqlite::Connection,
        correlation_id: &str,
        state: &str,
        lease_expires_at: Option<&str>,
    ) {
        connection
            .execute(
                "INSERT INTO app_owner_notification_outbox (
                     correlation_id, installation_id, installation_generation, workflow_id,
                     port_id, reviewed_request_digest, period_seconds, effect_ref,
                     payload_digest, severity, state, revision, fence, available_at,
                     expires_at, created_at, updated_at, lease_owner, lease_token,
                     lease_expires_at, payload_json
                 ) VALUES (
                     ?1, 'installation-1', 4, 'daily_digest', 'briefing',
                     'blake3:0000000000000000000000000000000000000000000000000000000000000000',
                     86400, 'effect:tool-invocation-1',
                     'blake3:1111111111111111111111111111111111111111111111111111111111111111',
                     'info', ?2, 1, 1,
                     '2026-09-02T00:00:00.000000000Z', '2026-09-09T00:00:00.000000000Z',
                     '2026-09-02T00:00:00.000000000Z', '2026-09-02T00:00:00.000000000Z',
                     ?3, ?4, ?5, ?6
                 )",
                params![
                    correlation_id,
                    state,
                    lease_expires_at.map(|_| "worker-1"),
                    lease_expires_at.map(|_| {
                        "blake3:2222222222222222222222222222222222222222222222222222222222222222"
                    }),
                    lease_expires_at,
                    b"{}".to_vec(),
                ],
            )
            .unwrap();
    }

    /// Terminal-payload maintenance is an always-owned lane: arming or
    /// disarming the schedule/event boot master must not park protected
    /// retention. The fixture omits `app_behavior_scope_policy` on purpose, so
    /// any future read of the unattended-execution posture from this rail
    /// fails as a missing table rather than silently coupling the two.
    #[test]
    fn terminal_payload_retention_is_owned_independently_of_the_behavior_posture() {
        let mut connection = notification_maintenance_fixture(None);
        insert_maintenance_row(&connection, "notification-delivered", "delivered", None);
        assert!(
            !notification_compaction_is_idle_and_empty(&connection).unwrap(),
            "an outstanding terminal payload must never read as an idle rail",
        );

        let now = Utc.with_ymd_and_hms(2026, 10, 4, 0, 0, 0).single().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let (rail, cursor, candidates) =
            load_notification_compaction_page(&transaction, now).unwrap();
        assert!(matches!(rail, TerminalCompactionRail::Forward));
        assert_eq!(
            cursor.forward_end.as_deref(),
            Some("notification-delivered"),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].correlation_id, "notification-delivered");
        assert_eq!(candidates[0].state, "delivered");
    }

    /// The two lanes answer to different authorities, and pinning both halves
    /// in one place is what stops a later "pause everything" edit from turning
    /// bounded retention into unbounded retention: the owner's pause stands
    /// down attempt-bearing delivery maintenance and leaves retention running.
    #[test]
    fn behavior_pause_stands_down_delivery_maintenance_but_never_retention() {
        let mut connection = notification_maintenance_fixture(Some(true));
        insert_maintenance_row(
            &connection,
            "notification-leased",
            "leased",
            Some("2026-09-02T00:00:30.000000000Z"),
        );
        insert_maintenance_row(&connection, "notification-delivered", "delivered", None);

        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 1, 0).single().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            reclaim_expired_notification_lease_page(&transaction, &timestamp(now), 32).unwrap(),
            0,
            "a paused scope must not reclaim an expired delivery lease",
        );
        let (_, _, candidates) = load_notification_compaction_page(&transaction, now).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.correlation_id.as_str())
                .collect::<Vec<_>>(),
            vec!["notification-delivered", "notification-leased"],
            "retention paging must not consult the pause",
        );
    }

    /// Protected-retention work must not be orphaned by an unbounded forward
    /// sweep either. After `TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT`
    /// forward pages the rail owes one revisit page, and taking that page may
    /// not rewind where the forward epoch had already reached.
    #[test]
    fn terminal_retention_reserves_a_revisit_page_without_discarding_forward_progress() {
        let mut connection = notification_maintenance_fixture(None);
        insert_maintenance_row(&connection, "notification-a", "delivered", None);
        insert_maintenance_row(&connection, "notification-b", "dead_letter", None);
        connection
            .execute(
                "INSERT INTO app_terminal_compaction_cursors (
                     candidate_kind, forward_after_candidate_ref,
                     forward_epoch_end_candidate_ref, forward_pages_since_revisit, updated_at
                 ) VALUES ('owner_notification', 'notification-a', 'notification-b', ?1,
                           '2026-09-02T00:00:00.000000000Z')",
                params![TERMINAL_COMPACTION_FORWARD_PAGES_PER_REVISIT],
            )
            .unwrap();

        let now = Utc.with_ymd_and_hms(2026, 10, 4, 0, 0, 0).single().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let (rail, mut cursor, candidates) =
            load_notification_compaction_page(&transaction, now).unwrap();
        assert!(matches!(rail, TerminalCompactionRail::Revisit));
        assert_eq!(cursor.revisit_end.as_deref(), Some("notification-b"));
        assert_eq!(candidates.len(), 2, "the revisit rail opens its own epoch");

        advance_notification_compaction_cursor(&mut cursor, rail, &candidates).unwrap();
        assert_eq!(cursor.forward_pages_since_revisit, 0);
        assert_eq!(
            cursor.forward_after.as_deref(),
            Some("notification-a"),
            "taking the owed revisit page must not rewind the forward epoch",
        );
        assert_eq!(cursor.revisit_after.as_deref(), Some("notification-b"));
    }

    #[test]
    fn capacity_refund_backoff_progresses_caps_and_bounds_max_ttl_retries() {
        assert_eq!(capacity_refund_backoff_seconds(1).unwrap(), 5);
        assert_eq!(capacity_refund_backoff_seconds(2).unwrap(), 10);
        assert_eq!(capacity_refund_backoff_seconds(10).unwrap(), 2_560);
        assert_eq!(capacity_refund_backoff_seconds(11).unwrap(), 3_600);
        assert_eq!(capacity_refund_backoff_seconds(10_000).unwrap(), 3_600);

        // One receipt may be created immediately. Each later receipt requires
        // the preceding durable availability boundary to pass. With the
        // reviewed 31-day TTL, 5-second exponential prefix, and one-hour cap,
        // no serialized correlation can create more than 753 receipts.
        let mut receipts = 1_i64;
        let mut elapsed_seconds = 0_i64;
        let max_ttl_seconds = i64::try_from(APP_NOTIFICATION_MAX_TTL_SECONDS).unwrap();
        loop {
            let delay = capacity_refund_backoff_seconds(receipts).unwrap();
            if elapsed_seconds.saturating_add(delay) >= max_ttl_seconds {
                break;
            }
            elapsed_seconds += delay;
            receipts += 1;
        }
        assert_eq!(receipts, 753);
    }

    #[test]
    fn capacity_refund_retry_is_capped_at_absolute_expiry() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-expiry-cap', 2, 1, 'worker',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).single().unwrap();
        let expires_at = now + Duration::seconds(3);
        assert_eq!(
            capacity_refund_retry_at(&transaction, "notification-expiry-cap", now, expires_at,)
                .unwrap(),
            expires_at,
        );
    }

    #[test]
    fn expired_lease_reclaim_preserves_capacity_retry_availability() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_behavior_scope_policy (
                     singleton INTEGER PRIMARY KEY, paused INTEGER NOT NULL
                 );
                 CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, installation_id TEXT NOT NULL,
                     workflow_id TEXT NOT NULL, port_id TEXT NOT NULL,
                     state TEXT NOT NULL, revision INTEGER NOT NULL,
                     fence INTEGER NOT NULL, available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL, lease_owner TEXT,
                     lease_token TEXT, lease_expires_at TEXT, updated_at TEXT NOT NULL
                 );
                 INSERT INTO app_behavior_scope_policy VALUES (1, 0);
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-reclaim', 'installation', 'workflow', 'port',
                     'leased', 2, 1, '2026-09-02T00:30:00.000000000Z',
                     '2026-09-02T01:00:00.000000000Z', 'worker',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:30.000000000Z',
                     '2026-09-02T00:00:00.000000000Z'
                 );",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            reclaim_expired_notification_lease_page(
                &transaction,
                "2026-09-02T00:01:00.000000000Z",
                32,
            )
            .unwrap(),
            1,
        );
        let retained: (String, String, Option<String>) = transaction
            .query_row(
                "SELECT state, available_at, lease_owner
                   FROM app_owner_notification_outbox
                  WHERE correlation_id = 'notification-reclaim'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            retained,
            (
                "pending".to_owned(),
                "2026-09-02T00:30:00.000000000Z".to_owned(),
                None,
            )
        );
    }

    #[test]
    fn durable_capacity_refund_is_replay_safe_and_revives_only_ceiling_terminal() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY,
                     state TEXT NOT NULL,
                     revision INTEGER NOT NULL,
                     fence INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL,
                     available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     lease_owner TEXT,
                     lease_token TEXT,
                     lease_expires_at TEXT,
                     last_error TEXT,
                     updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL,
                     charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL,
                     charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL,
                     created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-1', 'dead_letter', 8, 2, 64,
                     '2026-09-02T00:00:00.000000000Z',
                     '2026-10-03T00:00:00.000000000Z', NULL, NULL, NULL,
                     'delivery_attempt_ceiling_exhausted',
                     '2026-09-02T00:00:00.000000000Z'
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-1', 7, 1, 'worker-1',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 1, 0).single().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            apply_capacity_refund_receipt(&transaction, "notification-1", 7, 1, now).unwrap(),
            Some((9, 63)),
        );
        assert_eq!(
            apply_capacity_refund_receipt(&transaction, "notification-1", 7, 1, now).unwrap(),
            Some((9, 63)),
            "an applied receipt must not decrement the attempt twice",
        );
        let state: (String, Option<String>, Option<String>, String) = transaction
            .query_row(
                "SELECT state, last_error,
                        (SELECT applied_at
                           FROM app_owner_notification_attempt_refunds
                          WHERE correlation_id = 'notification-1'),
                        available_at
                   FROM app_owner_notification_outbox
                  WHERE correlation_id = 'notification-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(state.0, "pending");
        assert_eq!(state.1, None);
        assert!(state.2.is_some());
        assert_eq!(state.3, "2026-09-02T00:01:05.000000000Z");
    }

    #[test]
    fn durable_capacity_refund_publication_rejects_same_key_owner_collision() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let token = "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        publish_capacity_refund_receipt(
            &transaction,
            "notification-identity",
            7,
            2,
            "worker-1",
            token,
            "2026-09-02T00:00:00.000000000Z",
        )
        .unwrap();
        publish_capacity_refund_receipt(
            &transaction,
            "notification-identity",
            7,
            2,
            "worker-1",
            token,
            "2026-09-02T00:00:01.000000000Z",
        )
        .unwrap();
        assert!(matches!(
            publish_capacity_refund_receipt(
                &transaction,
                "notification-identity",
                7,
                2,
                "worker-2",
                "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "2026-09-02T00:00:02.000000000Z",
            ),
            Err(AppOwnerNotificationError::LeaseStale)
        ));
        let rows: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM app_owner_notification_attempt_refunds",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn durable_capacity_refund_does_not_revoke_live_successor_lease() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, state TEXT NOT NULL,
                     revision INTEGER NOT NULL, fence INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL, available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT,
                     last_error TEXT, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-2', 'leased', 10, 3, 2,
                     '2026-09-02T00:00:00.000000000Z',
                     '2026-10-03T00:00:00.000000000Z', 'successor',
                     'blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     '2026-09-02T00:05:00.000000000Z', NULL,
                     '2026-09-02T00:00:00.000000000Z'
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-2', 7, 1, 'original',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 1, 0).single().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            apply_capacity_refund_receipt(&transaction, "notification-2", 7, 1, now).unwrap(),
            None,
        );
        let state: (i64, i64, Option<String>) = transaction
            .query_row(
                "SELECT revision, attempt_count,
                        (SELECT applied_at
                           FROM app_owner_notification_attempt_refunds
                          WHERE correlation_id = 'notification-2')
                   FROM app_owner_notification_outbox
                  WHERE correlation_id = 'notification-2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (10, 2, None));
    }

    #[test]
    fn exact_capacity_refund_returns_to_fenced_pending_backoff() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, state TEXT NOT NULL,
                     revision INTEGER NOT NULL, fence INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL, available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT,
                     last_error TEXT, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-exact', 'leased', 7, 2, 3,
                     '2026-09-02T00:00:00.000000000Z',
                     '2026-10-03T00:00:00.000000000Z', 'worker',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:05:00.000000000Z', NULL,
                     '2026-09-02T00:00:00.000000000Z'
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-exact', 7, 2, 'worker',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:01:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 1, 0).single().unwrap();
        assert_eq!(
            apply_capacity_refund_receipt(&transaction, "notification-exact", 7, 2, now).unwrap(),
            Some((8, 2)),
        );
        let state: (String, i64, String, Option<String>, Option<String>) = transaction
            .query_row(
                "SELECT outbox.state, outbox.fence, outbox.available_at,
                        outbox.lease_owner, refunds.applied_at
                   FROM app_owner_notification_outbox AS outbox
                   JOIN app_owner_notification_attempt_refunds AS refunds
                     ON refunds.correlation_id = outbox.correlation_id
                  WHERE outbox.correlation_id = 'notification-exact'",
                [],
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
            .unwrap();
        assert_eq!(state.0, "pending");
        assert_eq!(state.1, 3);
        assert_eq!(state.2, "2026-09-02T00:01:05.000000000Z");
        assert_eq!(state.3, None);
        assert!(state.4.is_some());
    }

    #[test]
    fn durable_capacity_refund_repairs_an_expired_successor_lease() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, state TEXT NOT NULL,
                     revision INTEGER NOT NULL, fence INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL, available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT,
                     last_error TEXT, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-expired', 'leased', 10, 3, 2,
                     '2026-09-02T00:00:00.000000000Z',
                     '2026-10-03T00:00:00.000000000Z', 'successor',
                     'blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     '2026-09-02T00:00:30.000000000Z', NULL,
                     '2026-09-02T00:00:00.000000000Z'
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-expired', 7, 1, 'original',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 1, 0).single().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert_eq!(
            apply_capacity_refund_receipt(&transaction, "notification-expired", 7, 1, now,)
                .unwrap(),
            Some((11, 1)),
        );
        let state: (String, i64, i64, Option<String>, String) = transaction
            .query_row(
                "SELECT state, fence, attempt_count, lease_owner, available_at
                   FROM app_owner_notification_outbox
                  WHERE correlation_id = 'notification-expired'",
                [],
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
            .unwrap();
        assert_eq!(state.0, "pending");
        assert_eq!(state.1, 4);
        assert_eq!(state.2, 1);
        assert_eq!(state.3, None);
        assert_eq!(state.4, "2026-09-02T00:01:05.000000000Z");
    }

    #[test]
    fn successor_begin_and_settlement_consume_refunds_in_their_own_cas() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, state TEXT NOT NULL,
                     revision INTEGER NOT NULL, fence INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL, available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT,
                     last_error TEXT, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-successor', 'leased', 10, 3, 2,
                     '2026-09-02T00:00:00.000000000Z',
                     '2026-10-03T00:00:00.000000000Z', 'successor',
                     'blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     '2026-09-02T00:05:00.000000000Z', NULL,
                     '2026-09-02T00:00:00.000000000Z'
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-successor', 7, 1, 'original',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let begin_refunds =
            pending_capacity_refund_count(&transaction, "notification-successor").unwrap();
        assert_eq!(begin_refunds, 1);
        assert_eq!(
            transaction
                .execute(
                    "UPDATE app_owner_notification_outbox
                        SET revision = revision + 1,
                            attempt_count = attempt_count + 1 - ?1
                      WHERE correlation_id = 'notification-successor'
                        AND state = 'leased' AND revision = 10 AND fence = 3
                        AND attempt_count >= ?1",
                    params![i64::from(begin_refunds)],
                )
                .unwrap(),
            1,
        );
        mark_capacity_refunds_applied(
            &transaction,
            "notification-successor",
            begin_refunds,
            "2026-09-02T00:01:00.000000000Z",
        )
        .unwrap();
        publish_capacity_refund_receipt(
            &transaction,
            "notification-successor",
            11,
            3,
            "successor",
            "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "2026-09-02T00:01:01.000000000Z",
        )
        .unwrap();
        let settle_refunds =
            pending_capacity_refund_count(&transaction, "notification-successor").unwrap();
        assert_eq!(settle_refunds, 1);
        assert_eq!(
            transaction
                .execute(
                    "UPDATE app_owner_notification_outbox
                        SET state = 'delivered', revision = revision + 1,
                            attempt_count = attempt_count - ?1,
                            lease_owner = NULL, lease_token = NULL,
                            lease_expires_at = NULL
                      WHERE correlation_id = 'notification-successor'
                        AND state = 'leased' AND revision = 11 AND fence = 3
                        AND attempt_count >= ?1",
                    params![i64::from(settle_refunds)],
                )
                .unwrap(),
            1,
        );
        mark_capacity_refunds_applied(
            &transaction,
            "notification-successor",
            settle_refunds,
            "2026-09-02T00:01:02.000000000Z",
        )
        .unwrap();
        let state: (String, i64, i64) = transaction
            .query_row(
                "SELECT state, revision, attempt_count
                   FROM app_owner_notification_outbox
                  WHERE correlation_id = 'notification-successor'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, ("delivered".to_owned(), 12, 1));
    }

    #[test]
    fn pending_refund_fences_claim_until_bounded_drain_applies_it() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY, state TEXT NOT NULL,
                     revision INTEGER NOT NULL, fence INTEGER NOT NULL,
                     attempt_count INTEGER NOT NULL, available_at TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     lease_owner TEXT, lease_token TEXT, lease_expires_at TEXT,
                     last_error TEXT, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_attempt_refunds (
                     correlation_id TEXT NOT NULL, charged_revision INTEGER NOT NULL,
                     charged_fence INTEGER NOT NULL, charged_lease_owner TEXT NOT NULL,
                     charged_lease_token TEXT NOT NULL, created_at TEXT NOT NULL,
                     applied_at TEXT,
                     PRIMARY KEY(correlation_id, charged_revision, charged_fence)
                 );
                 INSERT INTO app_owner_notification_outbox VALUES (
                     'notification-pending', 'pending', 8, 2, 1,
                     '2026-09-02T00:00:00.000000000Z',
                     '2026-10-03T00:00:00.000000000Z', NULL, NULL, NULL, NULL,
                     '2026-09-02T00:00:00.000000000Z'
                 );
                 INSERT INTO app_owner_notification_attempt_refunds VALUES (
                     'notification-pending', 7, 1, 'original',
                     'blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     '2026-09-02T00:00:00.000000000Z', NULL
                 );",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let visible_before: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM app_owner_notification_outbox
                  WHERE state = 'pending'
                    AND NOT EXISTS (
                        SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds
                         WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id
                           AND refunds.applied_at IS NULL
                    )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(visible_before, 0);
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 0, 1, 0).single().unwrap();
        apply_capacity_refund_page(&transaction, now, 32).unwrap();
        let visible_after: (i64, i64, Option<String>) = transaction
            .query_row(
                "SELECT COUNT(*), MAX(attempt_count), MAX(available_at)
                   FROM app_owner_notification_outbox
                  WHERE state = 'pending'
                    AND NOT EXISTS (
                        SELECT 1 FROM app_owner_notification_attempt_refunds AS refunds
                         WHERE refunds.correlation_id = app_owner_notification_outbox.correlation_id
                           AND refunds.applied_at IS NULL
                    )",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(visible_after.0, 1);
        assert_eq!(visible_after.1, 0);
        assert_eq!(
            visible_after.2.as_deref(),
            Some("2026-09-02T00:01:05.000000000Z")
        );
    }

    #[test]
    fn final_claim_page_liveness_rejects_a_lease_expired_during_the_page() {
        let lease_expires_at = Utc.with_ymd_and_hms(2026, 9, 2, 0, 5, 0).single().unwrap();
        let claim = AppOwnerNotificationClaim {
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            correlation_id: "notification-liveness".to_owned(),
            installation_id: AppInstallationId::parse("installation-1").unwrap(),
            installation_generation: 1,
            workflow_id: AppName::parse("workflow").unwrap(),
            port_id: AppName::parse("port").unwrap(),
            reviewed_request_digest: AppDigest::blake3(b"review"),
            payload: StoredOwnerNotificationPayload {
                task_id: "task".to_owned(),
                execution_id: "execution".to_owned(),
                agent_id: "agent".to_owned(),
                title: None,
                message: "message".to_owned(),
                purpose: "purpose".to_owned(),
                kind: AppNotificationKindV1::Briefing,
                severity: AppNotificationSeverityV1::Info,
            },
            revision: 2,
            fence: 1,
            attempt_count: 0,
            lease_owner: "worker".to_owned(),
            lease_token: AppDigest::blake3(b"lease"),
            lease_expires_at: lease_expires_at.clone(),
            expires_at: lease_expires_at.clone() + Duration::minutes(1),
            created_at: lease_expires_at.clone() - Duration::minutes(1),
            timeout_secs: 120,
        };
        assert!(notification_claim_page_is_live(
            std::slice::from_ref(&claim),
            &(lease_expires_at - Duration::nanoseconds(1)),
        ));
        assert!(!notification_claim_page_is_live(
            std::slice::from_ref(&claim),
            &lease_expires_at,
        ));
    }
}
