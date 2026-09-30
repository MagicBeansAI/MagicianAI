//! Transactional reviewed contribution-frequency owner.
//!
//! Memory and retrieval proposals consume the same per-installation/workflow/
//! action/port bucket in the registry transaction that publishes their exact
//! terminal intent. The reviewed limit is a non-deserializable value, event
//! identity is deterministic, and an exact replay never spends twice.

use chrono::{DateTime, SecondsFormat, Utc};
use magician_app_contract::contribution::content_digest;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::{
    contribution::AppContributionError,
    records::{
        AppScope, APP_CONTRIBUTION_MAX_FREQUENCY_WINDOW_SECONDS,
        APP_CONTRIBUTION_MAX_PROPOSALS_PER_WINDOW,
    },
    registry::AppRegistryError,
};

const MAX_BUCKET_ROWS: i64 = 8_192;
const MAX_BUCKET_LEDGER_BYTES: usize = 4 * 1024 * 1024;
const PRUNE_BATCH: usize = 64;

/// Exact reviewed frequency input. Serialized app/model bytes cannot mint it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppReviewedContributionFrequencyV1 {
    max_proposals: u32,
    window_seconds: u64,
}

impl AppReviewedContributionFrequencyV1 {
    pub(crate) fn from_reviewed_limit(
        max_proposals: u32,
        window_seconds: u64,
    ) -> Result<Self, AppContributionError> {
        if max_proposals == 0
            || max_proposals > u32::from(APP_CONTRIBUTION_MAX_PROPOSALS_PER_WINDOW)
            || window_seconds == 0
            || window_seconds > APP_CONTRIBUTION_MAX_FREQUENCY_WINDOW_SECONDS
        {
            return Err(invalid_control(
                "reviewed contribution frequency is outside its bounded domain",
            ));
        }
        Ok(Self {
            max_proposals,
            window_seconds,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppContributionFrequencyConsumeOutcome {
    Consumed { event_id: String },
    ExactReplay { event_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FrequencyEventV1 {
    event_id: String,
    proposal_id: String,
    proposal_revision: u64,
    proposal_digest: String,
    issued_at_ms: i64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_contribution_frequency_in_transaction(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    scope_binding_ref: &str,
    installation_id: &str,
    workflow_id: &str,
    action_id: &str,
    contribution_port_id: &str,
    reviewed: AppReviewedContributionFrequencyV1,
    proposal_id: &str,
    proposal_revision: u64,
    proposal_digest: &str,
    issued_at_ms: i64,
    now: &DateTime<Utc>,
) -> Result<AppContributionFrequencyConsumeOutcome, AppContributionError> {
    validate_identity("scope binding", scope_binding_ref)?;
    validate_identity("installation", installation_id)?;
    validate_identity("workflow", workflow_id)?;
    validate_identity("action", action_id)?;
    validate_identity("contribution port", contribution_port_id)?;
    validate_identity("proposal", proposal_id)?;
    validate_digest(proposal_digest)?;
    if scope.principal.as_str().is_empty()
        || scope.workspace.as_str().is_empty()
        || proposal_revision == 0
        || issued_at_ms < 0
        || issued_at_ms > now.timestamp_millis()
    {
        return Err(invalid_control(
            "contribution frequency identity or proposal time is invalid",
        ));
    }
    prune_contribution_frequency_buckets_in_transaction(transaction, now, PRUNE_BATCH)?;
    validate_frequency_accounting(transaction)?;

    let event_id = frequency_event_id(
        installation_id,
        workflow_id,
        action_id,
        contribution_port_id,
        scope_binding_ref,
        proposal_id,
        proposal_revision,
        proposal_digest,
    );
    let mut statement = transaction.prepare(
        "SELECT event_ledger_json FROM app_contribution_frequency_buckets
          WHERE installation_id=?1 AND workflow_id=?2 AND action_id=?3
            AND contribution_port_id=?4 AND scope_binding_ref=?5
          ORDER BY window_start_epoch DESC",
    )?;
    let ledgers = statement
        .query_map(
            params![
                installation_id,
                workflow_id,
                action_id,
                contribution_port_id,
                scope_binding_ref,
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for ledger_json in ledgers {
        let ledger = decode_ledger(&ledger_json)?;
        for retained in ledger {
            if retained.event_id == event_id
                || (retained.proposal_id == proposal_id
                    && retained.proposal_revision == proposal_revision)
            {
                if retained
                    == (FrequencyEventV1 {
                        event_id: event_id.clone(),
                        proposal_id: proposal_id.to_owned(),
                        proposal_revision,
                        proposal_digest: proposal_digest.to_owned(),
                        issued_at_ms,
                    })
                {
                    return Ok(AppContributionFrequencyConsumeOutcome::ExactReplay { event_id });
                }
                return Err(AppContributionError::SubstitutedReplay);
            }
        }
    }
    let compacted_issued_through_ms = transaction
        .query_row(
            "SELECT compacted_issued_through_ms
               FROM app_contribution_frequency_compaction_heads
              WHERE installation_id=?1 AND workflow_id=?2 AND action_id=?3
                AND contribution_port_id=?4 AND scope_binding_ref=?5",
            params![
                installation_id,
                workflow_id,
                action_id,
                contribution_port_id,
                scope_binding_ref,
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if compacted_issued_through_ms.is_some_and(|high_water| issued_at_ms <= high_water) {
        return Err(AppContributionError::HistoryCompacted);
    }

    if now.timestamp() < 0 {
        return Err(invalid_control("frequency clock predates the Unix epoch"));
    }
    let window_seconds = i64::try_from(reviewed.window_seconds)
        .map_err(|_| invalid_control("reviewed frequency window overflow"))?;
    // The sealed source event owns the bucket. Processing-time bucketing would
    // let a response-lost retry cross a wall-clock boundary and spend again.
    let issued_epoch = issued_at_ms.div_euclid(1_000);
    let window_start = issued_epoch - issued_epoch.rem_euclid(window_seconds);
    let stored = transaction
        .query_row(
            "SELECT reviewed_max_proposals, consumed_count, event_ledger_json
               FROM app_contribution_frequency_buckets
              WHERE installation_id=?1 AND workflow_id=?2 AND action_id=?3
                AND contribution_port_id=?4 AND scope_binding_ref=?5
                AND window_seconds=?6 AND window_start_epoch=?7",
            params![
                installation_id,
                workflow_id,
                action_id,
                contribution_port_id,
                scope_binding_ref,
                window_seconds,
                window_start,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    let mut ledger = if let Some((stored_max, consumed, ledger_json)) = stored {
        if stored_max != i64::from(reviewed.max_proposals)
            || consumed != i64::try_from(decode_ledger(&ledger_json)?.len()).unwrap_or(i64::MAX)
        {
            return Err(invalid_control(
                "reviewed contribution frequency changed inside an active window",
            ));
        }
        if consumed >= stored_max {
            return Err(AppContributionError::Quota(
                "reviewed contribution proposals per window",
            ));
        }
        decode_ledger(&ledger_json)?
    } else {
        Vec::new()
    };
    ledger.push(FrequencyEventV1 {
        event_id: event_id.clone(),
        proposal_id: proposal_id.to_owned(),
        proposal_revision,
        proposal_digest: proposal_digest.to_owned(),
        issued_at_ms,
    });
    let ledger_json = serde_json::to_vec(&ledger)?;
    if ledger_json.len() > MAX_BUCKET_LEDGER_BYTES {
        return Err(AppContributionError::Quota(
            "contribution frequency replay ledger bytes",
        ));
    }
    transaction.execute(
        "INSERT INTO app_contribution_frequency_buckets(
             installation_id, workflow_id, action_id, contribution_port_id,
             scope_binding_ref, window_seconds, window_start_epoch,
             reviewed_max_proposals, consumed_count, event_ledger_json, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(
             installation_id, workflow_id, action_id, contribution_port_id,
             scope_binding_ref, window_seconds, window_start_epoch
         ) DO UPDATE SET consumed_count=excluded.consumed_count,
             event_ledger_json=excluded.event_ledger_json,
             updated_at=excluded.updated_at",
        params![
            installation_id,
            workflow_id,
            action_id,
            contribution_port_id,
            scope_binding_ref,
            window_seconds,
            window_start,
            i64::from(reviewed.max_proposals),
            i64::try_from(ledger.len()).map_err(|_| invalid_control("frequency count overflow"))?,
            ledger_json,
            timestamp(now),
        ],
    )?;
    validate_frequency_accounting(transaction)?;
    Ok(AppContributionFrequencyConsumeOutcome::Consumed { event_id })
}

/// Bounded pruning used by either destination's terminal publisher. Each
/// removed bucket advances a per-port issued-time high-water before deletion,
/// so an old proposal cannot become fresh again after replay bytes are gone.
pub(crate) fn prune_contribution_frequency_buckets_in_transaction(
    transaction: &Transaction<'_>,
    now: &DateTime<Utc>,
    limit: usize,
) -> Result<usize, AppContributionError> {
    if limit == 0 || limit > PRUNE_BATCH {
        return Err(invalid_control("frequency prune batch is invalid"));
    }
    let now_epoch = now.timestamp();
    let mut statement = transaction.prepare(
        "SELECT installation_id, workflow_id, action_id, contribution_port_id,
                scope_binding_ref, window_seconds, window_start_epoch,
                event_ledger_json
           FROM app_contribution_frequency_buckets
          WHERE window_start_epoch + (window_seconds * 2) <= ?1
          ORDER BY window_start_epoch, installation_id, workflow_id, action_id,
                   contribution_port_id
          LIMIT ?2",
    )?;
    let rows = statement
        .query_map(
            params![now_epoch, i64::try_from(limit).unwrap_or(i64::MAX)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for row in &rows {
        let high_water = decode_ledger(&row.7)?
            .into_iter()
            .map(|event| event.issued_at_ms)
            .max()
            .ok_or_else(|| invalid_control("frequency bucket has an empty ledger"))?;
        transaction.execute(
            "INSERT INTO app_contribution_frequency_compaction_heads(
                 installation_id, workflow_id, action_id, contribution_port_id,
                 scope_binding_ref, compacted_issued_through_ms, compacted_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(
                 installation_id, workflow_id, action_id, contribution_port_id,
                 scope_binding_ref
             ) DO UPDATE SET compacted_issued_through_ms=MAX(
                    compacted_issued_through_ms,
                    excluded.compacted_issued_through_ms
                ), compacted_at=excluded.compacted_at",
            params![
                row.0,
                row.1,
                row.2,
                row.3,
                row.4,
                high_water,
                timestamp(now)
            ],
        )?;
        transaction.execute(
            "DELETE FROM app_contribution_frequency_buckets
              WHERE installation_id=?1 AND workflow_id=?2 AND action_id=?3
                AND contribution_port_id=?4 AND scope_binding_ref=?5
                AND window_seconds=?6 AND window_start_epoch=?7",
            params![row.0, row.1, row.2, row.3, row.4, row.5, row.6],
        )?;
    }
    Ok(rows.len())
}

#[allow(clippy::too_many_arguments)]
fn frequency_event_id(
    installation_id: &str,
    workflow_id: &str,
    action_id: &str,
    contribution_port_id: &str,
    scope_binding_ref: &str,
    proposal_id: &str,
    proposal_revision: u64,
    proposal_digest: &str,
) -> String {
    let digest = content_digest(
        format!(
            "magician.app-contribution-frequency-event.v1\0{installation_id}\0{workflow_id}\\
             0{action_id}\0{contribution_port_id}\0{scope_binding_ref}\0{proposal_id}\\
             0{proposal_revision}\0{proposal_digest}"
        )
        .as_bytes(),
    );
    format!(
        "contribution-frequency:{}",
        digest.trim_start_matches("blake3:")
    )
}

fn decode_ledger(bytes: &[u8]) -> Result<Vec<FrequencyEventV1>, AppContributionError> {
    if bytes.is_empty() || bytes.len() > MAX_BUCKET_LEDGER_BYTES {
        return Err(invalid_control("frequency event ledger size is invalid"));
    }
    let ledger: Vec<FrequencyEventV1> = serde_json::from_slice(bytes)?;
    if ledger.is_empty() || ledger.len() > usize::from(APP_CONTRIBUTION_MAX_PROPOSALS_PER_WINDOW) {
        return Err(invalid_control("frequency event ledger count is invalid"));
    }
    Ok(ledger)
}

fn validate_frequency_accounting(
    transaction: &Transaction<'_>,
) -> Result<(), AppContributionError> {
    let (rows, bytes): (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(event_ledger_json)), 0)
           FROM app_contribution_frequency_buckets",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let compaction_heads: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_contribution_frequency_compaction_heads",
        [],
        |row| row.get(0),
    )?;
    if rows < 0
        || rows > MAX_BUCKET_ROWS
        || compaction_heads < 0
        || compaction_heads > MAX_BUCKET_ROWS
        || bytes < 0
        || bytes > MAX_BUCKET_ROWS.saturating_mul(MAX_BUCKET_LEDGER_BYTES as i64)
    {
        return Err(AppContributionError::Quota(
            "bounded contribution frequency accounting",
        ));
    }
    Ok(())
}

fn validate_identity(label: &str, value: &str) -> Result<(), AppContributionError> {
    if value.is_empty()
        || value.len() > 192
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(invalid_control(&format!("{label} identity is invalid")));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), AppContributionError> {
    if value.len() != 71
        || !value.starts_with("blake3:")
        || !value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_control("frequency proposal digest is invalid"));
    }
    Ok(())
}

fn timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn invalid_control(message: &str) -> AppContributionError {
    AppRegistryError::InvalidControlPlane(message.to_owned()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_identity_is_port_and_revision_specific() {
        let digest = content_digest(b"proposal");
        let first = frequency_event_id(
            "installation",
            "workflow",
            "action",
            "port",
            "scope",
            "proposal",
            1,
            &digest,
        );
        let replay = frequency_event_id(
            "installation",
            "workflow",
            "action",
            "port",
            "scope",
            "proposal",
            1,
            &digest,
        );
        let next = frequency_event_id(
            "installation",
            "workflow",
            "action",
            "port",
            "scope",
            "proposal",
            2,
            &digest,
        );
        assert_eq!(first, replay);
        assert_ne!(first, next);
    }

    #[test]
    fn reviewed_frequency_is_bounded() {
        assert!(AppReviewedContributionFrequencyV1::from_reviewed_limit(1, 1).is_ok());
        assert!(AppReviewedContributionFrequencyV1::from_reviewed_limit(0, 1).is_err());
        assert!(AppReviewedContributionFrequencyV1::from_reviewed_limit(1, 0).is_err());
    }
}
