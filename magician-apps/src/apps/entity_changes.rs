//! Bounded, identifier-only change projection for active app surfaces.
//!
//! The entity outbox remains the single durable mutation history. This module
//! reads that history through the registry owner and projects only record
//! identities, revisions and monotonic change sequences. Missing history,
//! rebuild markers and sequence discontinuities produce an explicit reset
//! instruction; they never guess at state or leak record bodies.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    entity_outbox::AppEntityOutboxEvent,
    lifecycle::AppInstallationStatus,
    models::{
        decode_app_contract, decode_bounded_json_value, AppContractError, AppContractLimits,
        AppInstallationId, AppName, AppRecordId, AppReference, AppRevision,
    },
    records::{AppChangeSequenceRange, AppCommittedRecordRevision, AppInstallation},
    registry::{AppRegistryError, AppRegistryService},
};
use magician::magician_v2::realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster};

pub const DEFAULT_APP_ENTITY_CHANGE_LIMIT: usize = 64;
pub const MAX_APP_ENTITY_CHANGE_LIMIT: usize = 128;
const MAX_APP_ENTITY_CHANGE_SCAN_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_APP_ENTITY_CHANGE_EVENT_ID_BYTES: i64 = 192;
pub const APP_ENTITY_CHANGED_EVENT_TYPE: &str = "app.entity.changed";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEntityChange {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
    pub change_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEntityChangeBatch {
    pub installation_id: AppInstallationId,
    pub surface_revision: AppRevision,
    pub after_change_sequence: u64,
    pub through_change_sequence: u64,
    pub current_change_sequence: u64,
    pub changes: Vec<AppEntityChange>,
    pub has_more: bool,
    pub reset_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEntityChangeSignal {
    pub installation_id: AppInstallationId,
    pub surface_revision: AppRevision,
    pub first_change_sequence: u64,
    pub last_change_sequence: u64,
    pub changes: Vec<AppEntityChange>,
    /// A durable rebuild invalidated every previously projected record. The
    /// receiver must discard local pages and hydrate from the canonical head.
    pub reset_required: bool,
}

impl AppEntityChangeSignal {
    pub fn from_mutation_receipt(
        receipt: &super::records::AppMutationReceipt,
        surface_revision: AppRevision,
    ) -> Result<Self, AppEntityChangeError> {
        let expected_len = receipt
            .change_seq_range
            .last
            .checked_sub(receipt.change_seq_range.first)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(AppEntityChangeError::CorruptHistory(
                "mutation receipt sequence span exceeds supported size",
            ))?;
        if expected_len != receipt.committed_record_revisions.len() {
            return Err(AppEntityChangeError::CorruptHistory(
                "mutation receipt records differ from its sequence range",
            ));
        }
        let changes = receipt
            .committed_record_revisions
            .iter()
            .enumerate()
            .map(|(offset, record)| {
                let offset = u64::try_from(offset).map_err(|_| {
                    AppEntityChangeError::CorruptHistory(
                        "mutation receipt offset exceeds supported size",
                    )
                })?;
                Ok::<AppEntityChange, AppEntityChangeError>(AppEntityChange {
                    entity: record.entity.clone(),
                    record_id: record.record_id.clone(),
                    record_revision: record.revision,
                    change_sequence: receipt.change_seq_range.first.checked_add(offset).ok_or(
                        AppEntityChangeError::CorruptHistory("mutation receipt sequence exhausted"),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            installation_id: receipt.installation_id.clone(),
            surface_revision,
            first_change_sequence: receipt.change_seq_range.first,
            last_change_sequence: receipt.change_seq_range.last,
            changes,
            reset_required: false,
        })
    }

    pub fn from_outbox_event(
        event: &AppEntityOutboxEvent,
        surface_revision: AppRevision,
    ) -> Result<Self, AppEntityChangeError> {
        let stored = StoredChangeEvent {
            event_id: event.event_id().to_string(),
            first: event.first_change_sequence(),
            last: event.last_change_sequence(),
            payload: serde_json::to_vec(event.payload())?,
        };
        let Some(changes) = project_exact_event(&stored, event.installation_id())? else {
            return Ok(Self {
                installation_id: event.installation_id().clone(),
                surface_revision,
                first_change_sequence: event.first_change_sequence(),
                last_change_sequence: event.last_change_sequence(),
                changes: Vec::new(),
                reset_required: true,
            });
        };
        Ok(Self {
            installation_id: event.installation_id().clone(),
            surface_revision,
            first_change_sequence: event.first_change_sequence(),
            last_change_sequence: event.last_change_sequence(),
            changes,
            reset_required: false,
        })
    }
}

pub fn emit_app_entity_change(
    broadcaster: &Arc<RuntimeTransportBroadcaster>,
    authenticated_scope: &AuthenticatedAppScope,
    signal: AppEntityChangeSignal,
) {
    let agent_id = format!("app:{}", signal.installation_id.as_str());
    let payload = match serde_json::to_value(signal) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(error = %error, "failed to encode app entity change signal");
            return;
        },
    };
    broadcaster.emit_agent_transport_event(AgentEventEnvelope::new_scoped(
        APP_ENTITY_CHANGED_EVENT_TYPE,
        &agent_id,
        authenticated_scope.scope().principal.as_str(),
        authenticated_scope.scope().workspace.as_str(),
        payload,
    ));
}

#[derive(Debug, Clone)]
pub struct AppEntityChangeService {
    registry: AppRegistryService,
}

impl AppEntityChangeService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    pub async fn read_after(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        expected_surface_revision: AppRevision,
        after_change_sequence: u64,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<AppEntityChangeBatch, AppEntityChangeError> {
        if limit == 0 || limit > MAX_APP_ENTITY_CHANGE_LIMIT {
            return Err(AppEntityChangeError::InvalidLimit);
        }
        let installation_id = installation_id.clone();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(read_changes_blocking(
                    connection,
                    scope,
                    &installation_id,
                    expected_surface_revision,
                    after_change_sequence,
                    limit,
                ))
            })
            .await?
            .ok_or(AppEntityChangeError::NotFound)?
    }

    pub async fn current_surface_revision(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppRevision, AppEntityChangeError> {
        let installation_id = installation_id.clone();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(
                    load_enabled_installation(connection, scope, &installation_id).and_then(
                        |installation| {
                            installation.active_surface_revision.ok_or(
                                AppEntityChangeError::CorruptInstallation(
                                    "enabled installation has no active surface revision",
                                ),
                            )
                        },
                    ),
                )
            })
            .await?
            .ok_or(AppEntityChangeError::NotFound)?
    }

    /// Validate one exact durable outbox event before deciding whether an
    /// enabled surface currently exists to receive it. Disabled/purged apps
    /// consume valid history without broadcasting; re-enabling hydrates from
    /// the canonical store rather than replaying stale advisory signals.
    pub async fn signal_for_outbox_event(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        event: &AppEntityOutboxEvent,
        now: DateTime<Utc>,
    ) -> Result<Option<AppEntityChangeSignal>, AppEntityChangeError> {
        let placeholder_revision = AppRevision::new(1)?;
        let mut signal = AppEntityChangeSignal::from_outbox_event(event, placeholder_revision)?;
        match self
            .current_surface_revision(authenticated_scope, event.installation_id(), now)
            .await
        {
            Ok(surface_revision) => {
                signal.surface_revision = surface_revision;
                Ok(Some(signal))
            },
            Err(AppEntityChangeError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Error)]
pub enum AppEntityChangeError {
    #[error("app installation does not exist in this authenticated scope")]
    NotFound,
    #[error("app surface changed before its deltas could be read")]
    SurfaceRevisionChanged,
    #[error(
        "app change cursor {after_change_sequence} is ahead of durable head \
         {current_change_sequence}; reload the surface"
    )]
    CursorAheadOfHead {
        after_change_sequence: u64,
        current_change_sequence: u64,
    },
    #[error("app entity change limit must be between 1 and {MAX_APP_ENTITY_CHANGE_LIMIT}")]
    InvalidLimit,
    #[error("app installation is corrupt: {0}")]
    CorruptInstallation(&'static str),
    #[error("app entity change history is corrupt: {0}")]
    CorruptHistory(&'static str),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationChangeProjection {
    receipt_id: AppReference,
    installation_id: AppInstallationId,
    committed_record_revisions: Vec<AppCommittedRecordRevision>,
    change_seq_range: AppChangeSequenceRange,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportChangeProjection {
    receipt_ref: AppReference,
    installation_id: AppInstallationId,
    source_archive_digest: super::models::AppDigest,
    records: Vec<AppCommittedRecordRevision>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RebuildChangeProjection {
    installation_id: AppInstallationId,
    receipt_ref: AppReference,
    dataset_generation: u64,
    rebuild_all: bool,
}

struct StoredChangeEvent {
    event_id: String,
    first: u64,
    last: u64,
    payload: Vec<u8>,
}

fn read_changes_blocking(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
    expected_surface_revision: AppRevision,
    after_change_sequence: u64,
    limit: usize,
) -> Result<AppEntityChangeBatch, AppEntityChangeError> {
    let transaction = connection.unchecked_transaction()?;
    let batch = read_changes_snapshot(
        &transaction,
        scope,
        installation_id,
        expected_surface_revision,
        after_change_sequence,
        limit,
    )?;
    transaction.commit()?;
    Ok(batch)
}

fn read_changes_snapshot(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
    expected_surface_revision: AppRevision,
    after_change_sequence: u64,
    limit: usize,
) -> Result<AppEntityChangeBatch, AppEntityChangeError> {
    let installation = load_enabled_installation(connection, scope, installation_id)?;
    let surface_revision =
        installation
            .active_surface_revision
            .ok_or(AppEntityChangeError::CorruptInstallation(
                "enabled installation has no active surface revision",
            ))?;
    if surface_revision != expected_surface_revision {
        return Err(AppEntityChangeError::SurfaceRevisionChanged);
    }
    let current_change_sequence = current_change_sequence(connection, installation_id)?;
    if after_change_sequence > current_change_sequence {
        return Err(AppEntityChangeError::CursorAheadOfHead {
            after_change_sequence,
            current_change_sequence,
        });
    }

    let sql_limit = i64::try_from(limit)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(AppEntityChangeError::InvalidLimit)?;
    let after_sql = i64::try_from(after_change_sequence)
        .map_err(|_| AppEntityChangeError::CorruptHistory("change sequence exceeds SQLite"))?;
    let (events, scan_exhausted) = {
        let mut statement = connection.prepare(
            "SELECT event_id, first_change_seq, last_change_seq, payload_json
               FROM app_entity_outbox
              WHERE installation_id = ?1 AND last_change_seq > ?2
                AND length(event_id) BETWEEN 1 AND ?3
                AND length(payload_json) BETWEEN 1 AND ?4
              ORDER BY first_change_seq ASC, sequence ASC
              LIMIT ?5",
        )?;
        let max_payload_bytes = i64::try_from(MAX_APP_ENTITY_CHANGE_SCAN_BYTES)
            .map_err(|_| AppEntityChangeError::CorruptHistory("scan byte limit overflowed"))?;
        let mut rows = statement.query(params![
            installation_id.as_str(),
            after_sql,
            MAX_APP_ENTITY_CHANGE_EVENT_ID_BYTES,
            max_payload_bytes,
            sql_limit
        ])?;
        let mut events = Vec::with_capacity(limit.saturating_add(1));
        let mut scanned_bytes = 0usize;
        let mut scan_exhausted = false;
        while let Some(row) = rows.next()? {
            let event_id = row.get::<_, String>(0)?;
            let first = positive_sequence(row.get::<_, i64>(1)?)?;
            let last = positive_sequence(row.get::<_, i64>(2)?)?;
            let payload = row.get::<_, Vec<u8>>(3)?;
            let Some(next_scanned_bytes) = admit_scan_bytes(scanned_bytes, payload.len()) else {
                scan_exhausted = true;
                break;
            };
            scanned_bytes = next_scanned_bytes;
            if last < first {
                return Err(AppEntityChangeError::CorruptHistory(
                    "outbox sequence range is reversed",
                ));
            }
            events.push(StoredChangeEvent {
                event_id,
                first,
                last,
                payload,
            });
        }
        (events, scan_exhausted)
    };
    if scan_exhausted {
        return Ok(reset_batch(
            installation_id,
            surface_revision,
            after_change_sequence,
            current_change_sequence,
        ));
    }

    let expected_first =
        after_change_sequence
            .checked_add(1)
            .ok_or(AppEntityChangeError::CorruptHistory(
                "change sequence exhausted",
            ))?;
    if events
        .first()
        .is_none_or(|event| event.first > expected_first)
    {
        return if current_change_sequence > after_change_sequence {
            Ok(reset_batch(
                installation_id,
                surface_revision,
                after_change_sequence,
                current_change_sequence,
            ))
        } else {
            Ok(AppEntityChangeBatch {
                installation_id: installation_id.clone(),
                surface_revision,
                after_change_sequence,
                through_change_sequence: after_change_sequence,
                current_change_sequence,
                changes: Vec::new(),
                has_more: false,
                reset_required: false,
            })
        };
    }

    let mut expected = expected_first;
    let mut changes = Vec::with_capacity(limit.min(MAX_APP_ENTITY_CHANGE_LIMIT));
    let mut reset_required = false;
    'events: for event in events {
        if event.last < expected {
            continue;
        }
        if event.first > expected {
            reset_required = true;
            break;
        }
        let Some(projected) = project_exact_event(&event, installation_id)? else {
            reset_required = true;
            break;
        };
        for change in projected {
            if change.change_sequence < expected {
                continue;
            }
            if change.change_sequence != expected {
                reset_required = true;
                break 'events;
            }
            changes.push(change);
            expected = expected
                .checked_add(1)
                .ok_or(AppEntityChangeError::CorruptHistory(
                    "change sequence exhausted",
                ))?;
            if changes.len() > limit {
                break 'events;
            }
        }
    }
    if reset_required {
        return Ok(reset_batch(
            installation_id,
            surface_revision,
            after_change_sequence,
            current_change_sequence,
        ));
    }
    let has_more = changes.len() > limit;
    if has_more {
        changes.truncate(limit);
    }
    let through_change_sequence = changes
        .last()
        .map_or(after_change_sequence, |change| change.change_sequence);
    if !has_more && through_change_sequence < current_change_sequence {
        // The bounded query could not account for the current durable head.
        // Force a canonical page instead of advancing across unseen history.
        return Ok(reset_batch(
            installation_id,
            surface_revision,
            after_change_sequence,
            current_change_sequence,
        ));
    }
    Ok(AppEntityChangeBatch {
        installation_id: installation_id.clone(),
        surface_revision,
        after_change_sequence,
        through_change_sequence,
        current_change_sequence,
        changes,
        has_more,
        reset_required: false,
    })
}

fn project_exact_event(
    event: &StoredChangeEvent,
    installation_id: &AppInstallationId,
) -> Result<Option<Vec<AppEntityChange>>, AppEntityChangeError> {
    let value = decode_bounded_json_value(&event.payload, &AppContractLimits::default())?;
    let records = if event.event_id.starts_with("app-entity-change:") {
        let projection: MutationChangeProjection = serde_json::from_value(value)?;
        if projection.installation_id != *installation_id
            || projection.change_seq_range.first != event.first
            || projection.change_seq_range.last != event.last
        {
            return Err(AppEntityChangeError::CorruptHistory(
                "mutation projection identity differs from its outbox row",
            ));
        }
        let _ = projection.receipt_id;
        projection.committed_record_revisions
    } else if event.event_id.starts_with("app-import-change:") {
        let projection: ImportChangeProjection = serde_json::from_value(value)?;
        if projection.installation_id != *installation_id {
            return Err(AppEntityChangeError::CorruptHistory(
                "import projection identity differs from its outbox row",
            ));
        }
        let _ = (projection.receipt_ref, projection.source_archive_digest);
        projection.records
    } else if event.event_id.starts_with("app-forget-rebuild:") {
        let projection: RebuildChangeProjection = serde_json::from_value(value)?;
        if projection.installation_id != *installation_id
            || !projection.rebuild_all
            || projection.dataset_generation == 0
        {
            return Err(AppEntityChangeError::CorruptHistory(
                "rebuild projection identity is invalid",
            ));
        }
        let _ = projection.receipt_ref;
        return Ok(None);
    } else {
        // A future producer must add an explicit, strictly decoded projection
        // before this consumer may acknowledge its row. Treating an unknown
        // kind as a rebuild would discard its semantics after one advisory
        // reload and permanently acknowledge evidence we did not understand.
        return Err(AppEntityChangeError::CorruptHistory(
            "unsupported entity outbox event kind",
        ));
    };
    let span = event
        .last
        .checked_sub(event.first)
        .and_then(|value| value.checked_add(1))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(AppEntityChangeError::CorruptHistory(
            "outbox sequence span exceeds supported size",
        ))?;
    if records.is_empty()
        || records.len() != span
        || records.len() > AppContractLimits::default().max_collection_items()
    {
        return Err(AppEntityChangeError::CorruptHistory(
            "outbox record count differs from its sequence range",
        ));
    }
    records
        .into_iter()
        .enumerate()
        .map(|(offset, record)| {
            let offset = u64::try_from(offset).map_err(|_| {
                AppEntityChangeError::CorruptHistory("record offset exceeds supported size")
            })?;
            let change_sequence =
                event
                    .first
                    .checked_add(offset)
                    .ok_or(AppEntityChangeError::CorruptHistory(
                        "change sequence exhausted",
                    ))?;
            Ok(AppEntityChange {
                entity: record.entity,
                record_id: record.record_id,
                record_revision: record.revision,
                change_sequence,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn load_enabled_installation(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
) -> Result<AppInstallation, AppEntityChangeError> {
    let max_bytes = i64::try_from(AppContractLimits::default().max_document_bytes())
        .map_err(|_| AppEntityChangeError::CorruptInstallation("document limit overflow"))?;
    let bytes = connection
        .query_row(
            "SELECT record_json FROM app_installations
              WHERE installation_id = ?1 AND length(record_json) BETWEEN 1 AND ?2",
            params![installation_id.as_str(), max_bytes],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppEntityChangeError::NotFound)?;
    let installation: AppInstallation = decode_app_contract(&bytes, &AppContractLimits::default())?;
    if installation.scope != *scope
        || installation.installation_id != *installation_id
        || installation.lifecycle.status != AppInstallationStatus::Enabled
    {
        return Err(AppEntityChangeError::NotFound);
    }
    Ok(installation)
}

fn current_change_sequence(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
) -> Result<u64, AppEntityChangeError> {
    let value: i64 = connection.query_row(
        "SELECT COALESCE(
             (SELECT next_change_seq - 1 FROM app_installation_sequences
               WHERE installation_id = ?1),
             (SELECT MAX(change_seq) FROM app_record_heads
               WHERE installation_id = ?1),
             0
         )",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    u64::try_from(value)
        .map_err(|_| AppEntityChangeError::CorruptHistory("current change sequence is negative"))
}

fn positive_sequence(value: i64) -> Result<u64, AppEntityChangeError> {
    u64::try_from(value).ok().filter(|value| *value > 0).ok_or(
        AppEntityChangeError::CorruptHistory("outbox change sequence is not positive"),
    )
}

fn admit_scan_bytes(current: usize, next: usize) -> Option<usize> {
    current
        .checked_add(next)
        .filter(|total| *total <= MAX_APP_ENTITY_CHANGE_SCAN_BYTES)
}

fn reset_batch(
    installation_id: &AppInstallationId,
    surface_revision: AppRevision,
    after_change_sequence: u64,
    current_change_sequence: u64,
) -> AppEntityChangeBatch {
    AppEntityChangeBatch {
        installation_id: installation_id.clone(),
        surface_revision,
        after_change_sequence,
        through_change_sequence: current_change_sequence,
        current_change_sequence,
        changes: Vec::new(),
        has_more: false,
        reset_required: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::{
        apps::registry::tests::{authenticated_scope, canonical_tempdir, publication, time},
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    async fn seeded_service() -> (
        tempfile::TempDir,
        AppRegistryService,
        AuthenticatedAppScope,
        AppInstallationId,
    ) {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = AppRegistryService::new(workspace.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        let installation_id = AppInstallationId::parse("install_entity_changes").unwrap();
        registry
            .publish_ready_for_review(
                &authenticated,
                publication(
                    &workspace,
                    "anonymous",
                    "default",
                    "attempt:entity-changes",
                    installation_id.as_str(),
                ),
                time(1),
            )
            .await
            .unwrap();
        let mut installation = registry
            .installation(&authenticated, &installation_id, time(2))
            .await
            .unwrap()
            .unwrap();
        installation.lifecycle.status = AppInstallationStatus::Enabled;
        installation.grant_revision = Some(AppRevision::new(1).unwrap());
        installation.active_surface_revision = Some(AppRevision::new(3).unwrap());
        installation.active_schema_revision = Some(AppRevision::new(2).unwrap());
        let record = serde_json::to_vec(&installation).unwrap();
        let installation_for_write = installation_id.clone();
        registry
            .execute_scoped_write(&authenticated, &time(2), move |connection, _| {
                connection.execute(
                    "UPDATE app_installations
                        SET lifecycle_status = 'enabled', record_json = ?2
                      WHERE installation_id = ?1",
                    params![installation_for_write.as_str(), record],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        (temporary, registry, authenticated, installation_id)
    }

    #[tokio::test]
    async fn bounded_reader_projects_only_exact_identities_and_sequences() {
        let (_temporary, registry, authenticated, installation_id) = seeded_service().await;
        let installation_for_write = installation_id.clone();
        registry
            .execute_scoped_write(&authenticated, &time(3), move |connection, _| {
                let payload = serde_json::to_vec(&serde_json::json!({
                    "receipt_id": "receipt:delta",
                    "installation_id": installation_for_write,
                    "committed_record_revisions": [
                        {"entity": "item", "record_id": "record_1", "revision": 4},
                        {"entity": "item", "record_id": "record_2", "revision": 1}
                    ],
                    "change_seq_range": {"first": 1, "last": 2}
                }))?;
                connection.execute(
                    "INSERT INTO app_installation_sequences (installation_id, next_change_seq)
                     VALUES (?1, 3)",
                    params![installation_for_write.as_str()],
                )?;
                connection.execute(
                    "INSERT INTO app_entity_outbox (
                         event_id, installation_id, first_change_seq, last_change_seq,
                         payload_json, delivery_state, available_at, lease_token,
                         lease_expires_at, created_at, delivered_at
                     ) VALUES ('app-entity-change:delta', ?1, 1, 2, ?2,
                               'pending', ?3, NULL, NULL, ?3, NULL)",
                    params![
                        installation_for_write.as_str(),
                        payload,
                        time(3).to_rfc3339()
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let batch = AppEntityChangeService::new(registry)
            .read_after(
                &authenticated,
                &installation_id,
                AppRevision::new(3).unwrap(),
                0,
                16,
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(batch.changes.len(), 2);
        assert_eq!(batch.changes[0].change_sequence, 1);
        assert_eq!(batch.changes[1].record_id.as_str(), "record_2");
        assert!(!batch.reset_required);
        let encoded = serde_json::to_value(&batch).unwrap();
        let encoded = encoded.to_string();
        assert!(!encoded.contains("payload"));
        assert!(!encoded.contains("fields"));
        assert!(!encoded.contains("handling_policy"));
    }

    #[tokio::test]
    async fn missing_history_and_rebuild_markers_require_a_fresh_bounded_page() {
        let (_temporary, registry, authenticated, installation_id) = seeded_service().await;
        let installation_for_write = installation_id.clone();
        registry
            .execute_scoped_write(&authenticated, &time(3), move |connection, _| {
                connection.execute(
                    "INSERT INTO app_installation_sequences (installation_id, next_change_seq)
                     VALUES (?1, 8)",
                    params![installation_for_write.as_str()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let batch = AppEntityChangeService::new(registry)
            .read_after(
                &authenticated,
                &installation_id,
                AppRevision::new(3).unwrap(),
                2,
                16,
                time(4),
            )
            .await
            .unwrap();
        assert!(batch.reset_required);
        assert!(batch.changes.is_empty());
        assert_eq!(batch.current_change_sequence, 7);
    }

    #[tokio::test]
    async fn oversized_persisted_event_identity_is_not_loaded_into_the_change_projection() {
        let (_temporary, registry, authenticated, installation_id) = seeded_service().await;
        let installation_for_write = installation_id.clone();
        registry
            .execute_scoped_write(&authenticated, &time(3), move |connection, _| {
                connection.execute(
                    "INSERT INTO app_installation_sequences (installation_id, next_change_seq)
                     VALUES (?1, 2)",
                    params![installation_for_write.as_str()],
                )?;
                connection.execute(
                    "INSERT INTO app_entity_outbox (
                         event_id, installation_id, first_change_seq, last_change_seq,
                         payload_json, delivery_state, available_at, lease_token,
                         lease_expires_at, created_at, delivered_at
                     ) VALUES (?1, ?2, 1, 1, X'7B7D',
                               'pending', ?3, NULL, NULL, ?3, NULL)",
                    params![
                        "x".repeat(MAX_APP_ENTITY_CHANGE_EVENT_ID_BYTES as usize + 1),
                        installation_for_write.as_str(),
                        time(3).to_rfc3339()
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let batch = AppEntityChangeService::new(registry)
            .read_after(
                &authenticated,
                &installation_id,
                AppRevision::new(3).unwrap(),
                0,
                16,
                time(4),
            )
            .await
            .unwrap();
        assert!(batch.reset_required);
        assert!(batch.changes.is_empty());
    }

    #[tokio::test]
    async fn cursor_ahead_of_durable_head_is_an_explicit_reloadable_conflict() {
        let (_temporary, registry, authenticated, installation_id) = seeded_service().await;
        let error = AppEntityChangeService::new(registry)
            .read_after(
                &authenticated,
                &installation_id,
                AppRevision::new(3).unwrap(),
                1,
                16,
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppEntityChangeError::CursorAheadOfHead {
                after_change_sequence: 1,
                current_change_sequence: 0
            }
        ));
    }

    #[test]
    fn mutation_signal_is_contiguous_and_contains_no_record_body() {
        let receipt = super::super::records::AppMutationReceipt {
            receipt_id: AppReference::parse("receipt:signal").unwrap(),
            installation_id: AppInstallationId::parse("install_signal").unwrap(),
            origin: super::super::records::AppMutationOrigin::OwnerApi {
                session_ref: AppReference::parse("session:test").unwrap(),
                request_ref: AppReference::parse("request:test").unwrap(),
            },
            mutation_key: super::super::models::AppDigest::blake3(b"mutation"),
            batch_digest: super::super::models::AppDigest::blake3(b"batch"),
            committed_record_revisions: vec![AppCommittedRecordRevision {
                entity: AppName::parse("item").unwrap(),
                record_id: AppRecordId::parse("record_1").unwrap(),
                revision: AppRevision::new(9).unwrap(),
            }],
            change_seq_range: AppChangeSequenceRange {
                first: 11,
                last: 11,
            },
            committed_at: time(5),
        };
        let signal =
            AppEntityChangeSignal::from_mutation_receipt(&receipt, AppRevision::new(3).unwrap())
                .unwrap();
        assert_eq!(signal.changes[0].change_sequence, 11);
        assert!(!signal.reset_required);
        let encoded = serde_json::to_string(&signal).unwrap();
        assert!(!encoded.contains("payload"));
        assert!(!encoded.contains("origin"));
    }

    #[test]
    fn aggregate_change_scan_is_hard_bounded_without_wrapping() {
        assert_eq!(
            admit_scan_bytes(0, MAX_APP_ENTITY_CHANGE_SCAN_BYTES),
            Some(MAX_APP_ENTITY_CHANGE_SCAN_BYTES)
        );
        assert_eq!(admit_scan_bytes(MAX_APP_ENTITY_CHANGE_SCAN_BYTES, 1), None);
        assert_eq!(admit_scan_bytes(usize::MAX, 1), None);
    }
}
