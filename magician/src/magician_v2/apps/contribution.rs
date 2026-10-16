//! Inert source-side contribution records.
//!
//! This module deliberately owns no destination decision. It validates the
//! exact shared candidate/invalidation documents that a future governed
//! workflow may append beside its canonical source settlement.

use chrono::{DateTime, SecondsFormat, Utc};
use magician_app_contract::contribution::{
    content_digest, AppContributionContractError, AppContributionRetractionPolicy,
    AppContributionUpdatePolicy, AppMemoryCandidateProposalV1, AppMemoryInvalidationReasonV1,
    AppMemoryInvalidationV1,
};
use rusqlite::Transaction;
use serde::Serialize;
use thiserror::Error;

use super::{
    approval_boundary::expected_app_installation_approval_ref,
    authority::AuthenticatedAppScope,
    lifecycle::AppLifecycleAttemptState,
    models::{
        AppContractError, AppContractLimits, AppDigest, AppName, AppReference, AppRevision,
        ValidateAppContract,
    },
    package_lock::{AppLockedContributionPortBinding, AppPackageLock},
    records::{
        app_granted_authority_digest, AppContributionDestinationBinding, AppContributionSource,
        AppGrantRevision, AppInstallation, AppInstallationApproval, AppLifecycleAttemptKind,
        AppReviewedContributionPortGrant, AppScope,
    },
    registry::{AppRegistryError, AppRegistryService},
};

pub const APP_MEMORY_OUTBOX_MAX_PENDING_PER_INSTALLATION: i64 = 1_024;
pub const APP_MEMORY_OUTBOX_MAX_ROWS_PER_SCOPE: i64 = 10_000;
pub const APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE: i64 = 64 * 1024 * 1024;
pub const APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE: i64 = 4_096;
pub const APP_MEMORY_OUTBOX_MAX_RECENT_TERMINALS_PER_KIND: i64 = 1_024;

/// Entity mutations carry a strictly newer record revision. Control-plane and
/// retention invalidations do not mutate the entity row, so they are bound to
/// the exact current record revision instead. Keeping the two cases explicit
/// prevents a lifecycle event from consuming the next real entity revision.
pub(crate) fn invalidation_advances_exact_source(
    reason: AppMemoryInvalidationReasonV1,
    invalidation_revision: u64,
    source_revision: u64,
) -> bool {
    match reason {
        AppMemoryInvalidationReasonV1::SourceUpdated
        | AppMemoryInvalidationReasonV1::SourceDeleted
        | AppMemoryInvalidationReasonV1::SourceRestored => invalidation_revision > source_revision,
        AppMemoryInvalidationReasonV1::SourceForgotten
        | AppMemoryInvalidationReasonV1::PolicyChanged
        | AppMemoryInvalidationReasonV1::GrantRevoked
        | AppMemoryInvalidationReasonV1::InstallationDisabled
        | AppMemoryInvalidationReasonV1::InstallationQuarantined
        | AppMemoryInvalidationReasonV1::InstallationUninstalledRetained
        | AppMemoryInvalidationReasonV1::InstallationPurged
        | AppMemoryInvalidationReasonV1::ContributionExpired => {
            invalidation_revision >= source_revision
        },
    }
}

#[derive(Debug, Error)]
pub enum AppContributionError {
    #[error("invalid contribution contract: {0}")]
    Contract(#[from] AppContributionContractError),
    #[error("invalid app contribution authority record: {0}")]
    ControlContract(#[from] AppContractError),
    #[error("app registry error: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("contribution serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("contribution quota exceeded: {0}")]
    Quota(&'static str),
    #[error("contribution replay substituted immutable bytes")]
    SubstitutedReplay,
    #[error("contribution replay is older than the bounded exact-replay window")]
    HistoryCompacted,
    #[error("app contribution authority refused: {0}")]
    Authority(String),
}

/// Move-only task-local contribution authority. It can be minted only by
/// intersecting the current grant with its consumed owner approval and exact
/// package lock. P6.3 may persist this inside its terminal intent; arbitrary
/// transport bytes cannot deserialize into authority.
#[derive(Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppContributionTaskBinding {
    schema: String,
    task_id: String,
    installation_id: super::models::AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    package_content_digest: AppDigest,
    package_lock_digest: AppDigest,
    grant_revision: super::models::AppRevision,
    grant_authority_digest: AppDigest,
    approval_id: AppReference,
    workflow_id: AppName,
    workflow_declaration_digest: AppDigest,
    action_id: AppName,
    action_declaration_digest: AppDigest,
    port_id: AppName,
    locked_port_digest: AppDigest,
    reviewed_grant_digest: AppDigest,
    destination_binding: AppContributionDestinationBinding,
    workflow_result_digest: AppDigest,
    reviewed_grant: AppReviewedContributionPortGrant,
    binding_digest: AppDigest,
}

#[allow(dead_code)] // Opaque binding inspectors are retained for downstream destination owners.
impl AppContributionTaskBinding {
    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn installation_id(&self) -> &super::models::AppInstallationId {
        &self.installation_id
    }

    pub fn installation_generation(&self) -> u64 {
        self.installation_generation
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn package_content_digest(&self) -> &AppDigest {
        &self.package_content_digest
    }

    pub fn package_lock_digest(&self) -> &AppDigest {
        &self.package_lock_digest
    }

    pub fn grant_revision(&self) -> super::models::AppRevision {
        self.grant_revision
    }

    pub fn grant_authority_digest(&self) -> &AppDigest {
        &self.grant_authority_digest
    }

    pub fn approval_id(&self) -> &AppReference {
        &self.approval_id
    }

    pub fn workflow_id(&self) -> &AppName {
        &self.workflow_id
    }

    pub fn workflow_declaration_digest(&self) -> &AppDigest {
        &self.workflow_declaration_digest
    }

    pub fn action_id(&self) -> &AppName {
        &self.action_id
    }

    pub fn action_declaration_digest(&self) -> &AppDigest {
        &self.action_declaration_digest
    }

    pub fn port_id(&self) -> &AppName {
        &self.port_id
    }

    pub fn reviewed_grant(&self) -> &AppReviewedContributionPortGrant {
        &self.reviewed_grant
    }

    pub fn locked_port_digest(&self) -> &AppDigest {
        &self.locked_port_digest
    }

    pub fn reviewed_grant_digest(&self) -> &AppDigest {
        &self.reviewed_grant_digest
    }

    pub fn destination_binding(&self) -> AppContributionDestinationBinding {
        self.destination_binding
    }

    pub fn workflow_result_digest(&self) -> &AppDigest {
        &self.workflow_result_digest
    }

    pub fn binding_digest(&self) -> &AppDigest {
        &self.binding_digest
    }
}

/// Reopen the uniquely consumed approval for the committed installation and
/// mint the workflow's exact task-local contribution bindings. This is the
/// terminal/recovery integration seam; callers do not reconstruct approval
/// identifiers or install/update generation rules themselves.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn load_consumed_contribution_task_bindings(
    registry: &AppRegistryService,
    authenticated: &AuthenticatedAppScope,
    task_id: &str,
    installation: &AppInstallation,
    workflow_id: &AppName,
    action_id: &AppName,
    package_content_digest: &AppDigest,
    package_lock: &AppPackageLock,
    grant: &AppGrantRevision,
    now: DateTime<Utc>,
) -> Result<Vec<AppContributionTaskBinding>, AppContributionError> {
    installation.validate_app_contract(&AppContractLimits::default())?;
    grant.validate_app_contract(&AppContractLimits::default())?;
    let attempt = registry
        .committed_attempt_for_installation(authenticated, &installation.installation_id, now)
        .await?
        .ok_or_else(|| {
            AppContributionError::Authority(
                "installation has no committed contribution review attempt".to_owned(),
            )
        })?;
    let approval_ref = attempt.approval_ref.as_ref().ok_or_else(|| {
        AppContributionError::Authority(
            "committed contribution review attempt has no approval".to_owned(),
        )
    })?;
    let attempt_installation_matches = match attempt.kind {
        AppLifecycleAttemptKind::InitialInstall => attempt.installation_id.is_none(),
        AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall => {
            attempt.installation_id.as_ref() == Some(&installation.installation_id)
        },
    };
    if attempt.state != AppLifecycleAttemptState::Committed
        || !attempt_installation_matches
        || attempt.candidate_package_revision_ref != installation.package_revision_ref
        || grant.installation_id != installation.installation_id
        || grant.package_revision_ref != installation.package_revision_ref
        || installation.grant_revision != Some(grant.revision)
    {
        return Err(AppContributionError::Authority(
            "committed contribution review no longer matches the installation".to_owned(),
        ));
    }
    let approval = registry
        .installation_approval(authenticated, approval_ref, now)
        .await?
        .ok_or_else(|| {
            AppContributionError::Authority(
                "committed contribution approval is unavailable".to_owned(),
            )
        })?;
    let expected_approval_ref = expected_app_installation_approval_ref(
        &attempt.attempt_id,
        &approval.session_ref,
        &grant.authority_digest,
        &approval.workflow_material_bindings,
    )?;
    let commit_generation = contribution_review_commit_generation(
        attempt.kind,
        attempt.source_installation_generation,
    )?;
    let consumed_revision = AppRevision::new(commit_generation)?;
    // A renewed owner decision has a later approval revision. Its committed
    // consumption and exact authority/material bindings remain mandatory.
    if approval.approval_id != *approval_ref
        || approval.approval_id != expected_approval_ref
        || &approval.authenticated_scope_ref != authenticated.scope_binding_ref()
        || approval.install_or_update_attempt_id != attempt.attempt_id
        || &approval.package_content_digest != package_content_digest
        || approval.granted_authority_digest != grant.authority_digest
        || approval.consumed_at.is_none()
        || approval.consumed_installation_revision != Some(consumed_revision)
        || installation.lifecycle.generation < commit_generation
    {
        return Err(AppContributionError::Authority(
            "consumed contribution approval no longer matches committed review".to_owned(),
        ));
    }
    bind_reviewed_contribution_ports_for_task(
        task_id,
        installation.lifecycle.generation,
        workflow_id,
        action_id,
        package_content_digest,
        package_lock,
        grant,
        &approval,
    )
}

/// Bind every granted port for one workflow to an exact task. Any unknown,
/// missing, substituted, widened, revoked, unconsumed, or stale axis refuses
/// the entire set; callers never receive partially usable authority.
pub(crate) fn bind_reviewed_contribution_ports_for_task(
    task_id: &str,
    installation_generation: u64,
    workflow_id: &AppName,
    action_id: &AppName,
    package_content_digest: &AppDigest,
    package_lock: &AppPackageLock,
    grant: &AppGrantRevision,
    approval: &AppInstallationApproval,
) -> Result<Vec<AppContributionTaskBinding>, AppContributionError> {
    if task_id.is_empty()
        || task_id.len() > 192
        || !task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
        || installation_generation == 0
    {
        return Err(AppContributionError::Authority(
            "task identity or installation generation is invalid".to_owned(),
        ));
    }
    grant.validate_app_contract(&AppContractLimits::default())?;
    approval.validate_app_contract(&AppContractLimits::default())?;
    if grant.revoked_at.is_some()
        || approval.consumed_at.is_none()
        || approval.consumed_installation_revision.is_none()
        || &approval.package_content_digest != package_content_digest
        || approval.granted_authority_digest != grant.authority_digest
        || app_granted_authority_digest(grant, &approval.contribution_grants)?
            != grant.authority_digest
    {
        return Err(AppContributionError::Authority(
            "current grant and consumed contribution approval do not match".to_owned(),
        ));
    }

    for reviewed in &approval.contribution_grants {
        let locked = package_lock
            .contribution_port(&reviewed.workflow_id, &reviewed.port_id)
            .ok_or_else(|| {
                AppContributionError::Authority(format!(
                    "approved contribution port `{}/{}` is absent from the package lock",
                    reviewed.workflow_id, reviewed.port_id
                ))
            })?;
        ensure_reviewed_port_narrows_lock(reviewed, locked)?;
    }

    let mut bindings = Vec::new();
    for reviewed in approval
        .contribution_grants
        .iter()
        .filter(|reviewed| reviewed.workflow_id == *workflow_id)
    {
        let locked = package_lock
            .contribution_port(workflow_id, &reviewed.port_id)
            .ok_or_else(|| {
                AppContributionError::Authority("reviewed contribution lock disappeared".to_owned())
            })?;
        let action_declaration_digest =
            locked.action_declaration_digest(action_id).ok_or_else(|| {
                AppContributionError::Authority(format!(
                    "action `{action_id}` is not locked to contribution workflow `{workflow_id}`"
                ))
            })?;
        let mut binding = AppContributionTaskBinding {
            schema: "magician.app-contribution-task-binding.v1".to_owned(),
            task_id: task_id.to_owned(),
            installation_id: grant.installation_id.clone(),
            installation_generation,
            package_revision_ref: grant.package_revision_ref.clone(),
            package_content_digest: package_content_digest.clone(),
            package_lock_digest: package_lock.lock_digest().clone(),
            grant_revision: grant.revision,
            grant_authority_digest: grant.authority_digest.clone(),
            approval_id: approval.approval_id.clone(),
            workflow_id: workflow_id.clone(),
            workflow_declaration_digest: locked.workflow_declaration_digest().clone(),
            action_id: action_id.clone(),
            action_declaration_digest: action_declaration_digest.clone(),
            port_id: reviewed.port_id.clone(),
            locked_port_digest: locked.binding_digest().clone(),
            reviewed_grant_digest: reviewed.grant_digest.clone(),
            destination_binding: locked.destination_binding(),
            workflow_result_digest: locked.workflow_result_digest().clone(),
            reviewed_grant: reviewed.clone(),
            binding_digest: AppDigest::blake3(b"pending-contribution-task-binding"),
        };
        binding.binding_digest = contribution_task_binding_digest(&binding)?;
        bindings.push(binding);
    }
    Ok(bindings)
}

/// Rebuild and compare a task contribution binding from current canonical
/// authorities. Recovery uses this instead of trusting serialized fields or
/// reinterpreting a prior friendly selector.
#[allow(dead_code)] // Explicit point revalidation remains available beside canonical reload+compare.
pub(crate) fn revalidate_contribution_task_binding(
    binding: &AppContributionTaskBinding,
    package_lock: &AppPackageLock,
    grant: &AppGrantRevision,
    approval: &AppInstallationApproval,
) -> Result<(), AppContributionError> {
    let rebound = bind_reviewed_contribution_ports_for_task(
        binding.task_id(),
        binding.installation_generation(),
        binding.workflow_id(),
        binding.action_id(),
        binding.package_content_digest(),
        package_lock,
        grant,
        approval,
    )?;
    if rebound
        .iter()
        .filter(|candidate| candidate.port_id == binding.port_id)
        .count()
        != 1
        || !rebound.iter().any(|candidate| candidate == binding)
    {
        return Err(AppContributionError::Authority(
            "task contribution binding does not reproduce from canonical authority".to_owned(),
        ));
    }
    Ok(())
}

fn contribution_review_commit_generation(
    kind: AppLifecycleAttemptKind,
    source_installation_generation: Option<u64>,
) -> Result<u64, AppContributionError> {
    match (kind, source_installation_generation) {
        (AppLifecycleAttemptKind::InitialInstall, None) => Ok(2),
        (AppLifecycleAttemptKind::Update, Some(source_generation)) => {
            source_generation.checked_add(2).ok_or_else(|| {
                AppContributionError::Authority("review generation overflow".to_owned())
            })
        },
        (AppLifecycleAttemptKind::Reinstall, Some(source_generation)) => {
            source_generation.checked_add(1).ok_or_else(|| {
                AppContributionError::Authority("review generation overflow".to_owned())
            })
        },
        _ => Err(AppContributionError::Authority(
            "review attempt generation evidence is invalid".to_owned(),
        )),
    }
}

fn ensure_reviewed_port_narrows_lock(
    reviewed: &AppReviewedContributionPortGrant,
    locked: &AppLockedContributionPortBinding,
) -> Result<(), AppContributionError> {
    reviewed.validate()?;
    let requested = locked.declaration();
    let source_narrows = match (&reviewed.source, &requested.source) {
        (
            AppContributionSource::MutationBackedEntityProjection {
                entity: granted_entity,
                selected_fields: granted_fields,
            },
            AppContributionSource::MutationBackedEntityProjection {
                entity: requested_entity,
                selected_fields: requested_fields,
            },
        ) => {
            granted_entity == requested_entity
                && is_nonempty_subset(granted_fields, requested_fields)
        },
    };
    if reviewed.locked_port_digest != *locked.binding_digest()
        || reviewed.destination != requested.destination
        || reviewed.destination_binding != locked.destination_binding()
        || !source_narrows
        || !is_nonempty_subset(&reviewed.purposes, &requested.purposes)
        || !is_nonempty_subset(&reviewed.audiences, &requested.audiences)
        || !is_nonempty_subset(&reviewed.evidence_classes, &requested.evidence_classes)
        || !reviewed.frequency.narrows(requested.frequency)
        || reviewed.maximum_retention_seconds == 0
        || reviewed.maximum_retention_seconds > requested.maximum_retention_seconds
    {
        return Err(AppContributionError::Authority(format!(
            "reviewed contribution port `{}/{}` widens or substitutes its lock",
            reviewed.workflow_id, reviewed.port_id
        )));
    }
    Ok(())
}

fn is_nonempty_subset<T: Ord>(selected: &[T], requested: &[T]) -> bool {
    if selected.is_empty() {
        return false;
    }
    let selected = selected.iter().collect::<std::collections::BTreeSet<_>>();
    let requested = requested.iter().collect::<std::collections::BTreeSet<_>>();
    selected.len() <= requested.len() && selected.is_subset(&requested)
}

fn contribution_task_binding_digest(
    binding: &AppContributionTaskBinding,
) -> Result<AppDigest, AppContributionError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": binding.schema,
        "task_id": binding.task_id,
        "installation_id": binding.installation_id,
        "installation_generation": binding.installation_generation,
        "package_revision_ref": binding.package_revision_ref,
        "package_content_digest": binding.package_content_digest,
        "package_lock_digest": binding.package_lock_digest,
        "grant_revision": binding.grant_revision,
        "grant_authority_digest": binding.grant_authority_digest,
        "approval_id": binding.approval_id,
        "workflow_id": binding.workflow_id,
        "workflow_declaration_digest": binding.workflow_declaration_digest,
        "action_id": binding.action_id,
        "action_declaration_digest": binding.action_declaration_digest,
        "port_id": binding.port_id,
        "locked_port_digest": binding.locked_port_digest,
        "reviewed_grant_digest": binding.reviewed_grant_digest,
        "destination_binding": binding.destination_binding,
        "workflow_result_digest": binding.workflow_result_digest,
        "reviewed_grant": binding.reviewed_grant,
    }))?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedMemoryContribution {
    pub event_id: String,
    pub proposal: AppMemoryCandidateProposalV1,
    pub proposal_json: Vec<u8>,
    pub payload_digest: String,
}

impl PreparedMemoryContribution {
    pub fn new(proposal: AppMemoryCandidateProposalV1) -> Result<Self, AppContributionError> {
        proposal.validate()?;
        if proposal.header.update_policy != AppContributionUpdatePolicy::ReplaceExactSourceHead
            || proposal.header.retraction_policy
                != AppContributionRetractionPolicy::TombstoneOnAnySourceDrift
            || proposal.header.sources.len() != 1
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "V1 memory contribution requires one exact source, replace-exact-source-head, and \
                 tombstone-on-drift"
                    .to_owned(),
            )
            .into());
        }
        let proposal_json = serde_json::to_vec(&proposal)?;
        let payload_digest = content_digest(&proposal_json);
        Ok(Self {
            event_id: deterministic_event_id("proposal", &proposal.proposal_digest),
            proposal,
            proposal_json,
            payload_digest,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedMemoryInvalidation {
    pub event_id: String,
    pub invalidation: AppMemoryInvalidationV1,
    pub invalidation_json: Vec<u8>,
    pub payload_digest: String,
}

impl PreparedMemoryInvalidation {
    pub fn new(invalidation: AppMemoryInvalidationV1) -> Result<Self, AppContributionError> {
        invalidation.validate()?;
        let invalidation_json = serde_json::to_vec(&invalidation)?;
        let payload_digest = content_digest(&invalidation_json);
        Ok(Self {
            event_id: deterministic_event_id("invalidation", &invalidation.invalidation_digest),
            invalidation,
            invalidation_json,
            payload_digest,
        })
    }
}

pub(crate) fn enforce_memory_contribution_quota(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    installation_id: &str,
    incoming_bytes: usize,
    now: &DateTime<Utc>,
    replacing_event_id: Option<&str>,
) -> Result<(), AppContributionError> {
    let now = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let pending_for_installation: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_memory_contribution_outbox
          WHERE installation_id = ?1 AND delivered_at IS NULL
            AND proposal_expires_at > ?2
            AND delivery_state IN ('pending', 'leased', 'dispatching')
            AND (?3 IS NULL OR event_id <> ?3)",
        rusqlite::params![installation_id, now, replacing_event_id],
        |row| row.get(0),
    )?;
    if pending_for_installation >= APP_MEMORY_OUTBOX_MAX_PENDING_PER_INSTALLATION {
        return Err(AppContributionError::Quota("per-installation pending rows"));
    }
    let (rows, bytes): (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(proposal_json)), 0)
           FROM app_memory_contribution_outbox
          WHERE delivered_at IS NULL AND proposal_expires_at > ?1
            AND delivery_state IN ('pending', 'leased', 'dispatching')
            AND (?2 IS NULL OR event_id <> ?2)",
        rusqlite::params![now, replacing_event_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let incoming_bytes = i64::try_from(incoming_bytes)
        .map_err(|_| AppContributionError::Quota("scope byte conversion"))?;
    if rows >= APP_MEMORY_OUTBOX_MAX_ROWS_PER_SCOPE
        || bytes.saturating_add(incoming_bytes) > APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE
    {
        return Err(AppContributionError::Quota("scope rows or bytes"));
    }
    let (head_rows, head_bytes): (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(proposal_json)), 0)
           FROM app_memory_contribution_heads
          WHERE principal=?1 AND workspace=?2",
        rusqlite::params![scope.principal.as_str(), scope.workspace.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let replaced_bytes = replacing_event_id
        .map(|event_id| {
            transaction.query_row(
                "SELECT COALESCE(length(proposal_json), 0)
                   FROM app_memory_contribution_heads WHERE event_id=?1",
                [event_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .transpose()?
        .unwrap_or(0);
    if replacing_event_id.is_none() && head_rows >= APP_MEMORY_OUTBOX_MAX_SOURCE_HEADS_PER_SCOPE {
        return Err(AppContributionError::Quota("distinct source heads"));
    }
    if head_bytes
        .saturating_sub(replaced_bytes)
        .saturating_add(incoming_bytes)
        > APP_MEMORY_OUTBOX_MAX_BYTES_PER_SCOPE
    {
        return Err(AppContributionError::Quota("source-head sealed bytes"));
    }
    Ok(())
}

fn deterministic_event_id(kind: &str, sealed_digest: &str) -> String {
    let digest = content_digest(format!("{kind}\0{sealed_digest}").as_bytes());
    format!("memory-{kind}:{}", digest.trim_start_matches("blake3:"))
}

impl From<rusqlite::Error> for AppContributionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Registry(AppRegistryError::Sqlite(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_contribution_authority_is_move_only_and_not_deserializable() {
        static_assertions::assert_not_impl_any!(
            AppContributionTaskBinding: Clone, std::fmt::Debug, serde::de::DeserializeOwned
        );
    }

    #[test]
    fn event_identity_changes_under_digest_substitution() {
        assert_ne!(
            deterministic_event_id("proposal", &content_digest(b"one")),
            deterministic_event_id("proposal", &content_digest(b"two")),
        );
        assert_ne!(
            deterministic_event_id("proposal", &content_digest(b"one")),
            deterministic_event_id("invalidation", &content_digest(b"one")),
        );
    }
}
