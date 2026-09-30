//! Durable Phase 5B app-memory candidate store and live source resolver.
//!
//! Candidates persist in the scoped app-registry SQLite database so mutate,
//! disable, retain, forget and purge can settle them in the same transaction.
//! Prompt inclusion re-reads that store; a missing database or candidate fails
//! closed instead of trusting a cached envelope.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    authority::AuthenticatedAppScope,
    lifecycle::AppInstallationStatus,
    memory::{
        resolve_app_memory_eligibility, AppMemoryCandidate, AppMemoryCandidateCommand,
        AppMemoryCandidateLifecycleError, AppMemoryCandidateStatus, AppMemoryEligibilityError,
        AppMemoryResolvedSourceState, AppMemorySourceRef, ResolvedAppMemorySource,
    },
    memory_bridge::{
        evaluate_app_memory_retrieval, parse_source_eligibility_envelope,
        settle_app_memory_candidate, AppMemoryBridgeError, AppMemoryEnvelopeSource,
        AppMemoryRetrievalDecision, AppMemorySourceEligibilityEnvelope,
    },
    models::{
        decode_app_contract, decode_bounded_json_value, AppContractError, AppContractLimits,
        AppDigest, AppInstallationId, AppName, AppRecordId, AppReference, AppRevision,
        ValidateAppContract,
    },
    policy::ResolvedAppDataHandlingPolicy,
    records::{validate_policy, AppDataHandlingPolicy, AppInstallation, AppScope},
    registry::{
        encode_bounded_json, format_timestamp, open_existing_scoped_registry_read_only,
        AppRegistryError, AppRegistryService,
    },
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const ACTIVE_CANDIDATE_STATUSES: &[&str] = &["proposed", "accepted"];
const APP_MEMORY_INDEX_MAX_CANDIDATES: usize = 4_096;

/// Atomic publication decision made by the durable candidate-store owner.
/// An exact idempotent replay is recovered before cancellation is considered;
/// only a genuinely missing candidate can settle as a no-effect cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppMemoryCandidatePublication {
    Published(AppMemoryCandidate),
    Recovered(AppMemoryCandidate),
    Cancelled { candidate_id: AppReference },
}

/// Prompt target used to select the relevant app-memory rows before applying
/// the bounded result limit. Keeping this independent of agent prompt types
/// avoids coupling the app registry to the rendering subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppMemoryPromptTarget {
    User,
    Agent { agent_id: String },
    AgentGoal { agent_id: String, goal_id: String },
}

#[derive(Debug, Error)]
pub enum AppMemoryStoreError {
    #[error(transparent)]
    Eligibility(#[from] AppMemoryEligibilityError),
    #[error(transparent)]
    Lifecycle(#[from] AppMemoryCandidateLifecycleError),
    #[error(transparent)]
    Bridge(#[from] AppMemoryBridgeError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error("app-memory store SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("app-memory store encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("app-memory candidate `{0}` is corrupt")]
    CorruptCandidate(String),
    #[error("app-memory candidate compare-and-swap lost")]
    CompareAndSwapLost,
    #[error("app-memory candidate publication is not an allowed lifecycle transition")]
    InvalidLifecycleTransition,
    #[error("app-memory index projection failed after destination commit: {0}")]
    IndexProjection(String),
}

impl AppRegistryService {
    /// Reconcile the content-bearing derived index projection only after the
    /// registry owner has committed its canonical lifecycle/source mutation.
    /// Replays deliberately reach this seam so a crash after commit but before
    /// the content-free journal signal repairs itself deterministically.
    pub(crate) async fn synchronize_app_memory_index_projection_for_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppMemoryStoreError> {
        let memory =
            crate::magician_v2::agents::AgentMemoryService::with_scoped_memory_scope_in_workspace(
                self.workspace_layout().clone(),
                authenticated.scope().principal.as_str(),
                authenticated.scope().workspace.as_str(),
            );
        crate::magician_v2::agents::memory_prompt_blocks::synchronize_app_memory_index_projection(
            &memory, now,
        )
        .await
        .map(|_| ())
        .map_err(|error| AppMemoryStoreError::IndexProjection(error.to_string()))
    }

    pub async fn tombstone_app_memory_index_projection_for_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
        invalidation_binding: &str,
    ) -> Result<(), AppMemoryStoreError> {
        let memory =
            crate::magician_v2::agents::AgentMemoryService::with_scoped_memory_scope_in_workspace(
                self.workspace_layout().clone(),
                authenticated.scope().principal.as_str(),
                authenticated.scope().workspace.as_str(),
            );
        crate::magician_v2::agents::memory_prompt_blocks::tombstone_app_memory_index_projection(
            &memory,
            invalidation_binding,
        )
        .await
        .map_err(|error| AppMemoryStoreError::IndexProjection(error.to_string()))
    }

    pub(crate) async fn publish_app_memory_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        candidate: AppMemoryCandidate,
        cancellation: Option<CancellationToken>,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryCandidatePublication, AppMemoryStoreError> {
        candidate.validate_app_contract(&AppContractLimits::default())?;
        if candidate.status != AppMemoryCandidateStatus::Proposed
            || candidate.candidate_revision != AppRevision::new(1)?
            || candidate.proposed_at != now
            || candidate.updated_at != now
            || candidate.scope != *authenticated.scope()
        {
            return Err(AppMemoryStoreError::InvalidLifecycleTransition);
        }
        self.execute_scoped_typed_write(authenticated, &now, move |connection, _scope| {
            let transaction = connection.transaction()?;
            let publication = publish_app_memory_candidate_in_transaction(
                &transaction,
                candidate,
                cancellation.as_ref(),
            )?;
            transaction.commit()?;
            Ok(publication)
        })
        .await
    }

    pub async fn current_app_memory_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        candidate_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppMemoryCandidate>, AppMemoryStoreError> {
        let loaded = self
            .execute_scoped_typed_read(authenticated, &now, move |connection, scope| {
                let candidate = load_app_memory_candidate(connection, &candidate_id)?;
                if candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.scope != *scope)
                {
                    return Err(AppMemoryStoreError::CorruptCandidate(
                        candidate_id.to_string(),
                    ));
                }
                Ok(candidate)
            })
            .await?;
        Ok(loaded.flatten())
    }

    /// Bounded owner-review projection ordered deterministically by newest
    /// lifecycle update. Candidate bodies are already scoped registry records;
    /// this method never searches source app data or treats list membership as
    /// retrieval eligibility.
    pub async fn list_app_memory_candidates(
        &self,
        authenticated: &AuthenticatedAppScope,
        status: Option<AppMemoryCandidateStatus>,
        after: Option<(DateTime<Utc>, AppReference)>,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppMemoryCandidate>, AppMemoryStoreError> {
        if limit == 0 || limit > AppContractLimits::default().max_collection_items() {
            return Err(AppContractError::invalid(
                "limit",
                "must be within the bounded app-memory review page",
            )
            .into());
        }
        let limit = i64::try_from(limit).map_err(|_| {
            AppMemoryStoreError::CorruptCandidate(
                "app-memory review limit exceeds SQLite range".to_owned(),
            )
        })?;
        let loaded = self
            .execute_scoped_typed_read(authenticated, &now, move |connection, scope| {
                let candidates = match (status, after) {
                    (Some(status), Some((updated_at, candidate_id))) => load_candidates_by_sql(
                        connection,
                        "SELECT record_json
                           FROM app_memory_candidates
                          WHERE status = ?1
                            AND (updated_at < ?2
                                 OR (updated_at = ?2 AND candidate_id > ?3))
                          ORDER BY updated_at DESC, candidate_id ASC
                          LIMIT ?4",
                        params![
                            status_label(status),
                            format_timestamp(&updated_at),
                            candidate_id.as_str(),
                            limit
                        ],
                    ),
                    (Some(status), None) => load_candidates_by_sql(
                        connection,
                        "SELECT record_json
                           FROM app_memory_candidates
                          WHERE status = ?1
                          ORDER BY updated_at DESC, candidate_id ASC
                          LIMIT ?2",
                        params![status_label(status), limit],
                    ),
                    (None, Some((updated_at, candidate_id))) => load_candidates_by_sql(
                        connection,
                        "SELECT record_json
                           FROM app_memory_candidates
                          WHERE updated_at < ?1
                             OR (updated_at = ?1 AND candidate_id > ?2)
                          ORDER BY updated_at DESC, candidate_id ASC
                          LIMIT ?3",
                        params![format_timestamp(&updated_at), candidate_id.as_str(), limit],
                    ),
                    (None, None) => load_candidates_by_sql(
                        connection,
                        "SELECT record_json
                           FROM app_memory_candidates
                          ORDER BY updated_at DESC, candidate_id ASC
                          LIMIT ?1",
                        params![limit],
                    ),
                }?;
                if let Some(candidate) = candidates
                    .iter()
                    .find(|candidate| candidate.scope != *scope)
                {
                    return Err(AppMemoryStoreError::CorruptCandidate(
                        candidate.candidate_id.to_string(),
                    ));
                }
                Ok(candidates)
            })
            .await?;
        Ok(loaded.unwrap_or_default())
    }

    /// Apply one reviewed lifecycle command against the current durable row.
    /// Acceptance is additionally fenced by a live source resolve in the same
    /// SQLite transaction, so an updated, disabled or policy-revoked record
    /// cannot become eligible between proposal and owner approval.
    pub async fn transition_app_memory_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        candidate_id: AppReference,
        command: AppMemoryCandidateCommand,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryCandidate, AppMemoryStoreError> {
        let result = self
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                let transaction = connection.transaction()?;
                let current =
                    load_app_memory_candidate(&transaction, &candidate_id)?.ok_or_else(|| {
                        AppMemoryStoreError::CorruptCandidate(candidate_id.to_string())
                    })?;
                if current.scope != *scope {
                    return Err(AppMemoryStoreError::CorruptCandidate(
                        candidate_id.to_string(),
                    ));
                }
                if matches!(
                    (current.status, command),
                    (
                        AppMemoryCandidateStatus::Accepted,
                        AppMemoryCandidateCommand::Accept
                    ) | (
                        AppMemoryCandidateStatus::Rejected,
                        AppMemoryCandidateCommand::Reject
                    )
                ) {
                    transaction.commit()?;
                    return Ok(current);
                }
                let next = current.apply(command, now)?;
                if command == AppMemoryCandidateCommand::Accept {
                    let live = resolve_live_app_memory_sources(&transaction, scope, &next, now)?;
                    let live_refs = live.iter().collect::<Vec<_>>();
                    resolve_app_memory_eligibility(
                        &next,
                        &live_refs,
                        now,
                        &AppContractLimits::default(),
                    )?;
                }
                persist_app_memory_candidate_cas(&transaction, &current, &next)?;
                transaction.commit()?;
                Ok(next)
            })
            .await?;
        self.synchronize_app_memory_index_projection_for_scope(authenticated, now)
            .await?;
        Ok(result)
    }
}

/// Proposal retries may carry a later wall-clock timestamp, but they must not
/// edit any identity-bearing candidate material. Both records have already
/// passed contract validation, so the sealed fingerprint binds the scope,
/// sources, claim digest, policy labels and destination; the durable id and
/// revision are checked separately here.
fn is_idempotent_proposal_replay(
    existing: &AppMemoryCandidate,
    candidate: &AppMemoryCandidate,
) -> bool {
    existing.status == AppMemoryCandidateStatus::Proposed
        && candidate.status == AppMemoryCandidateStatus::Proposed
        && existing.candidate_id == candidate.candidate_id
        && existing.candidate_revision == candidate.candidate_revision
        && existing.candidate_fingerprint == candidate.candidate_fingerprint
}

fn publish_app_memory_candidate_in_transaction(
    transaction: &Transaction<'_>,
    candidate: AppMemoryCandidate,
    cancellation: Option<&CancellationToken>,
) -> Result<AppMemoryCandidatePublication, AppMemoryStoreError> {
    if let Some(existing) = load_app_memory_candidate(transaction, &candidate.candidate_id)? {
        if existing == candidate || is_idempotent_proposal_replay(&existing, &candidate) {
            return Ok(AppMemoryCandidatePublication::Recovered(existing));
        }
        return Err(AppMemoryStoreError::InvalidLifecycleTransition);
    }

    // This is the final no-effect boundary: the per-scope write owner has
    // proven that no replay exists and still holds serialization through the
    // insert below. Cancellation after this check is a late Stop; the
    // non-droppable caller observes the committed Published outcome.
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(AppMemoryCandidatePublication::Cancelled {
            candidate_id: candidate.candidate_id,
        });
    }
    persist_new_app_memory_candidate(transaction, &candidate)?;
    Ok(AppMemoryCandidatePublication::Published(candidate))
}

pub fn registry_memory_store_error(error: AppMemoryStoreError) -> AppRegistryError {
    match error {
        AppMemoryStoreError::Registry(error) => error,
        other => AppRegistryError::StateConflict(other.to_string()),
    }
}

#[derive(Debug, Clone)]
struct StoreRecordHead {
    record_revision: AppRevision,
    schema_revision: AppRevision,
    payload: Value,
    handling_policy: AppDataHandlingPolicy,
    deleted_at: Option<DateTime<Utc>>,
}

pub fn persist_new_app_memory_candidate(
    transaction: &Transaction<'_>,
    candidate: &AppMemoryCandidate,
) -> Result<(), AppMemoryStoreError> {
    candidate.validate_app_contract(&AppContractLimits::default())?;
    let record_json = encode_bounded_json(candidate, &AppContractLimits::default())?;
    transaction.execute(
        "INSERT INTO app_memory_candidates (
             candidate_id, candidate_revision, candidate_fingerprint, status,
             record_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            candidate.candidate_id.as_str(),
            revision_i64(candidate.candidate_revision)?,
            candidate.candidate_fingerprint.as_str(),
            status_label(candidate.status),
            record_json,
            format_timestamp(&candidate.proposed_at),
            format_timestamp(&candidate.updated_at),
        ],
    )?;
    replace_candidate_sources(transaction, candidate)?;
    Ok(())
}

pub fn persist_app_memory_candidate_cas(
    transaction: &Transaction<'_>,
    previous: &AppMemoryCandidate,
    next: &AppMemoryCandidate,
) -> Result<(), AppMemoryStoreError> {
    if previous.candidate_id != next.candidate_id {
        return Err(AppMemoryStoreError::CorruptCandidate(
            next.candidate_id.to_string(),
        ));
    }
    next.validate_app_contract(&AppContractLimits::default())?;
    let record_json = encode_bounded_json(next, &AppContractLimits::default())?;
    let updated = transaction.execute(
        "UPDATE app_memory_candidates
            SET candidate_revision = ?1,
                candidate_fingerprint = ?2,
                status = ?3,
                record_json = ?4,
                updated_at = ?5
          WHERE candidate_id = ?6
            AND candidate_revision = ?7
            AND candidate_fingerprint = ?8
            AND status = ?9",
        params![
            revision_i64(next.candidate_revision)?,
            next.candidate_fingerprint.as_str(),
            status_label(next.status),
            record_json,
            format_timestamp(&next.updated_at),
            previous.candidate_id.as_str(),
            revision_i64(previous.candidate_revision)?,
            previous.candidate_fingerprint.as_str(),
            status_label(previous.status),
        ],
    )?;
    if updated != 1 {
        return Err(AppMemoryStoreError::CompareAndSwapLost);
    }
    replace_candidate_sources(transaction, next)?;
    Ok(())
}

pub fn load_app_memory_candidate(
    connection: &Connection,
    candidate_id: &AppReference,
) -> Result<Option<AppMemoryCandidate>, AppMemoryStoreError> {
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record_json FROM app_memory_candidates WHERE candidate_id = ?1",
            params![candidate_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| decode_candidate(&bytes, candidate_id.as_str()))
        .transpose()
}

pub fn load_app_memory_candidates_for_record(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity_name: &AppName,
    record_id: &AppRecordId,
) -> Result<Vec<AppMemoryCandidate>, AppMemoryStoreError> {
    load_candidates_by_sql(
        connection,
        "SELECT c.record_json
         FROM app_memory_candidates c
         JOIN app_memory_candidate_sources s
           ON s.candidate_id = c.candidate_id
         WHERE s.installation_id = ?1
           AND s.entity_name = ?2
           AND s.record_id = ?3
           AND c.status IN ('proposed', 'accepted')",
        params![
            installation_id.as_str(),
            entity_name.as_str(),
            record_id.as_str()
        ],
    )
}

pub fn load_app_memory_candidates_for_installation(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<Vec<AppMemoryCandidate>, AppMemoryStoreError> {
    load_candidates_by_sql(
        connection,
        "SELECT DISTINCT c.record_json
         FROM app_memory_candidates c
         JOIN app_memory_candidate_sources s
           ON s.candidate_id = c.candidate_id
         WHERE s.installation_id = ?1
           AND c.status IN ('proposed', 'accepted')",
        params![installation_id.as_str()],
    )
}

/// Settle every active candidate that cites a just-committed record. Must run
/// inside the same SQLite transaction as the record write.
pub fn settle_candidates_for_committed_records(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    records: &[(AppName, AppRecordId)],
    settled_at: DateTime<Utc>,
) -> Result<(), AppMemoryStoreError> {
    let mut seen = std::collections::HashSet::new();
    for (entity, record_id) in records {
        if !seen.insert((entity.as_str().to_owned(), record_id.as_str().to_owned())) {
            continue;
        }
        for candidate in
            load_app_memory_candidates_for_record(transaction, installation_id, entity, record_id)?
        {
            settle_one_candidate(transaction, scope, &candidate, settled_at)?;
        }
    }
    Ok(())
}

/// Settle every active candidate that cites an installation whose lifecycle
/// just became disable/retain/quarantine/purge. Must run in the same
/// transaction as the installation CAS.
pub fn settle_candidates_for_installation(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    settled_at: DateTime<Utc>,
) -> Result<(), AppMemoryStoreError> {
    for candidate in load_app_memory_candidates_for_installation(transaction, installation_id)? {
        settle_one_candidate(transaction, scope, &candidate, settled_at)?;
    }
    Ok(())
}

pub fn resolve_live_app_memory_sources(
    connection: &Connection,
    scope: &AppScope,
    candidate: &AppMemoryCandidate,
    resolved_at: DateTime<Utc>,
) -> Result<Vec<ResolvedAppMemorySource>, AppMemoryStoreError> {
    let mut resolved = Vec::with_capacity(candidate.source_refs.len());
    for source in &candidate.source_refs {
        resolved.push(resolve_one_live_source(
            connection,
            scope,
            source,
            resolved_at,
        )?);
    }
    Ok(resolved)
}

/// Prompt-time live resolve. Missing workspace, scope, store or candidate
/// fails closed. Ordinary memories without an envelope stay eligible.
pub fn resolve_prompt_app_memory_eligibility(
    workspace: Option<&ArtifactV2Workspace>,
    principal: Option<&str>,
    workspace_name: Option<&str>,
    metadata: &Value,
    resolved_at: DateTime<Utc>,
) -> bool {
    match parse_source_eligibility_envelope(metadata) {
        None => true,
        Some(Err(_)) => false,
        Some(Ok(envelope)) => {
            let Some(workspace) = workspace else {
                return false;
            };
            let (Some(principal), Some(workspace_name)) = (principal, workspace_name) else {
                return false;
            };
            match resolve_envelope_from_existing_store(
                workspace,
                principal,
                workspace_name,
                &envelope,
                resolved_at,
            ) {
                Ok(true) => true,
                Ok(false) | Err(_) => false,
            }
        },
    }
}

pub fn resolve_prompt_app_memory_eligibility_batch(
    workspace: Option<&ArtifactV2Workspace>,
    principal: Option<&str>,
    workspace_name: Option<&str>,
    metadatas: impl IntoIterator<Item = Value>,
    resolved_at: DateTime<Utc>,
) -> HashMap<String, bool> {
    let mut decisions = HashMap::new();
    let Some(workspace) = workspace else {
        for metadata in metadatas {
            if let Some(Ok(envelope)) = parse_source_eligibility_envelope(&metadata) {
                decisions.insert(envelope.candidate_id.to_string(), false);
            }
        }
        return decisions;
    };
    let (Some(principal), Some(workspace_name)) = (principal, workspace_name) else {
        for metadata in metadatas {
            if let Some(Ok(envelope)) = parse_source_eligibility_envelope(&metadata) {
                decisions.insert(envelope.candidate_id.to_string(), false);
            }
        }
        return decisions;
    };
    let Ok(scope) = (|| {
        Ok::<_, AppMemoryStoreError>(AppScope {
            principal: AppReference::parse(principal)?,
            workspace: AppReference::parse(workspace_name)?,
        })
    })() else {
        for metadata in metadatas {
            if let Some(Ok(envelope)) = parse_source_eligibility_envelope(&metadata) {
                decisions.insert(envelope.candidate_id.to_string(), false);
            }
        }
        return decisions;
    };
    let connection = match open_existing_scoped_registry_read_only(workspace, &scope) {
        Ok(Some(connection)) => connection,
        Ok(None) | Err(_) => {
            for metadata in metadatas {
                if let Some(Ok(envelope)) = parse_source_eligibility_envelope(&metadata) {
                    decisions.insert(envelope.candidate_id.to_string(), false);
                }
            }
            return decisions;
        },
    };
    for metadata in metadatas {
        if let Some(Ok(envelope)) = parse_source_eligibility_envelope(&metadata) {
            let allowed =
                envelope_is_live(&connection, &scope, &envelope, resolved_at).unwrap_or(false);
            decisions.insert(envelope.candidate_id.to_string(), allowed);
        }
    }
    decisions
}

/// Load accepted app-memory candidates that are eligible at this exact prompt
/// boundary. The app registry remains their canonical store; prompt retrieval
/// projects them directly instead of copying claims into a second JSON tier
/// whose lifecycle could drift from the source record.
pub(crate) fn load_prompt_eligible_app_memory_candidates(
    workspace: Option<&ArtifactV2Workspace>,
    principal: Option<&str>,
    workspace_name: Option<&str>,
    target: &AppMemoryPromptTarget,
    resolved_at: DateTime<Utc>,
) -> Result<Vec<AppMemoryCandidate>, AppMemoryStoreError> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let (Some(principal), Some(workspace_name)) = (principal, workspace_name) else {
        return Ok(Vec::new());
    };
    let scope = AppScope {
        principal: AppReference::parse(principal)?,
        workspace: AppReference::parse(workspace_name)?,
    };
    let Some(connection) = open_existing_scoped_registry_read_only(workspace, &scope)? else {
        return Ok(Vec::new());
    };
    let limit =
        i64::try_from(AppContractLimits::default().max_collection_items()).unwrap_or(i64::MAX);
    // Filter the target in SQLite before LIMIT. A global newest-first limit
    // would allow candidates for other agents or goals to crowd the requested
    // prompt out of its bounded window.
    let candidates = match target {
        AppMemoryPromptTarget::User => load_candidates_by_sql(
            &connection,
            "SELECT record_json
              FROM app_memory_candidates
              WHERE status = 'accepted'
                AND json_extract(CAST(record_json AS TEXT),
                                 '$.intended_tier_scope.kind') = 'user'
              ORDER BY updated_at DESC, candidate_id ASC
              LIMIT ?1",
            params![limit],
        )?,
        AppMemoryPromptTarget::Agent { agent_id } => {
            let (agent_ref, alternate_agent_ref) = app_reference_spellings("agent", agent_id);
            load_candidates_by_sql(
                &connection,
                "SELECT record_json
                   FROM app_memory_candidates
                  WHERE status = 'accepted'
                    AND json_extract(CAST(record_json AS TEXT),
                                     '$.intended_tier_scope.kind') = 'agent'
                    AND json_extract(CAST(record_json AS TEXT),
                                     '$.intended_tier_scope.agent_id') IN (?1, ?2)
                  ORDER BY updated_at DESC, candidate_id ASC
                  LIMIT ?3",
                params![agent_ref, alternate_agent_ref, limit],
            )?
        },
        AppMemoryPromptTarget::AgentGoal { agent_id, goal_id } => {
            let (agent_ref, alternate_agent_ref) = app_reference_spellings("agent", agent_id);
            let (goal_ref, alternate_goal_ref) = app_reference_spellings("goal", goal_id);
            load_candidates_by_sql(
                &connection,
                "SELECT record_json
                   FROM app_memory_candidates
                  WHERE status = 'accepted'
                    AND json_extract(CAST(record_json AS TEXT),
                                     '$.intended_tier_scope.kind') = 'agent_goal'
                    AND json_extract(CAST(record_json AS TEXT),
                                     '$.intended_tier_scope.agent_id') IN (?1, ?2)
                    AND json_extract(CAST(record_json AS TEXT),
                                     '$.intended_tier_scope.goal_id') IN (?3, ?4)
                  ORDER BY updated_at DESC, candidate_id ASC
                  LIMIT ?5",
                params![
                    agent_ref,
                    alternate_agent_ref,
                    goal_ref,
                    alternate_goal_ref,
                    limit
                ],
            )?
        },
    };
    let mut eligible = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let live =
            match resolve_live_app_memory_sources(&connection, &scope, &candidate, resolved_at) {
                Ok(live) => live,
                Err(_) => continue,
            };
        let live_refs = live.iter().collect::<Vec<_>>();
        if matches!(
            evaluate_app_memory_retrieval(&candidate, &live_refs, resolved_at),
            AppMemoryRetrievalDecision::Eligible(_)
        ) {
            eligible.push(candidate);
        }
    }
    Ok(eligible)
}

/// Destination-owner enumeration used only to materialize the derived hybrid
/// index projection. Unlike prompt selection this spans all declared target
/// scopes, but it still reopens every exact source and returns only accepted,
/// currently eligible candidates. Personal-agent retrieval projections remain
/// a distinct lane and never enter this enumeration.
pub(crate) fn load_indexable_app_memory_candidates(
    workspace: Option<&ArtifactV2Workspace>,
    principal: Option<&str>,
    workspace_name: Option<&str>,
    resolved_at: DateTime<Utc>,
) -> Result<Vec<AppMemoryCandidate>, AppMemoryStoreError> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let (Some(principal), Some(workspace_name)) = (principal, workspace_name) else {
        return Ok(Vec::new());
    };
    let scope = AppScope {
        principal: AppReference::parse(principal)?,
        workspace: AppReference::parse(workspace_name)?,
    };
    let Some(connection) = open_existing_scoped_registry_read_only(workspace, &scope)? else {
        return Ok(Vec::new());
    };
    let candidates = load_candidates_by_sql(
        &connection,
        "SELECT record_json
           FROM app_memory_candidates
          WHERE status = 'accepted'
          ORDER BY updated_at DESC, candidate_id ASC
          LIMIT ?1",
        params![i64::try_from(APP_MEMORY_INDEX_MAX_CANDIDATES).unwrap_or(i64::MAX)],
    )?;
    let mut eligible = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let live =
            match resolve_live_app_memory_sources(&connection, &scope, &candidate, resolved_at) {
                Ok(live) => live,
                Err(_) => continue,
            };
        let live_refs = live.iter().collect::<Vec<_>>();
        if matches!(
            evaluate_app_memory_retrieval(&candidate, &live_refs, resolved_at),
            AppMemoryRetrievalDecision::Eligible(_)
        ) {
            eligible.push(candidate);
        }
    }
    Ok(eligible)
}

fn app_reference_spellings(kind: &str, value: &str) -> (String, String) {
    let prefix = format!("{kind}:");
    match value.strip_prefix(&prefix) {
        Some(unqualified) => (value.to_owned(), unqualified.to_owned()),
        None => (value.to_owned(), format!("{prefix}{value}")),
    }
}

fn resolve_envelope_from_existing_store(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    envelope: &AppMemorySourceEligibilityEnvelope,
    resolved_at: DateTime<Utc>,
) -> Result<bool, AppMemoryStoreError> {
    let scope = AppScope {
        principal: AppReference::parse(principal)?,
        workspace: AppReference::parse(workspace_name)?,
    };
    let Some(connection) = open_existing_scoped_registry_read_only(workspace, &scope)? else {
        return Ok(false);
    };
    envelope_is_live(&connection, &scope, envelope, resolved_at)
}

/// Revalidate an accepted destination-owned contribution against the same
/// current installation/grant/entity/policy owner used by legacy app-memory
/// candidates, without requiring a second mutable candidate row in SQLite.
/// The filesystem receipt/projection remains the sole lifecycle owner.
pub(crate) fn projected_app_memory_candidates_are_live(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    candidates: &[AppMemoryCandidate],
    resolved_at: DateTime<Utc>,
) -> Result<Vec<bool>, AppMemoryStoreError> {
    let scope = AppScope {
        principal: AppReference::parse(principal.to_owned())?,
        workspace: AppReference::parse(workspace_name.to_owned())?,
    };
    let Some(connection) = open_existing_scoped_registry_read_only(workspace, &scope)? else {
        return Ok(vec![false; candidates.len()]);
    };
    let mut live_candidates = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate
            .validate_app_contract(&AppContractLimits::default())
            .is_err()
            || candidate.scope != scope
            || candidate.status != AppMemoryCandidateStatus::Accepted
        {
            live_candidates.push(false);
            continue;
        }
        let live = resolve_live_app_memory_sources(&connection, &scope, candidate, resolved_at)?;
        let live_refs = live.iter().collect::<Vec<_>>();
        live_candidates.push(matches!(
            evaluate_app_memory_retrieval(candidate, &live_refs, resolved_at),
            AppMemoryRetrievalDecision::Eligible(_)
        ));
    }
    Ok(live_candidates)
}

fn envelope_is_live(
    connection: &Connection,
    scope: &AppScope,
    envelope: &AppMemorySourceEligibilityEnvelope,
    resolved_at: DateTime<Utc>,
) -> Result<bool, AppMemoryStoreError> {
    let Some(candidate) = load_app_memory_candidate(connection, &envelope.candidate_id)? else {
        return Ok(false);
    };
    if candidate.candidate_revision != envelope.candidate_revision
        || candidate.candidate_fingerprint != envelope.candidate_fingerprint
        || !sources_match_envelope(&candidate, &envelope.sources)
    {
        return Ok(false);
    }
    let live = resolve_live_app_memory_sources(connection, scope, &candidate, resolved_at)?;
    let live_refs: Vec<&ResolvedAppMemorySource> = live.iter().collect();
    Ok(matches!(
        evaluate_app_memory_retrieval(&candidate, &live_refs, resolved_at),
        AppMemoryRetrievalDecision::Eligible(_)
    ))
}

fn sources_match_envelope(
    candidate: &AppMemoryCandidate,
    envelope_sources: &[AppMemoryEnvelopeSource],
) -> bool {
    if candidate.source_refs.len() != envelope_sources.len() {
        return false;
    }
    candidate.source_refs.iter().all(|known| {
        envelope_sources.iter().any(|envelope| {
            envelope.installation_id == known.installation_id
                && envelope.entity_name == known.entity_name
                && envelope.record_id == known.record_id
                && envelope.record_revision == known.record_revision
        })
    })
}

fn settle_one_candidate(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    candidate: &AppMemoryCandidate,
    settled_at: DateTime<Utc>,
) -> Result<(), AppMemoryStoreError> {
    let live = resolve_live_app_memory_sources(transaction, scope, candidate, settled_at)?;
    let live_refs: Vec<&ResolvedAppMemorySource> = live.iter().collect();
    match settle_app_memory_candidate(candidate, &live_refs, settled_at)? {
        AppMemoryRetrievalDecision::Settled(next) => {
            persist_app_memory_candidate_cas(transaction, candidate, &next)
        },
        AppMemoryRetrievalDecision::Eligible(_) | AppMemoryRetrievalDecision::FailClosed(_) => {
            Ok(())
        },
    }
}

fn resolve_one_live_source(
    connection: &Connection,
    scope: &AppScope,
    prior: &AppMemorySourceRef,
    resolved_at: DateTime<Utc>,
) -> Result<ResolvedAppMemorySource, AppMemoryStoreError> {
    let Some(installation) = load_installation(connection, &prior.installation_id)? else {
        return Ok(ResolvedAppMemorySource::from_trusted_store_snapshot(
            scope,
            prior.clone(),
            1,
            AppRevision::new(1)?,
            AppMemoryResolvedSourceState::Purged,
            Some(AppInstallationStatus::Purged),
            None,
            resolved_at,
        )?);
    };
    if installation.scope != *scope || installation.installation_id != prior.installation_id {
        return Err(AppMemoryEligibilityError::CrossScopeEvidence.into());
    }
    let eligibility_revision = AppRevision::new(installation.lifecycle.generation.max(1))?;
    match installation.lifecycle.status {
        AppInstallationStatus::Enabled => {
            resolve_enabled_record_source(connection, scope, prior, &installation, resolved_at)
        },
        AppInstallationStatus::Purged => Ok(ResolvedAppMemorySource::from_trusted_store_snapshot(
            scope,
            prior.clone(),
            installation.lifecycle.generation,
            eligibility_revision,
            AppMemoryResolvedSourceState::Purged,
            Some(AppInstallationStatus::Purged),
            None,
            resolved_at,
        )?),
        status => Ok(ResolvedAppMemorySource::from_trusted_store_snapshot(
            scope,
            prior.clone(),
            installation.lifecycle.generation,
            eligibility_revision,
            AppMemoryResolvedSourceState::Dormant,
            Some(status),
            None,
            resolved_at,
        )?),
    }
}

fn resolve_enabled_record_source(
    connection: &Connection,
    scope: &AppScope,
    prior: &AppMemorySourceRef,
    installation: &AppInstallation,
    resolved_at: DateTime<Utc>,
) -> Result<ResolvedAppMemorySource, AppMemoryStoreError> {
    let eligibility_revision = AppRevision::new(installation.lifecycle.generation.max(1))?;
    let Some(record) = load_record_head(
        connection,
        &prior.installation_id,
        &prior.entity_name,
        &prior.record_id,
    )?
    else {
        return Ok(ResolvedAppMemorySource::from_trusted_store_snapshot(
            scope,
            prior.clone(),
            installation.lifecycle.generation,
            eligibility_revision,
            AppMemoryResolvedSourceState::Purged,
            Some(AppInstallationStatus::Purged),
            None,
            resolved_at,
        )?);
    };
    let mut current = prior.clone();
    current.package_revision_ref = installation.package_revision_ref.clone();
    if let Some(grant_revision) = installation.grant_revision {
        current.grant_revision = grant_revision;
    }
    current.schema_revision = record.schema_revision;
    current.record_revision = record.record_revision;
    current.canonical_source_ref.revision = Some(record.record_revision);
    if record.deleted_at.is_some() {
        return Ok(ResolvedAppMemorySource::from_trusted_store_snapshot(
            scope,
            current,
            installation.lifecycle.generation,
            eligibility_revision,
            AppMemoryResolvedSourceState::Deleted,
            None,
            None,
            resolved_at,
        )?);
    }
    let policy = record.handling_policy.clone();
    let effective_policy = ResolvedAppDataHandlingPolicy::from_trusted_store_policy(
        policy.clone(),
        AppDigest::blake3(b"app-memory-store-policy"),
        resolved_at,
    );
    let state = if policy.memory_promotion != super::records::AppMemoryPromotion::CandidateAllowed {
        AppMemoryResolvedSourceState::PromotionRevoked
    } else if policy.model_processing == super::models::AppModelProcessing::None
        || current.handling_labels.model_processing == super::models::AppModelProcessing::None
    {
        AppMemoryResolvedSourceState::ModelProcessingDenied
    } else if current
        .selected_fields
        .iter()
        .any(|field| !field_path_exists(&record.payload, field))
    {
        // Keep identity but change policy digest so eligibility reports a
        // policy change rather than inventing a new source state.
        current.handling_labels.policy_digest = AppDigest::blake3(b"missing-source-field");
        AppMemoryResolvedSourceState::Eligible
    } else {
        AppMemoryResolvedSourceState::Eligible
    };
    Ok(ResolvedAppMemorySource::from_trusted_store_snapshot(
        scope,
        current,
        installation.lifecycle.generation,
        eligibility_revision,
        state,
        None,
        Some(effective_policy),
        resolved_at,
    )?)
}

fn load_installation(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<Option<AppInstallation>, AppMemoryStoreError> {
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| {
            decode_app_contract(&bytes, &AppContractLimits::default())
                .map_err(AppMemoryStoreError::from)
        })
        .transpose()
}

fn load_record_head(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity_name: &AppName,
    record_id: &AppRecordId,
) -> Result<Option<StoreRecordHead>, AppMemoryStoreError> {
    let row = connection
        .query_row(
            "SELECT h.record_revision, h.schema_revision, h.deleted_at,
                    r.payload_json, r.handling_policy_json
             FROM app_record_heads h
             JOIN app_record_revisions r
               ON r.installation_id = h.installation_id
              AND r.entity_name = h.entity_name
              AND r.record_id = h.record_id
              AND r.record_revision = h.record_revision
             WHERE h.installation_id = ?1 AND h.entity_name = ?2 AND h.record_id = ?3",
            params![
                installation_id.as_str(),
                entity_name.as_str(),
                record_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((revision, schema_revision, deleted_at, payload, policy)) = row else {
        return Ok(None);
    };
    Ok(Some(StoreRecordHead {
        record_revision: AppRevision::new(u64_from_i64(revision)?)?,
        schema_revision: AppRevision::new(u64_from_i64(schema_revision)?)?,
        payload: decode_bounded_json_value(&payload, &AppContractLimits::default())?,
        handling_policy: {
            let value = decode_bounded_json_value(&policy, &AppContractLimits::default())?;
            let policy: AppDataHandlingPolicy = serde_json::from_value(value)?;
            validate_policy(&policy, &AppContractLimits::default())?;
            policy
        },
        deleted_at: deleted_at
            .as_deref()
            .map(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|value| value.with_timezone(&Utc))
                    .map_err(|error| {
                        AppMemoryStoreError::CorruptCandidate(format!(
                            "invalid record deleted_at: {error}"
                        ))
                    })
            })
            .transpose()?,
    }))
}

fn replace_candidate_sources(
    transaction: &Transaction<'_>,
    candidate: &AppMemoryCandidate,
) -> Result<(), AppMemoryStoreError> {
    transaction.execute(
        "DELETE FROM app_memory_candidate_sources WHERE candidate_id = ?1",
        params![candidate.candidate_id.as_str()],
    )?;
    for source in &candidate.source_refs {
        transaction.execute(
            "INSERT INTO app_memory_candidate_sources (
                 candidate_id, installation_id, entity_name, record_id, record_revision
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                candidate.candidate_id.as_str(),
                source.installation_id.as_str(),
                source.entity_name.as_str(),
                source.record_id.as_str(),
                revision_i64(source.record_revision)?,
            ],
        )?;
    }
    Ok(())
}

fn load_candidates_by_sql(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<AppMemoryCandidate>, AppMemoryStoreError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params, |row| row.get::<_, Vec<u8>>(0))?;
    let mut candidates = Vec::new();
    for row in rows {
        candidates.push(decode_candidate(&row?, "app-memory-candidate")?);
    }
    Ok(candidates)
}

fn decode_candidate(
    bytes: &[u8],
    identity: &str,
) -> Result<AppMemoryCandidate, AppMemoryStoreError> {
    let candidate: AppMemoryCandidate = decode_app_contract(bytes, &AppContractLimits::default())?;
    if identity != "app-memory-candidate" && candidate.candidate_id.as_str() != identity {
        return Err(AppMemoryStoreError::CorruptCandidate(identity.to_owned()));
    }
    let _ = ACTIVE_CANDIDATE_STATUSES;
    Ok(candidate)
}

fn status_label(status: super::memory::AppMemoryCandidateStatus) -> &'static str {
    match status {
        super::memory::AppMemoryCandidateStatus::Proposed => "proposed",
        super::memory::AppMemoryCandidateStatus::Accepted => "accepted",
        super::memory::AppMemoryCandidateStatus::Rejected => "rejected",
        super::memory::AppMemoryCandidateStatus::Stale => "stale",
        super::memory::AppMemoryCandidateStatus::Tombstoned => "tombstoned",
    }
}

fn revision_i64(revision: AppRevision) -> Result<i64, AppMemoryStoreError> {
    i64::try_from(revision.get()).map_err(|_| {
        AppMemoryStoreError::CorruptCandidate("revision exceeds SQLite integer range".to_owned())
    })
}

fn u64_from_i64(value: i64) -> Result<u64, AppMemoryStoreError> {
    u64::try_from(value)
        .map_err(|_| AppMemoryStoreError::CorruptCandidate("negative stored revision".to_owned()))
}

fn field_path_exists(value: &Value, field: &super::models::AppFieldPath) -> bool {
    let mut current = value;
    for segment in field.as_str().split('.') {
        let Some(next) = current.as_object().and_then(|object| object.get(segment)) else {
            return false;
        };
        current = next;
    }
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn create_memory_candidate_tables_for_test(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(super::registry::APP_REGISTRY_SCHEMA_V13)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::{
        memory::{AppMemoryCandidateStatus, AppMemorySemanticDestination, AppMemoryTierScope},
        memory_bridge::attach_source_eligibility_envelope,
        models::{
            AppDataClassification, AppFieldPath, AppHandlingLabels, AppModelProcessing,
            AppSourceRef, AppSourceRefKind,
        },
        records::{
            AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
        },
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 18, 1, 0, second)
            .single()
            .unwrap()
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn labels() -> AppHandlingLabels {
        AppHandlingLabels {
            classification: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            policy_digest: digest("field-policy"),
            provenance_digest: digest("field-provenance"),
        }
    }

    fn source_ref() -> AppMemorySourceRef {
        AppMemorySourceRef {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: reference("package:1"),
            grant_revision: AppRevision::new(1).unwrap(),
            schema_revision: AppRevision::new(1).unwrap(),
            entity_name: AppName::parse("person").unwrap(),
            record_id: AppRecordId::parse("record_1").unwrap(),
            record_revision: AppRevision::new(1).unwrap(),
            selected_fields: vec![AppFieldPath::parse("name").unwrap()],
            canonical_source_ref: AppSourceRef {
                kind: AppSourceRefKind::EntityField,
                reference: reference("entity:person/record:1"),
                revision: Some(AppRevision::new(1).unwrap()),
                fields: vec![AppFieldPath::parse("name").unwrap()],
            },
            handling_labels: labels(),
        }
    }

    fn accepted_candidate() -> AppMemoryCandidate {
        let source = source_ref();
        let mut stored = AppMemoryCandidate {
            protocol_version: super::super::models::AppProtocolVersion::V1,
            candidate_id: reference("memory:candidate:store"),
            candidate_revision: AppRevision::new(2).unwrap(),
            candidate_fingerprint: digest("pending"),
            scope: AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            intended_tier_scope: AppMemoryTierScope::User,
            semantic_destination: AppMemorySemanticDestination::Entities,
            source_refs: vec![source.clone()],
            derived_claim_or_summary: "Asha is a mentor".to_owned(),
            claim_content_digest: AppDigest::blake3_canonical_json(&json!("Asha is a mentor"))
                .unwrap(),
            handling_labels: labels(),
            evidence_and_provenance_refs: vec![source.canonical_source_ref],
            status: AppMemoryCandidateStatus::Accepted,
            proposed_at: time(11),
            updated_at: time(12),
        };
        stored.seal_fingerprint().unwrap();
        stored
            .validate_app_contract(&AppContractLimits::default())
            .expect("valid candidate");
        stored
    }

    fn proposed_candidate() -> AppMemoryCandidate {
        let mut candidate = accepted_candidate();
        candidate.candidate_revision = AppRevision::new(1).unwrap();
        candidate.status = AppMemoryCandidateStatus::Proposed;
        candidate.proposed_at = time(11);
        candidate.updated_at = time(11);
        candidate.seal_fingerprint().unwrap();
        candidate
            .validate_app_contract(&AppContractLimits::default())
            .expect("valid proposal");
        candidate
    }

    #[test]
    fn proposal_retry_identity_ignores_only_proposal_timestamps() {
        let existing = proposed_candidate();
        let mut retry = existing.clone();
        retry.proposed_at = time(12);
        retry.updated_at = time(12);
        retry
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();

        assert!(is_idempotent_proposal_replay(&existing, &retry));

        retry.derived_claim_or_summary = "Asha is a sponsor".to_owned();
        retry.claim_content_digest =
            AppDigest::blake3_canonical_json(&json!(retry.derived_claim_or_summary)).unwrap();
        retry.seal_fingerprint().unwrap();
        assert!(!is_idempotent_proposal_replay(&existing, &retry));
    }

    #[test]
    fn proposal_retry_cannot_cross_lifecycle_state() {
        let existing = proposed_candidate();
        let accepted = existing
            .apply(AppMemoryCandidateCommand::Accept, time(12))
            .unwrap();

        assert!(!is_idempotent_proposal_replay(&accepted, &existing));
        assert!(!is_idempotent_proposal_replay(&existing, &accepted));
    }

    #[test]
    fn publication_owner_recovers_exact_replay_before_cancellation() {
        let mut connection = Connection::open_in_memory().unwrap();
        create_memory_candidate_tables_for_test(&connection).unwrap();
        let candidate = proposed_candidate();

        {
            let transaction = connection.transaction().unwrap();
            let publication =
                publish_app_memory_candidate_in_transaction(&transaction, candidate.clone(), None)
                    .unwrap();
            assert!(matches!(
                publication,
                AppMemoryCandidatePublication::Published(ref published)
                    if published == &candidate
            ));
            transaction.commit().unwrap();
        }

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut retry = candidate.clone();
        retry.proposed_at = time(12);
        retry.updated_at = time(12);
        {
            let transaction = connection.transaction().unwrap();
            let publication = publish_app_memory_candidate_in_transaction(
                &transaction,
                retry,
                Some(&cancellation),
            )
            .unwrap();
            assert!(matches!(
                publication,
                AppMemoryCandidatePublication::Recovered(ref recovered)
                    if recovered == &candidate
            ));
            transaction.commit().unwrap();
        }

        let mut missing = proposed_candidate();
        missing.candidate_id = reference("memory:candidate:cancelled-before-publish");
        missing.seal_fingerprint().unwrap();
        let missing_id = missing.candidate_id.clone();
        {
            let transaction = connection.transaction().unwrap();
            let publication = publish_app_memory_candidate_in_transaction(
                &transaction,
                missing,
                Some(&cancellation),
            )
            .unwrap();
            assert_eq!(
                publication,
                AppMemoryCandidatePublication::Cancelled {
                    candidate_id: missing_id.clone(),
                }
            );
            transaction.commit().unwrap();
        }
        assert!(load_app_memory_candidate(&connection, &missing_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn prompt_target_matches_qualified_and_unqualified_references() {
        assert_eq!(
            app_reference_spellings("agent", "personal-assistant"),
            (
                "personal-assistant".to_owned(),
                "agent:personal-assistant".to_owned()
            )
        );
        assert_eq!(
            app_reference_spellings("goal", "goal:quarterly-plan"),
            (
                "goal:quarterly-plan".to_owned(),
                "quarterly-plan".to_owned()
            )
        );
    }

    #[test]
    fn persist_round_trips_accepted_candidate() {
        let mut connection = Connection::open_in_memory().unwrap();
        create_memory_candidate_tables_for_test(&connection).unwrap();
        let transaction = connection.transaction().unwrap();
        let stored = accepted_candidate();
        let source = stored.source_refs[0].clone();
        persist_new_app_memory_candidate(&transaction, &stored).unwrap();
        transaction.commit().unwrap();
        let loaded = load_app_memory_candidate(&connection, &stored.candidate_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, AppMemoryCandidateStatus::Accepted);
        assert_eq!(loaded.source_refs[0].record_id, source.record_id);
        assert_eq!(
            load_app_memory_candidates_for_record(
                &connection,
                &source.installation_id,
                &source.entity_name,
                &source.record_id,
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn missing_workspace_fails_closed_for_enveloped_prompt() {
        let mut metadata = json!({});
        let stored = accepted_candidate();
        attach_source_eligibility_envelope(&mut metadata, &stored);
        assert!(!resolve_prompt_app_memory_eligibility(
            None,
            Some("anonymous"),
            Some("default"),
            &metadata,
            time(13),
        ));
        assert!(resolve_prompt_app_memory_eligibility(
            None,
            None,
            None,
            &json!({"candidate_kind": "tier"}),
            time(13),
        ));
    }

    #[test]
    fn store_snapshot_marks_updated_record_stale() {
        let prior = source_ref();
        let mut current = prior.clone();
        current.record_revision = AppRevision::new(2).unwrap();
        current.canonical_source_ref.revision = Some(AppRevision::new(2).unwrap());
        let policy = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::CandidateAllowed,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };
        let live = ResolvedAppMemorySource::from_trusted_store_snapshot(
            &AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            current,
            2,
            AppRevision::new(2).unwrap(),
            AppMemoryResolvedSourceState::Eligible,
            None,
            Some(ResolvedAppDataHandlingPolicy::from_trusted_store_policy(
                policy,
                digest("policy"),
                time(13),
            )),
            time(13),
        )
        .unwrap();
        let mut stored = accepted_candidate();
        stored.source_refs = vec![prior];
        stored.seal_fingerprint().unwrap();
        stored
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
        let AppMemoryRetrievalDecision::Settled(next) =
            settle_app_memory_candidate(&stored, &[&live], time(13)).unwrap()
        else {
            panic!("updated record must settle stale");
        };
        assert_eq!(next.status, AppMemoryCandidateStatus::Stale);
    }
}
