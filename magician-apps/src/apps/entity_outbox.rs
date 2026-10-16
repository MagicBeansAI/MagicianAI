//! Move-only delivery leases for canonical app entity-change events.
//!
//! Mutation/import/forget commits append to `app_entity_outbox` atomically.
//! This owner is the only consumer seam: claims, acknowledgements and retries
//! are scoped, bounded, crash-reclaimable and joined to the exact stored event
//! bytes before state advances.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use uuid::Uuid;

use super::{
    authority::AuthenticatedAppScope,
    models::{
        decode_bounded_json_value, AppContractLimits, AppDigest, AppInstallationId, AppReference,
    },
    registry::{AppRegistryError, AppRegistryService},
};

const MAX_ENTITY_OUTBOX_CLAIM_BATCH: usize = 64;
const MAX_ENTITY_OUTBOX_LEASE: StdDuration = StdDuration::from_secs(5 * 60);
const MAX_ENTITY_OUTBOX_RETRY_DELAY: StdDuration = StdDuration::from_secs(60 * 60);

#[derive(Debug)]
pub struct AppEntityOutboxEvent {
    event_id: AppReference,
    installation_id: AppInstallationId,
    first_change_sequence: u64,
    last_change_sequence: u64,
    payload: Value,
    payload_digest: AppDigest,
}

impl AppEntityOutboxEvent {
    pub fn event_id(&self) -> &AppReference {
        &self.event_id
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn first_change_sequence(&self) -> u64 {
        self.first_change_sequence
    }

    pub fn last_change_sequence(&self) -> u64 {
        self.last_change_sequence
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }

    pub fn payload_digest(&self) -> &AppDigest {
        &self.payload_digest
    }
}

/// Move-only, non-deserializable lease. Private token and sequence fields keep
/// transport input from fabricating an acknowledgement.
#[derive(Debug)]
pub struct AppEntityOutboxLease {
    sequence: i64,
    event: AppEntityOutboxEvent,
    lease_owner: AppReference,
    lease_token: AppReference,
    lease_expires_at: DateTime<Utc>,
    attempt_count: u32,
}

impl AppEntityOutboxLease {
    pub fn event(&self) -> &AppEntityOutboxEvent {
        &self.event
    }

    pub fn lease_owner(&self) -> &AppReference {
        &self.lease_owner
    }

    pub fn lease_expires_at(&self) -> &DateTime<Utc> {
        &self.lease_expires_at
    }

    pub fn attempt_count(&self) -> u32 {
        self.attempt_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEntityOutboxAcknowledgeOutcome {
    Delivered,
    AlreadyDelivered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntityOutboxAcknowledgeReceipt {
    pub event_id: AppReference,
    pub delivery_receipt_id: AppReference,
    pub outcome: AppEntityOutboxAcknowledgeOutcome,
}

#[allow(async_fn_in_trait)]
pub trait AppRegistryEntityOutboxExt {
    async fn claim_entity_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: AppReference,
        limit: usize,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppEntityOutboxLease>, AppRegistryError>;
    async fn acknowledge_entity_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppEntityOutboxLease,
        delivery_receipt_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppEntityOutboxAcknowledgeReceipt, AppRegistryError>;
    async fn release_entity_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppEntityOutboxLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppRegistryError>;
}

impl AppRegistryEntityOutboxExt for AppRegistryService {
    async fn claim_entity_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: AppReference,
        limit: usize,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppEntityOutboxLease>, AppRegistryError> {
        if limit == 0 || limit > MAX_ENTITY_OUTBOX_CLAIM_BATCH {
            return Err(AppRegistryError::InvalidControlPlane(format!(
                "entity outbox claim limit must be between 1 and {MAX_ENTITY_OUTBOX_CLAIM_BATCH}"
            )));
        }
        if lease_duration < StdDuration::from_secs(1) || lease_duration > MAX_ENTITY_OUTBOX_LEASE {
            return Err(AppRegistryError::InvalidControlPlane(
                "entity outbox lease must be between one second and five minutes".to_owned(),
            ));
        }
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            claim_entity_outbox_blocking(connection, lease_owner, limit, lease_duration, &now)
        })
        .await
    }

    async fn acknowledge_entity_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppEntityOutboxLease,
        delivery_receipt_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppEntityOutboxAcknowledgeReceipt, AppRegistryError> {
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            acknowledge_entity_outbox_blocking(connection, lease, delivery_receipt_id, &now)
        })
        .await
    }

    async fn release_entity_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppEntityOutboxLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppRegistryError> {
        if retry_delay > MAX_ENTITY_OUTBOX_RETRY_DELAY {
            return Err(AppRegistryError::InvalidControlPlane(
                "entity outbox retry delay cannot exceed one hour".to_owned(),
            ));
        }
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            release_entity_outbox_blocking(connection, lease, retry_delay, &now)
        })
        .await
    }
}

fn claim_entity_outbox_blocking(
    connection: &mut rusqlite::Connection,
    lease_owner: AppReference,
    limit: usize,
    lease_duration: StdDuration,
    now: &DateTime<Utc>,
) -> Result<Vec<AppEntityOutboxLease>, AppRegistryError> {
    let lease_duration = Duration::from_std(lease_duration).map_err(|_| {
        AppRegistryError::InvalidControlPlane("entity outbox lease duration overflow".to_owned())
    })?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("entity outbox lease expiry overflow".to_owned())
    })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let max_payload_bytes = i64::try_from(AppContractLimits::default().max_document_bytes())
        .map_err(|_| {
            AppRegistryError::InvalidControlPlane("entity outbox payload limit overflow".to_owned())
        })?;
    let corrupt_ready_rows: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_entity_outbox
          WHERE delivered_at IS NULL AND available_at <= ?1
            AND (delivery_state = 'pending'
                 OR (delivery_state = 'leased' AND lease_expires_at <= ?1))
            AND (length(event_id) NOT BETWEEN 1 AND 192
                 OR length(installation_id) NOT BETWEEN 1 AND 128
                 OR length(payload_json) NOT BETWEEN 1 AND ?2)",
        params![timestamp(now), max_payload_bytes],
        |row| row.get(0),
    )?;
    if corrupt_ready_rows != 0 {
        return Err(AppRegistryError::InvalidControlPlane(
            "entity outbox contains an unbounded ready row".to_owned(),
        ));
    }
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT sequence, event_id, installation_id, first_change_seq,
                    last_change_seq, payload_json, attempt_count
               FROM app_entity_outbox
              WHERE delivered_at IS NULL AND available_at <= ?1
                AND (delivery_state = 'pending'
                     OR (delivery_state = 'leased' AND lease_expires_at <= ?1))
                AND length(event_id) BETWEEN 1 AND 192
                AND length(installation_id) BETWEEN 1 AND 128
                AND length(payload_json) BETWEEN 1 AND ?2
              ORDER BY sequence ASC LIMIT ?3",
        )?;
        let rows = statement
            .query_map(
                params![timestamp(now), max_payload_bytes, usize_i64(limit)?],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut leases = Vec::with_capacity(rows.len());
    for (sequence, event_id, installation_id, first, last, payload_bytes, prior_attempts) in rows {
        let event =
            decode_entity_outbox_event(event_id, installation_id, first, last, &payload_bytes)?;
        let lease_token =
            AppReference::parse(format!("entity-outbox-lease:{}", Uuid::new_v4().simple()))?;
        let updated = transaction.execute(
            "UPDATE app_entity_outbox
                SET delivery_state = 'leased', lease_owner = ?1, lease_token = ?2,
                    lease_expires_at = ?3, attempt_count = attempt_count + 1
              WHERE sequence = ?4 AND delivered_at IS NULL AND available_at <= ?5
                AND (delivery_state = 'pending'
                     OR (delivery_state = 'leased' AND lease_expires_at <= ?5))",
            params![
                lease_owner.as_str(),
                lease_token.as_str(),
                timestamp(&lease_expires_at),
                sequence,
                timestamp(now),
            ],
        )?;
        if updated != 1 {
            return Err(AppRegistryError::CompareAndSwapLost("entity outbox claim"));
        }
        let attempt_count = prior_attempts
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "entity outbox attempt count exceeds supported range".to_owned(),
                )
            })?;
        leases.push(AppEntityOutboxLease {
            sequence,
            event,
            lease_owner: lease_owner.clone(),
            lease_token,
            lease_expires_at,
            attempt_count,
        });
    }
    transaction.commit()?;
    Ok(leases)
}

fn acknowledge_entity_outbox_blocking(
    connection: &mut rusqlite::Connection,
    lease: AppEntityOutboxLease,
    delivery_receipt_id: AppReference,
    now: &DateTime<Utc>,
) -> Result<AppEntityOutboxAcknowledgeReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = transaction
        .query_row(
            "SELECT installation_id, first_change_seq, last_change_seq, payload_json,
                    delivery_state, lease_owner, lease_token, lease_expires_at,
                    delivery_receipt_id, delivered_at
               FROM app_entity_outbox WHERE sequence = ?1 AND event_id = ?2",
            params![lease.sequence, lease.event.event_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        installation_id,
        first,
        last,
        payload,
        state,
        owner,
        token,
        expiry,
        receipt,
        delivered,
    )) = stored
    else {
        return Err(AppRegistryError::MissingRecord {
            entity: "entity outbox event",
            identity: lease.event.event_id.to_string(),
        });
    };
    let stored_event = decode_entity_outbox_event(
        lease.event.event_id.to_string(),
        installation_id,
        first,
        last,
        &payload,
    )?;
    if !same_entity_event(&stored_event, &lease.event) {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    if state == "delivered" {
        if receipt.as_deref() == Some(delivery_receipt_id.as_str()) && delivered.is_some() {
            return Ok(AppEntityOutboxAcknowledgeReceipt {
                event_id: lease.event.event_id,
                delivery_receipt_id,
                outcome: AppEntityOutboxAcknowledgeOutcome::AlreadyDelivered,
            });
        }
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    let expected_lease_expiry = timestamp(&lease.lease_expires_at);
    if now >= &lease.lease_expires_at
        || owner.as_deref() != Some(lease.lease_owner.as_str())
        || token.as_deref() != Some(lease.lease_token.as_str())
        || expiry.as_deref() != Some(expected_lease_expiry.as_str())
    {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    let updated = transaction.execute(
        "UPDATE app_entity_outbox
            SET delivery_state = 'delivered', delivery_receipt_id = ?1,
                delivered_at = ?2, lease_owner = NULL, lease_token = NULL,
                lease_expires_at = NULL
          WHERE sequence = ?3 AND event_id = ?4 AND delivery_state = 'leased'
            AND lease_owner = ?5 AND lease_token = ?6 AND lease_expires_at = ?7
            AND delivered_at IS NULL",
        params![
            delivery_receipt_id.as_str(),
            timestamp(now),
            lease.sequence,
            lease.event.event_id.as_str(),
            lease.lease_owner.as_str(),
            lease.lease_token.as_str(),
            expected_lease_expiry,
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    transaction.commit()?;
    Ok(AppEntityOutboxAcknowledgeReceipt {
        event_id: lease.event.event_id,
        delivery_receipt_id,
        outcome: AppEntityOutboxAcknowledgeOutcome::Delivered,
    })
}

fn release_entity_outbox_blocking(
    connection: &mut rusqlite::Connection,
    lease: AppEntityOutboxLease,
    retry_delay: StdDuration,
    now: &DateTime<Utc>,
) -> Result<(), AppRegistryError> {
    if now >= &lease.lease_expires_at {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    let retry_delay = Duration::from_std(retry_delay).map_err(|_| {
        AppRegistryError::InvalidControlPlane("entity outbox retry delay overflow".to_owned())
    })?;
    let available_at = now.checked_add_signed(retry_delay).ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("entity outbox retry timestamp overflow".to_owned())
    })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let updated = transaction.execute(
        "UPDATE app_entity_outbox
            SET delivery_state = 'pending', available_at = ?1,
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE sequence = ?2 AND event_id = ?3 AND delivery_state = 'leased'
            AND lease_owner = ?4 AND lease_token = ?5 AND lease_expires_at = ?6
            AND delivered_at IS NULL",
        params![
            timestamp(&available_at),
            lease.sequence,
            lease.event.event_id.as_str(),
            lease.lease_owner.as_str(),
            lease.lease_token.as_str(),
            timestamp(&lease.lease_expires_at),
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    transaction.commit()?;
    Ok(())
}

fn decode_entity_outbox_event(
    event_id: String,
    installation_id: String,
    first: i64,
    last: i64,
    payload_bytes: &[u8],
) -> Result<AppEntityOutboxEvent, AppRegistryError> {
    let first_change_sequence = positive_u64(first)?;
    let last_change_sequence = positive_u64(last)?;
    if last_change_sequence < first_change_sequence {
        return Err(AppRegistryError::InvalidControlPlane(
            "entity outbox sequence range is corrupt".to_owned(),
        ));
    }
    let payload = decode_bounded_json_value(payload_bytes, &AppContractLimits::default())?;
    let payload_digest = AppDigest::blake3(payload_bytes);
    Ok(AppEntityOutboxEvent {
        event_id: AppReference::parse(event_id)?,
        installation_id: AppInstallationId::parse(installation_id)?,
        first_change_sequence,
        last_change_sequence,
        payload,
        payload_digest,
    })
}

fn same_entity_event(left: &AppEntityOutboxEvent, right: &AppEntityOutboxEvent) -> bool {
    left.event_id == right.event_id
        && left.installation_id == right.installation_id
        && left.first_change_sequence == right.first_change_sequence
        && left.last_change_sequence == right.last_change_sequence
        && left.payload_digest == right.payload_digest
}

fn positive_u64(value: i64) -> Result<u64, AppRegistryError> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            AppRegistryError::InvalidControlPlane(
                "entity outbox sequence is outside the supported range".to_owned(),
            )
        })
}

fn usize_i64(value: usize) -> Result<i64, AppRegistryError> {
    i64::try_from(value).map_err(|_| {
        AppRegistryError::InvalidControlPlane("entity outbox limit overflow".to_owned())
    })
}

fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::entity_changes::AppEntityChangeSignal;
    use magician::magician_v2::{
        apps::{
            models::AppRevision,
            registry::tests::{authenticated_scope, canonical_tempdir, publication, time},
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    async fn seeded_entity_outbox() -> (tempfile::TempDir, AppRegistryService, AuthenticatedAppScope)
    {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        service
            .publish_ready_for_review(
                &authenticated,
                publication(
                    &ArtifactV2Workspace::new(temporary.path()),
                    "anonymous",
                    "default",
                    "attempt:entity-outbox",
                    "install_entity_outbox",
                ),
                time(1),
            )
            .await
            .unwrap();
        service
            .execute_scoped_write(&authenticated, &time(2), |connection, _| {
                connection.execute(
                    "INSERT INTO app_entity_outbox (
                         event_id, installation_id, first_change_seq, last_change_seq,
                         payload_json, delivery_state, available_at, lease_token,
                         lease_expires_at, created_at, delivered_at
                     ) VALUES (?1, ?2, 1, 1, ?3, 'pending', ?4, NULL, NULL, ?4, NULL)",
                    params![
                        "app-entity-change:test",
                        "install_entity_outbox",
                        serde_json::to_vec(&serde_json::json!({
                            "committed_record_revisions": [{
                                "entity": "item",
                                "record_id": "record_1",
                                "revision": 1
                            }]
                        }))?,
                        time(2).to_rfc3339(),
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        (temporary, service, authenticated)
    }

    #[test]
    fn rebuild_delivery_is_an_explicit_body_free_reset_signal() {
        let event = AppEntityOutboxEvent {
            event_id: AppReference::parse("app-forget-rebuild:receipt:test").unwrap(),
            installation_id: AppInstallationId::parse("install_entity_outbox").unwrap(),
            first_change_sequence: 7,
            last_change_sequence: 9,
            payload: serde_json::json!({
                "installation_id": "install_entity_outbox",
                "receipt_ref": "receipt:test",
                "dataset_generation": 2,
                "rebuild_all": true
            }),
            payload_digest: AppDigest::blake3(b"rebuild"),
        };
        let signal =
            AppEntityChangeSignal::from_outbox_event(&event, AppRevision::new(4).unwrap()).unwrap();
        assert!(signal.reset_required);
        assert!(signal.changes.is_empty());
        assert_eq!(signal.first_change_sequence, 7);
        assert_eq!(signal.last_change_sequence, 9);
    }

    #[test]
    fn import_delivery_projects_only_contiguous_record_identities() {
        let event = AppEntityOutboxEvent {
            event_id: AppReference::parse("app-import-change:receipt:test").unwrap(),
            installation_id: AppInstallationId::parse("install_entity_outbox").unwrap(),
            first_change_sequence: 10,
            last_change_sequence: 11,
            payload: serde_json::json!({
                "receipt_ref": "receipt:test",
                "installation_id": "install_entity_outbox",
                "source_archive_digest": AppDigest::blake3(b"archive"),
                "records": [
                    {"entity": "item", "record_id": "record_1", "revision": 2},
                    {"entity": "item", "record_id": "record_2", "revision": 1}
                ]
            }),
            payload_digest: AppDigest::blake3(b"import"),
        };
        let signal =
            AppEntityChangeSignal::from_outbox_event(&event, AppRevision::new(4).unwrap()).unwrap();
        assert!(!signal.reset_required);
        assert_eq!(signal.changes.len(), 2);
        assert_eq!(signal.changes[0].change_sequence, 10);
        assert_eq!(signal.changes[1].change_sequence, 11);
        let encoded = serde_json::to_string(&signal).unwrap();
        assert!(!encoded.contains("source_archive_digest"));
    }

    #[test]
    fn unknown_delivery_kind_cannot_be_misclassified_as_a_rebuild_and_acknowledged() {
        let event = AppEntityOutboxEvent {
            event_id: AppReference::parse("app-future-change:receipt:test").unwrap(),
            installation_id: AppInstallationId::parse("install_entity_outbox").unwrap(),
            first_change_sequence: 12,
            last_change_sequence: 12,
            payload: serde_json::json!({
                "installation_id": "install_entity_outbox",
                "future_semantics": true
            }),
            payload_digest: AppDigest::blake3(b"future"),
        };
        assert!(matches!(
            AppEntityChangeSignal::from_outbox_event(&event, AppRevision::new(4).unwrap(),),
            Err(
                super::super::entity_changes::AppEntityChangeError::CorruptHistory(
                    "unsupported entity outbox event kind"
                )
            )
        ));
    }

    #[test]
    fn durable_mutation_identity_and_range_mismatch_fail_closed() {
        let mismatched_identity = AppEntityOutboxEvent {
            event_id: AppReference::parse("app-entity-change:receipt:test").unwrap(),
            installation_id: AppInstallationId::parse("install_entity_outbox").unwrap(),
            first_change_sequence: 12,
            last_change_sequence: 12,
            payload: serde_json::json!({
                "receipt_id": "receipt:test",
                "installation_id": "install_other",
                "committed_record_revisions": [
                    {"entity": "item", "record_id": "record_1", "revision": 1}
                ],
                "change_seq_range": {"first": 12, "last": 12}
            }),
            payload_digest: AppDigest::blake3(b"identity"),
        };
        assert!(AppEntityChangeSignal::from_outbox_event(
            &mismatched_identity,
            AppRevision::new(4).unwrap(),
        )
        .is_err());

        let mismatched_range = AppEntityOutboxEvent {
            payload: serde_json::json!({
                "receipt_id": "receipt:test",
                "installation_id": "install_entity_outbox",
                "committed_record_revisions": [
                    {"entity": "item", "record_id": "record_1", "revision": 1}
                ],
                "change_seq_range": {"first": 11, "last": 11}
            }),
            payload_digest: AppDigest::blake3(b"range"),
            ..mismatched_identity
        };
        assert!(AppEntityChangeSignal::from_outbox_event(
            &mismatched_range,
            AppRevision::new(4).unwrap(),
        )
        .is_err());
    }

    #[tokio::test]
    async fn entity_outbox_claim_release_reclaim_and_ack_are_exactly_leased() {
        let (_temporary, service, authenticated) = seeded_entity_outbox().await;

        let first = service
            .claim_entity_outbox(
                &authenticated,
                AppReference::parse("worker:one").unwrap(),
                1,
                StdDuration::from_secs(10),
                time(3),
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(first.attempt_count(), 1);
        service
            .release_entity_outbox(&authenticated, first, StdDuration::from_secs(1), time(4))
            .await
            .unwrap();
        let second = service
            .claim_entity_outbox(
                &authenticated,
                AppReference::parse("worker:two").unwrap(),
                1,
                StdDuration::from_secs(10),
                time(6),
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(second.attempt_count(), 2);
        let ack = service
            .acknowledge_entity_outbox(
                &authenticated,
                second,
                AppReference::parse("delivery:entity-outbox").unwrap(),
                time(7),
            )
            .await
            .unwrap();
        assert_eq!(ack.outcome, AppEntityOutboxAcknowledgeOutcome::Delivered);
        assert!(service
            .claim_entity_outbox(
                &authenticated,
                AppReference::parse("worker:three").unwrap(),
                1,
                StdDuration::from_secs(10),
                time(8),
            )
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn expired_entity_outbox_lease_is_reclaimed_without_a_startup_scan() {
        let (_temporary, service, authenticated) = seeded_entity_outbox().await;
        let expired = service
            .claim_entity_outbox(
                &authenticated,
                AppReference::parse("worker:crashed").unwrap(),
                1,
                StdDuration::from_secs(2),
                time(3),
            )
            .await
            .unwrap()
            .pop()
            .unwrap();

        let replacement = service
            .claim_entity_outbox(
                &authenticated,
                AppReference::parse("worker:recovery").unwrap(),
                1,
                StdDuration::from_secs(10),
                time(6),
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(replacement.attempt_count(), 2);
        assert!(matches!(
            service
                .acknowledge_entity_outbox(
                    &authenticated,
                    expired,
                    AppReference::parse("delivery:stale").unwrap(),
                    time(7),
                )
                .await,
            Err(AppRegistryError::OutboxLeaseStale)
        ));
        let receipt = service
            .acknowledge_entity_outbox(
                &authenticated,
                replacement,
                AppReference::parse("delivery:recovered").unwrap(),
                time(7),
            )
            .await
            .unwrap();
        assert_eq!(
            receipt.outcome,
            AppEntityOutboxAcknowledgeOutcome::Delivered
        );
    }

    #[tokio::test]
    async fn acknowledgement_rejoins_the_exact_stored_event_bytes() {
        let (_temporary, service, authenticated) = seeded_entity_outbox().await;
        let lease = service
            .claim_entity_outbox(
                &authenticated,
                AppReference::parse("worker:exact-bytes").unwrap(),
                1,
                StdDuration::from_secs(10),
                time(3),
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        let semantically_equal = serde_json::to_vec_pretty(lease.event().payload()).unwrap();
        service
            .execute_scoped_write(&authenticated, &time(4), move |connection, _| {
                connection.execute(
                    "UPDATE app_entity_outbox SET payload_json = ?1 WHERE event_id = ?2",
                    params![semantically_equal, "app-entity-change:test"],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        assert!(matches!(
            service
                .acknowledge_entity_outbox(
                    &authenticated,
                    lease,
                    AppReference::parse("delivery:changed-bytes").unwrap(),
                    time(4),
                )
                .await,
            Err(AppRegistryError::OutboxLeaseStale)
        ));
    }

    #[tokio::test]
    async fn lease_bounds_reject_subsecond_churn_before_storage_access() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        assert!(matches!(
            service
                .claim_entity_outbox(
                    &authenticated,
                    AppReference::parse("worker:too-fast").unwrap(),
                    1,
                    StdDuration::from_millis(999),
                    time(3),
                )
                .await,
            Err(AppRegistryError::InvalidControlPlane(_))
        ));
        assert!(!temporary.path().join("scopes").exists());
    }

    #[test]
    fn entity_outbox_leases_are_not_transport_or_clone_authority() {
        static_assertions::assert_not_impl_any!(
            AppEntityOutboxLease: Clone, serde::de::DeserializeOwned
        );
    }
}
