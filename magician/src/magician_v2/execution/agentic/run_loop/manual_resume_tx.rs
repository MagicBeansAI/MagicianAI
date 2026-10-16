//! Durable all-or-none admission for a manual resume of an execution tree.
//!
//! A tree resume spans several independently durable records: execution
//! statuses, retained pause envelopes, and stateless control generations. A
//! process can disappear between any two of those writes. This small sealed
//! roster is the decision record which makes that partial state unambiguous:
//!
//! - [`ManualResumeTransactionPhase::Preparing`] always rolls back to Paused;
//! - [`ManualResumeTransactionPhase::Committed`] always rolls forward every
//!   member of the same roster.
//!
//! Exact-continuation members carry a fresh claim UUID and the revision of the
//! retained source checkpoint. Neither a process identity nor a reused steer
//! UUID is sufficient: a stale heartbeat from the same process must not be
//! able to renew a same-key replacement checkpoint.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::magician_v2::agents::{AgentStorage, FileLockGuard};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::ArtifactV2Error;
use crate::magician_v2::storage::models::WaitingState;

const SCHEMA_VERSION: u8 = 2;
const SEAL_DOMAIN: &str = "magician.manual-resume-tree-transaction.v2";
const MAX_TRANSACTION_BYTES: u64 = 512 * 1024;
const MAX_TRANSACTION_JSON_DEPTH: usize = 32;
const MAX_TRANSACTION_JSON_NODES: usize = 16_384;
const MAX_ROSTER_MEMBERS: usize = 512;
const MAX_ACTIVE_TRANSACTIONS_PER_SCOPE: usize = 32;
const MAX_SCOPE_TRANSACTION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCOPE_ROSTER_MEMBERS: usize = 512;
const MAX_ID_BYTES: usize = 1_024;
const MAX_PAUSE_KEY_BYTES: usize = 8 * 1024;
const MUTATION_LOCK_STRIPES: usize = 64;
const OWNER_LEASE_MS: i64 = 120_000;

fn validate_scope_admission_totals(
    existing_transactions: usize,
    existing_members: usize,
    existing_bytes: u64,
    candidate_members: usize,
    candidate_bytes: u64,
) -> Result<(), String> {
    if existing_transactions
        .checked_add(1)
        .is_none_or(|total| total > MAX_ACTIVE_TRANSACTIONS_PER_SCOPE)
    {
        return Err("manual_resume_transaction_scope_count_exceeded".to_owned());
    }
    if existing_members
        .checked_add(candidate_members)
        .is_none_or(|total| total > MAX_SCOPE_ROSTER_MEMBERS)
    {
        return Err("manual_resume_transaction_scope_members_exceeded".to_owned());
    }
    if existing_bytes
        .checked_add(candidate_bytes)
        .is_none_or(|total| total > MAX_SCOPE_TRANSACTION_BYTES)
    {
        return Err("manual_resume_transaction_scope_bytes_exceeded".to_owned());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManualResumeTransactionPhase {
    /// No member may run. Recovery restores the entire roster to Paused.
    Preparing,
    /// Every member is authorized. Recovery re-admits the complete roster.
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualResumeTransactionMember {
    pub execution_id: String,
    pub resume_state: WaitingState,
    /// Durable status-only generation observed while the member was Paused.
    /// State-only admission must compare this as well as the enum to reject a
    /// later Paused generation (ABA) created for a different reason.
    pub source_status_revision: u64,
    /// Present only for an Executing exact continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_key: Option<String>,
    /// Immutable parked/source segment bound to the retained pause body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_segment: Option<String>,
    /// Immutable successor segment where the resumed worker owns its durable
    /// claim/pin. Foreign boot recovery checks this address, not the retired
    /// parked source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_segment: Option<String>,
    /// Fresh UUID for this member of this resume attempt. It is never a
    /// process id or a stateless steer generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_claim_id: Option<String>,
    /// Stateless control UUID transferred from the retained manual fence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_generation: Option<String>,
    /// The immutable envelope revision observed by the successful claim.
    /// Filled while Preparing and required before commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    /// Set only after the member has an independent durable boot proof: its
    /// state-only status is restored, or its exact successor LoopState exists
    /// (or the execution has already reached a later durable lifecycle state).
    #[serde(default)]
    pub rollforward_durable: bool,
}

impl ManualResumeTransactionMember {
    pub(crate) fn state_only(
        execution_id: String,
        resume_state: WaitingState,
        source_status_revision: u64,
    ) -> Self {
        Self {
            execution_id,
            resume_state,
            source_status_revision,
            pause_key: None,
            source_segment: None,
            resume_segment: None,
            member_claim_id: None,
            control_generation: None,
            source_revision: None,
            rollforward_durable: false,
        }
    }

    pub(crate) fn exact(
        execution_id: String,
        pause_key: String,
        source_segment: String,
        resume_segment: String,
        resume_state: WaitingState,
        source_status_revision: u64,
        control_generation: Uuid,
    ) -> Self {
        Self {
            execution_id,
            resume_state,
            source_status_revision,
            pause_key: Some(pause_key),
            source_segment: Some(source_segment),
            resume_segment: Some(resume_segment),
            member_claim_id: Some(Uuid::new_v4().to_string()),
            control_generation: Some(control_generation.to_string()),
            source_revision: None,
            rollforward_durable: false,
        }
    }

    pub(crate) fn is_exact(&self) -> bool {
        self.pause_key.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualResumeTransaction {
    pub schema_version: u8,
    pub transaction_id: String,
    pub principal: String,
    pub workspace: String,
    pub root_execution_id: String,
    pub roster_digest: String,
    pub phase: ManualResumeTransactionPhase,
    /// One process owns recovery of the whole roster at a time. Per-member
    /// pause leases are subordinate to this lease and must never be acquired
    /// without it.
    pub owner_instance_id: String,
    pub owner_claim_id: String,
    pub owner_lease_expires_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub members: Vec<ManualResumeTransactionMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SealedManualResumeTransaction {
    schema_version: u8,
    hmac_sha256: String,
    payload: ManualResumeTransaction,
}

fn mutation_locks() -> &'static [tokio::sync::Mutex<()>] {
    static LOCKS: OnceLock<Vec<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| {
            (0..MUTATION_LOCK_STRIPES)
                .map(|_| tokio::sync::Mutex::new(()))
                .collect()
        })
        .as_slice()
}

fn catalog_mutation_locks() -> &'static [tokio::sync::Mutex<()>] {
    static LOCKS: OnceLock<Vec<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| {
            (0..MUTATION_LOCK_STRIPES)
                .map(|_| tokio::sync::Mutex::new(()))
                .collect()
        })
        .as_slice()
}

fn execution_admission_process_locks(
) -> &'static std::sync::Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn locally_active_transactions() -> &'static std::sync::Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<std::sync::Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// Process-local half of the whole-roster singleflight. `begin` registers the
/// transaction before publishing its file, and durable removal unregisters it.
/// This closes the window before per-execution controls exist in which a second
/// startup/reconcile pass in this process could otherwise adopt a live
/// Preparing transaction and roll it back.
pub(crate) struct LocalActivityGuard {
    transaction_id: String,
}

impl Drop for LocalActivityGuard {
    fn drop(&mut self) {
        locally_active_transactions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.transaction_id);
    }
}

pub(crate) fn try_claim_local_activity(transaction_id: &str) -> Option<LocalActivityGuard> {
    let mut active = locally_active_transactions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !active.insert(transaction_id.to_owned()) {
        return None;
    }
    Some(LocalActivityGuard {
        transaction_id: transaction_id.to_owned(),
    })
}

fn mutation_lock_index(path: &Path) -> usize {
    let digest = blake3::hash(path.to_string_lossy().as_bytes());
    usize::from(digest.as_bytes()[0]) % MUTATION_LOCK_STRIPES
}

fn process_instance_id() -> &'static str {
    static INSTANCE_ID: OnceLock<String> = OnceLock::new();
    INSTANCE_ID
        .get_or_init(|| Uuid::new_v4().to_string())
        .as_str()
}

fn transaction_dir(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    workspace_layout
        .scope_root(principal, workspace)
        .join("restricted")
        .join("manual_resume_transactions")
}

fn transaction_path(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
) -> PathBuf {
    let identity = blake3::hash(root_execution_id.as_bytes())
        .to_hex()
        .to_string();
    transaction_dir(workspace_layout, principal, workspace).join(format!("{identity}.json"))
}

fn catalog_lock_target(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    transaction_dir(workspace_layout, principal, workspace).join("catalog")
}

fn catalog_epoch_path(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    transaction_dir(workspace_layout, principal, workspace).join("catalog.epoch")
}

async fn rotate_catalog_epoch(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<String, String> {
    let path = catalog_epoch_path(workspace_layout, principal, workspace);
    let epoch = Uuid::new_v4().to_string();
    workspace_layout
        .write_string_atomic_path(&path, &epoch)
        .await
        .map_err(|error| format!("manual_resume_transaction_catalog_epoch_write_failed:{error}"))?;
    Ok(epoch)
}

/// Read the cheap mutation token while holding [`ScopeCatalogExclusion`]. A
/// legacy/missing token is stable until the next begin/remove rotates it.
pub(crate) async fn scope_catalog_epoch(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<String, String> {
    let path = catalog_epoch_path(workspace_layout, principal, workspace);
    match workspace_layout
        .read_to_string_bounded_path(&path, 128)
        .await
    {
        Ok(epoch) if Uuid::parse_str(epoch.trim()).is_ok() => Ok(epoch.trim().to_owned()),
        Ok(_) => Err("manual_resume_transaction_catalog_epoch_invalid".to_owned()),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok("legacy-unversioned-catalog".to_owned())
        },
        Err(error) => Err(format!(
            "manual_resume_transaction_catalog_epoch_read_failed:{error}"
        )),
    }
}

/// Short scope-wide exclusion shared by transaction publication/removal and
/// generic orphan membership refresh. Ordinary dispatch uses the companion
/// per-execution exclusion for its longer admission window.
pub(crate) struct ScopeCatalogExclusion {
    _process: tokio::sync::MutexGuard<'static, ()>,
    _file: FileLockGuard,
}

/// Cross-process exclusion between ordinary orphan admission and transaction
/// preflight/publication for one execution. Tree resume acquires every member
/// in sorted order before inspecting Paused state; generic recovery holds its
/// member through durable dispatch admission.
pub(crate) struct ExecutionAdmissionExclusion {
    _process: tokio::sync::OwnedMutexGuard<()>,
    _file: FileLockGuard,
}

pub(crate) async fn acquire_execution_admission_exclusion(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> Result<ExecutionAdmissionExclusion, String> {
    let identity = blake3::hash(execution_id.as_bytes()).to_hex().to_string();
    let path = transaction_dir(workspace_layout, principal, workspace)
        .join("execution_admission")
        .join(identity);
    let process_lock = {
        let mut locks = execution_admission_process_locks()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&path).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(path.clone(), Arc::downgrade(&lock));
            lock
        }
    };
    let process = process_lock.lock_owned().await;
    let file = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_execution_admission_lock_failed:{error}"))?;
    Ok(ExecutionAdmissionExclusion {
        _process: process,
        _file: file,
    })
}

pub(crate) async fn acquire_scope_catalog_exclusion(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<ScopeCatalogExclusion, String> {
    let catalog_path = catalog_lock_target(workspace_layout, principal, workspace);
    let process = catalog_mutation_locks()[mutation_lock_index(&catalog_path)]
        .lock()
        .await;
    let file = AgentStorage::acquire_file_lock_exclusive(&catalog_path)
        .await
        .map_err(|error| format!("manual_resume_transaction_catalog_lock_failed:{error}"))?;
    Ok(ScopeCatalogExclusion {
        _process: process,
        _file: file,
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

fn exact_fields_are_coherent(member: &ManualResumeTransactionMember) -> bool {
    let present = [
        member.pause_key.is_some(),
        member.source_segment.is_some(),
        member.resume_segment.is_some(),
        member.member_claim_id.is_some(),
        member.control_generation.is_some(),
    ];
    present.iter().all(|value| *value) || present.iter().all(|value| !*value)
}

fn immutable_roster_digest(
    root_execution_id: &str,
    members: &[ManualResumeTransactionMember],
) -> Result<String, String> {
    let roster = members
        .iter()
        .map(|member| {
            serde_json::json!({
                "execution_id": member.execution_id,
                "resume_state": member.resume_state,
                "source_status_revision": member.source_status_revision,
                "pause_key": member.pause_key,
                "source_segment": member.source_segment,
                "resume_segment": member.resume_segment,
                "member_claim_id": member.member_claim_id,
                "control_generation": member.control_generation,
            })
        })
        .collect::<Vec<_>>();
    let canonical = crate::magician_v2::json_traversal::canonical_json_bytes(&serde_json::json!({
        "domain": "magician.manual-resume-tree-roster.v2",
        "root_execution_id": root_execution_id,
        "members": roster,
    }))
    .map_err(|error| format!("manual_resume_roster_encode_failed:{error}"))?;
    Ok(blake3::hash(&canonical).to_hex().to_string())
}

fn validate_payload(payload: &ManualResumeTransaction) -> Result<(), String> {
    if payload.schema_version != SCHEMA_VERSION
        || Uuid::parse_str(&payload.transaction_id).is_err()
        || payload.principal.trim().is_empty()
        || payload.workspace.trim().is_empty()
        || payload.root_execution_id.trim().is_empty()
        || payload.root_execution_id.len() > MAX_ID_BYTES
        || Uuid::parse_str(&payload.owner_instance_id).is_err()
        || Uuid::parse_str(&payload.owner_claim_id).is_err()
        || payload.owner_lease_expires_at_ms <= 0
        || payload.members.is_empty()
        || payload.members.len() > MAX_ROSTER_MEMBERS
        || payload.created_at_ms <= 0
        || payload.updated_at_ms < payload.created_at_ms
    {
        return Err("manual_resume_transaction_header_invalid".to_owned());
    }

    let mut seen = HashSet::with_capacity(payload.members.len());
    let mut previous: Option<&str> = None;
    for member in &payload.members {
        if member.execution_id.trim().is_empty()
            || member.execution_id.len() > MAX_ID_BYTES
            || !seen.insert(member.execution_id.as_str())
            || previous.is_some_and(|previous| previous >= member.execution_id.as_str())
            || !exact_fields_are_coherent(member)
            || member
                .pause_key
                .as_ref()
                .is_some_and(|key| key.is_empty() || key.len() > MAX_PAUSE_KEY_BYTES)
            || member
                .source_segment
                .as_deref()
                .is_some_and(|segment| segment.is_empty() || segment.len() > MAX_ID_BYTES)
            || member
                .resume_segment
                .as_deref()
                .is_some_and(|segment| segment.is_empty() || segment.len() > MAX_ID_BYTES)
            || member.member_claim_id.as_deref().is_some_and(|value| {
                Uuid::parse_str(value).is_err() || value == payload.transaction_id
            })
            || member
                .control_generation
                .as_deref()
                .is_some_and(|value| Uuid::parse_str(value).is_err())
            || member
                .source_revision
                .as_deref()
                .is_some_and(|revision| revision.is_empty() || revision.len() > MAX_ID_BYTES)
            || (!member.is_exact() && member.source_revision.is_some())
            || (member.is_exact() && member.resume_state != WaitingState::Executing)
            || (!member.is_exact()
                && !matches!(
                    member.resume_state,
                    WaitingState::WaitingChildren | WaitingState::WaitingUser
                ))
        {
            return Err("manual_resume_transaction_member_invalid".to_owned());
        }
        previous = Some(member.execution_id.as_str());
    }
    if !seen.contains(payload.root_execution_id.as_str()) {
        return Err("manual_resume_transaction_root_missing".to_owned());
    }
    if payload.phase == ManualResumeTransactionPhase::Committed
        && payload
            .members
            .iter()
            .any(|member| member.is_exact() && member.source_revision.is_none())
    {
        return Err("manual_resume_transaction_unclaimed_commit".to_owned());
    }
    if payload.phase == ManualResumeTransactionPhase::Preparing
        && payload
            .members
            .iter()
            .any(|member| member.rollforward_durable)
    {
        return Err("manual_resume_transaction_precommit_settlement".to_owned());
    }
    let expected_digest = immutable_roster_digest(&payload.root_execution_id, &payload.members)?;
    if !constant_time_eq(expected_digest.as_bytes(), payload.roster_digest.as_bytes()) {
        return Err("manual_resume_transaction_roster_mismatch".to_owned());
    }
    Ok(())
}

fn seal_payload(payload: ManualResumeTransaction) -> Result<SealedManualResumeTransaction, String> {
    validate_payload(&payload)?;
    let bytes = crate::magician_v2::json_traversal::canonical_json_bytes(&serde_json::json!({
        "domain": SEAL_DOMAIN,
        "schema_version": SCHEMA_VERSION,
        "payload": payload,
    }))
    .map_err(|error| format!("manual_resume_transaction_encode_failed:{error}"))?;
    let signer = crate::magician_v2::secrets::app_control_plane_signer()
        .ok_or_else(|| "manual_resume_transaction_signer_unavailable".to_owned())?;
    Ok(SealedManualResumeTransaction {
        schema_version: SCHEMA_VERSION,
        hmac_sha256: signer.fingerprint(SEAL_DOMAIN, &bytes),
        payload,
    })
}

fn verify_opened(
    sealed: SealedManualResumeTransaction,
    principal: &str,
    workspace: &str,
) -> Result<ManualResumeTransaction, String> {
    if sealed.schema_version != SCHEMA_VERSION
        || sealed.payload.principal != principal
        || sealed.payload.workspace != workspace
    {
        return Err("manual_resume_transaction_scope_mismatch".to_owned());
    }
    let expected = seal_payload(sealed.payload.clone())?;
    if !constant_time_eq(
        expected.hmac_sha256.as_bytes(),
        sealed.hmac_sha256.as_bytes(),
    ) {
        return Err("manual_resume_transaction_seal_mismatch".to_owned());
    }
    Ok(sealed.payload)
}

async fn load_path(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    path: &Path,
) -> Result<Option<ManualResumeTransaction>, String> {
    match workspace_layout.metadata_path(path).await {
        Ok(None) => return Ok(None),
        Ok(Some(metadata)) if metadata.is_file() => {},
        Ok(Some(_)) => return Err("manual_resume_transaction_not_regular_file".to_owned()),
        Err(error) => return Err(format!("manual_resume_transaction_metadata_failed:{error}")),
    }
    let sealed = workspace_layout
        .read_json_bounded_stream_path::<SealedManualResumeTransaction, _>(
            path,
            MAX_TRANSACTION_BYTES,
            MAX_TRANSACTION_JSON_DEPTH,
            MAX_TRANSACTION_JSON_NODES,
        )
        .await
        .map_err(|error| format!("manual_resume_transaction_read_failed:{error}"))?;
    verify_opened(sealed, principal, workspace).map(Some)
}

async fn save_path(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
    payload: ManualResumeTransaction,
) -> Result<ManualResumeTransaction, String> {
    let sealed = seal_payload(payload)?;
    let encoded = serde_json::to_vec(&sealed)
        .map_err(|error| format!("manual_resume_transaction_encode_failed:{error}"))?;
    if encoded.len() as u64 > MAX_TRANSACTION_BYTES {
        return Err("manual_resume_transaction_size_exceeded".to_owned());
    }
    if let Some(parent) = path.parent() {
        workspace_layout
            .create_dir_all_path(parent)
            .await
            .map_err(|error| format!("manual_resume_transaction_parent_failed:{error}"))?;
    }
    match workspace_layout
        .write_json_compact_atomic_path(path, &sealed)
        .await
    {
        Ok(()) => Ok(sealed.payload),
        Err(error) => {
            // Atomic replacement can become canonical before the final parent
            // directory sync reports an error. Classify that uncertain outcome
            // while the caller still owns the transaction file lock: returning
            // an error for an already-landed commit could make the caller roll
            // back a durable roll-forward decision.
            match load_path(
                workspace_layout,
                &sealed.payload.principal,
                &sealed.payload.workspace,
                path,
            )
            .await
            {
                Ok(Some(canonical)) if canonical == sealed.payload => {
                    crate::magician_v2::artifact_v2::io::sync_parent_dir(path)
                        .await
                        .map_err(|sync_error| {
                            format!(
                                "manual_resume_transaction_write_uncertain:{error}; parent sync retry failed:{sync_error}"
                            )
                        })?;
                    Ok(canonical)
                },
                _ => Err(format!("manual_resume_transaction_write_failed:{error}")),
            }
        },
    }
}

/// Create the durable rollback decision before claiming any member checkpoint
/// or changing any execution status. One root can have only one active record
/// across all processes.
pub(crate) async fn begin(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    mut members: Vec<ManualResumeTransactionMember>,
) -> Result<(ManualResumeTransaction, LocalActivityGuard), String> {
    members.sort_by(|left, right| left.execution_id.cmp(&right.execution_id));
    let catalog_path = catalog_lock_target(workspace_layout, principal, workspace);
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    // Tree mutexes are process-local and a caller may address a subtree rather
    // than the persisted root. Serialize the complete scope catalog so a
    // parent-root and child-root request cannot publish overlapping rosters in
    // two processes.
    let _catalog_guard = catalog_mutation_locks()[mutation_lock_index(&catalog_path)]
        .lock()
        .await;
    let _catalog_file_guard = AgentStorage::acquire_file_lock_exclusive(&catalog_path)
        .await
        .map_err(|error| format!("manual_resume_transaction_catalog_lock_failed:{error}"))?;
    let candidate_ids = members
        .iter()
        .map(|member| member.execution_id.as_str())
        .collect::<HashSet<_>>();
    let active_transactions =
        list_scope_under_catalog_exclusion(workspace_layout, principal, workspace).await?;
    if active_transactions.iter().any(|active| {
        active
            .members
            .iter()
            .any(|member| candidate_ids.contains(member.execution_id.as_str()))
    }) {
        return Err("manual_resume_transaction_roster_already_active".to_owned());
    }
    let existing_members = active_transactions
        .iter()
        .try_fold(0_usize, |total, active| {
            total
                .checked_add(active.members.len())
                .ok_or_else(|| "manual_resume_transaction_scope_members_exceeded".to_owned())
        })?;
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    if load_path(workspace_layout, principal, workspace, &path)
        .await?
        .is_some()
    {
        return Err("manual_resume_transaction_already_active".to_owned());
    }
    // Rotate before publication while the scope catalog lock is held. Readers
    // that observe the new epoch can only enter after the transaction file is
    // canonical (or after this begin failed, in which case a harmless refresh
    // sees no new member).
    rotate_catalog_epoch(workspace_layout, principal, workspace).await?;
    let now = Utc::now().timestamp_millis().max(1);
    let payload = ManualResumeTransaction {
        schema_version: SCHEMA_VERSION,
        transaction_id: Uuid::new_v4().to_string(),
        principal: principal.to_owned(),
        workspace: workspace.to_owned(),
        root_execution_id: root_execution_id.to_owned(),
        roster_digest: immutable_roster_digest(root_execution_id, &members)?,
        phase: ManualResumeTransactionPhase::Preparing,
        owner_instance_id: process_instance_id().to_owned(),
        owner_claim_id: Uuid::new_v4().to_string(),
        owner_lease_expires_at_ms: now.saturating_add(OWNER_LEASE_MS),
        created_at_ms: now,
        updated_at_ms: now,
        members,
    };
    let existing_bytes = active_transactions
        .iter()
        .try_fold(0_u64, |total, transaction| {
            let sealed = seal_payload(transaction.clone())?;
            let encoded = serde_json::to_vec(&sealed)
                .map_err(|error| format!("manual_resume_transaction_encode_failed:{error}"))?;
            total
                .checked_add(encoded.len() as u64)
                .ok_or_else(|| "manual_resume_transaction_scope_bytes_exceeded".to_owned())
        })?;
    let candidate_bytes = serde_json::to_vec(&seal_payload(payload.clone())?)
        .map_err(|error| format!("manual_resume_transaction_encode_failed:{error}"))?
        .len() as u64;
    validate_scope_admission_totals(
        active_transactions.len(),
        existing_members,
        existing_bytes,
        payload.members.len(),
        candidate_bytes,
    )?;
    locally_active_transactions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(payload.transaction_id.clone());
    let local_activity = LocalActivityGuard {
        transaction_id: payload.transaction_id.clone(),
    };
    match save_path(workspace_layout, &path, payload.clone()).await {
        Ok(saved) => Ok((saved, local_activity)),
        Err(error) => Err(error),
    }
}

fn require_current_owner(
    payload: &ManualResumeTransaction,
    owner_claim_id: &str,
) -> Result<(), String> {
    if payload.owner_instance_id != process_instance_id()
        || payload.owner_claim_id != owner_claim_id
        || Utc::now().timestamp_millis() >= payload.owner_lease_expires_at_ms
    {
        return Err("manual_resume_transaction_owner_changed".to_owned());
    }
    Ok(())
}

/// Acquire or renew the single cross-process owner of an existing roster.
/// `Ok(None)` means an unexpired foreign owner still owns every member; the
/// caller must defer the complete transaction without touching any checkpoint.
pub(crate) async fn claim_for_recovery(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    transaction_id: &str,
) -> Result<Option<ManualResumeTransaction>, String> {
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    let mut payload = load_path(workspace_layout, principal, workspace, &path)
        .await?
        .ok_or_else(|| "manual_resume_transaction_missing".to_owned())?;
    if payload.transaction_id != transaction_id {
        return Err("manual_resume_transaction_generation_changed".to_owned());
    }
    if payload.root_execution_id != root_execution_id {
        return Err("manual_resume_transaction_root_path_mismatch".to_owned());
    }
    let now = Utc::now().timestamp_millis().max(1);
    if payload.owner_instance_id != process_instance_id() && now < payload.owner_lease_expires_at_ms
    {
        return Ok(None);
    }
    if payload.owner_instance_id != process_instance_id() {
        payload.owner_instance_id = process_instance_id().to_owned();
        payload.owner_claim_id = Uuid::new_v4().to_string();
    }
    payload.owner_lease_expires_at_ms = now.saturating_add(OWNER_LEASE_MS);
    payload.updated_at_ms = now.max(payload.created_at_ms);
    save_path(workspace_layout, &path, payload).await.map(Some)
}

pub(crate) async fn renew_owner(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    transaction_id: &str,
    owner_claim_id: &str,
) -> Result<bool, String> {
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    let Some(mut payload) = load_path(workspace_layout, principal, workspace, &path).await? else {
        return Ok(false);
    };
    if payload.transaction_id != transaction_id
        || payload.owner_instance_id != process_instance_id()
        || payload.owner_claim_id != owner_claim_id
    {
        return Ok(false);
    }
    if payload.root_execution_id != root_execution_id {
        return Err("manual_resume_transaction_root_path_mismatch".to_owned());
    }
    let now = Utc::now().timestamp_millis().max(1);
    payload.owner_lease_expires_at_ms = now.saturating_add(OWNER_LEASE_MS);
    payload.updated_at_ms = now.max(payload.created_at_ms);
    save_path(workspace_layout, &path, payload).await?;
    Ok(true)
}

pub(crate) async fn load_for_root(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
) -> Result<Option<ManualResumeTransaction>, String> {
    let loaded = load_path(
        workspace_layout,
        principal,
        workspace,
        &transaction_path(workspace_layout, principal, workspace, root_execution_id),
    )
    .await?;
    if loaded
        .as_ref()
        .is_some_and(|payload| payload.root_execution_id != root_execution_id)
    {
        return Err("manual_resume_transaction_root_path_mismatch".to_owned());
    }
    Ok(loaded)
}

/// Bind the revision returned by the exact pause claim to its already-minted
/// per-attempt claim UUID. This does not change `roster_digest`: that digest is
/// the immutable plan which the checkpoint seal also carries.
pub(crate) async fn record_claimed_revision(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    transaction_id: &str,
    owner_claim_id: &str,
    execution_id: &str,
    source_revision: &str,
) -> Result<ManualResumeTransaction, String> {
    if source_revision.is_empty() || source_revision.len() > MAX_ID_BYTES {
        return Err("manual_resume_transaction_source_revision_invalid".to_owned());
    }
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    let mut payload = load_path(workspace_layout, principal, workspace, &path)
        .await?
        .ok_or_else(|| "manual_resume_transaction_missing".to_owned())?;
    if payload.transaction_id != transaction_id
        || payload.phase != ManualResumeTransactionPhase::Preparing
    {
        return Err("manual_resume_transaction_generation_changed".to_owned());
    }
    if payload.root_execution_id != root_execution_id {
        return Err("manual_resume_transaction_root_path_mismatch".to_owned());
    }
    require_current_owner(&payload, owner_claim_id)?;
    let member = payload
        .members
        .iter_mut()
        .find(|member| member.execution_id == execution_id)
        .ok_or_else(|| "manual_resume_transaction_member_missing".to_owned())?;
    if !member.is_exact()
        || member
            .source_revision
            .as_deref()
            .is_some_and(|revision| revision != source_revision)
    {
        return Err("manual_resume_transaction_claim_mismatch".to_owned());
    }
    member.source_revision = Some(source_revision.to_owned());
    let now = Utc::now().timestamp_millis().max(payload.created_at_ms);
    payload.owner_lease_expires_at_ms = now.saturating_add(OWNER_LEASE_MS);
    payload.updated_at_ms = now;
    save_path(workspace_layout, &path, payload).await
}

/// Publish the one-way roll-forward decision after every exact checkpoint is
/// durably claimed and before any member status or launch gate is released.
pub(crate) async fn commit(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    transaction_id: &str,
    owner_claim_id: &str,
) -> Result<ManualResumeTransaction, String> {
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    let mut payload = load_path(workspace_layout, principal, workspace, &path)
        .await?
        .ok_or_else(|| "manual_resume_transaction_missing".to_owned())?;
    if payload.transaction_id != transaction_id {
        return Err("manual_resume_transaction_generation_changed".to_owned());
    }
    if payload.root_execution_id != root_execution_id {
        return Err("manual_resume_transaction_root_path_mismatch".to_owned());
    }
    require_current_owner(&payload, owner_claim_id)?;
    if payload.phase == ManualResumeTransactionPhase::Committed {
        return Ok(payload);
    }
    if payload
        .members
        .iter()
        .any(|member| member.is_exact() && member.source_revision.is_none())
    {
        return Err("manual_resume_transaction_claims_incomplete".to_owned());
    }
    payload.phase = ManualResumeTransactionPhase::Committed;
    let now = Utc::now().timestamp_millis().max(payload.created_at_ms);
    payload.owner_lease_expires_at_ms = now.saturating_add(OWNER_LEASE_MS);
    payload.updated_at_ms = now;
    save_path(workspace_layout, &path, payload).await
}

pub(crate) async fn mark_member_rollforward_durable(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    transaction_id: &str,
    owner_claim_id: &str,
    execution_id: &str,
) -> Result<ManualResumeTransaction, String> {
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    let mut payload = load_path(workspace_layout, principal, workspace, &path)
        .await?
        .ok_or_else(|| "manual_resume_transaction_missing".to_owned())?;
    if payload.transaction_id != transaction_id
        || payload.root_execution_id != root_execution_id
        || payload.phase != ManualResumeTransactionPhase::Committed
    {
        return Err("manual_resume_transaction_generation_changed".to_owned());
    }
    require_current_owner(&payload, owner_claim_id)?;
    let member = payload
        .members
        .iter_mut()
        .find(|member| member.execution_id == execution_id)
        .ok_or_else(|| "manual_resume_transaction_member_missing".to_owned())?;
    member.rollforward_durable = true;
    let now = Utc::now().timestamp_millis().max(payload.created_at_ms);
    payload.owner_lease_expires_at_ms = now.saturating_add(OWNER_LEASE_MS);
    payload.updated_at_ms = now;
    save_path(workspace_layout, &path, payload).await
}

/// Enumerate bounded active transaction records for one already-authorized
/// scope. A malformed or oversized record fails the complete scope scan; it is
/// never skipped in a way that would let ordinary orphan recovery resume one
/// member outside its roster.
pub(crate) async fn list_scope_under_catalog_exclusion(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<Vec<ManualResumeTransaction>, String> {
    let dir = transaction_dir(workspace_layout, principal, workspace);
    let entries = workspace_layout
        .read_dir_path_or_empty(&dir)
        .await
        .map_err(|error| format!("manual_resume_transaction_list_failed:{error}"))?;
    let mut paths = entries
        .into_iter()
        .filter(|entry| entry.is_file && entry.file_name.ends_with(".json"))
        .map(|entry| dir.join(entry.file_name))
        .collect::<Vec<_>>();
    if paths.len() > MAX_ACTIVE_TRANSACTIONS_PER_SCOPE {
        return Err("manual_resume_transaction_scope_count_exceeded".to_owned());
    }
    paths.sort();
    let mut transactions = Vec::with_capacity(paths.len());
    let mut roots = HashSet::with_capacity(paths.len());
    let mut member_owners = HashSet::new();
    let mut admitted_bytes = 0_u64;
    let mut admitted_members = 0_usize;
    for path in paths {
        let metadata = workspace_layout
            .metadata_path(&path)
            .await
            .map_err(|error| format!("manual_resume_transaction_metadata_failed:{error}"))?
            .ok_or_else(|| "manual_resume_transaction_disappeared".to_owned())?;
        admitted_bytes = admitted_bytes
            .checked_add(metadata.len())
            .ok_or_else(|| "manual_resume_transaction_scope_bytes_exceeded".to_owned())?;
        if admitted_bytes > MAX_SCOPE_TRANSACTION_BYTES {
            return Err("manual_resume_transaction_scope_bytes_exceeded".to_owned());
        }
        let transaction = load_path(workspace_layout, principal, workspace, &path)
            .await?
            .ok_or_else(|| "manual_resume_transaction_disappeared".to_owned())?;
        admitted_members = admitted_members
            .checked_add(transaction.members.len())
            .ok_or_else(|| "manual_resume_transaction_scope_members_exceeded".to_owned())?;
        if admitted_members > MAX_SCOPE_ROSTER_MEMBERS {
            return Err("manual_resume_transaction_scope_members_exceeded".to_owned());
        }
        if path
            != transaction_path(
                workspace_layout,
                principal,
                workspace,
                &transaction.root_execution_id,
            )
            || !roots.insert(transaction.root_execution_id.clone())
            || transaction
                .members
                .iter()
                .any(|member| !member_owners.insert(member.execution_id.clone()))
        {
            return Err("manual_resume_transaction_scope_roster_conflict".to_owned());
        }
        transactions.push(transaction);
    }
    Ok(transactions)
}

/// Catalog-coherent public scan. Callers that already hold
/// [`ScopeCatalogExclusion`] must use
/// [`list_scope_under_catalog_exclusion`] to preserve the non-reentrant lock
/// order.
pub(crate) async fn list_scope(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<Vec<ManualResumeTransaction>, String> {
    let _catalog_exclusion =
        acquire_scope_catalog_exclusion(workspace_layout, principal, workspace).await?;
    list_scope_under_catalog_exclusion(workspace_layout, principal, workspace).await
}

/// Remove only the exact transaction generation the caller settled. A newer
/// attempt for the same root is never deleted by stale cleanup.
pub(crate) async fn remove(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    root_execution_id: &str,
    transaction_id: &str,
    owner_claim_id: &str,
) -> Result<(), String> {
    let catalog_path = catalog_lock_target(workspace_layout, principal, workspace);
    let path = transaction_path(workspace_layout, principal, workspace, root_execution_id);
    let _catalog_guard = catalog_mutation_locks()[mutation_lock_index(&catalog_path)]
        .lock()
        .await;
    let _catalog_file_guard = AgentStorage::acquire_file_lock_exclusive(&catalog_path)
        .await
        .map_err(|error| format!("manual_resume_transaction_catalog_lock_failed:{error}"))?;
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("manual_resume_transaction_lock_failed:{error}"))?;
    let Some(payload) = load_path(workspace_layout, principal, workspace, &path).await? else {
        // A previous unlink may have landed while its parent sync returned an
        // error. Re-sync before reporting the generation durably absent.
        crate::magician_v2::artifact_v2::io::sync_parent_dir(&path)
            .await
            .map_err(|error| format!("manual_resume_transaction_remove_sync_failed:{error}"))?;
        locally_active_transactions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(transaction_id);
        return Ok(());
    };
    if payload.transaction_id != transaction_id {
        return Err("manual_resume_transaction_generation_changed".to_owned());
    }
    if payload.root_execution_id != root_execution_id {
        return Err("manual_resume_transaction_root_path_mismatch".to_owned());
    }
    require_current_owner(&payload, owner_claim_id)?;
    if payload.phase == ManualResumeTransactionPhase::Committed
        && payload
            .members
            .iter()
            .any(|member| !member.rollforward_durable)
    {
        return Err("manual_resume_transaction_members_not_durable".to_owned());
    }
    // Rotate before unlink for the same reason as begin: an epoch refresh may
    // conservatively retain an about-to-disappear member, never miss one.
    rotate_catalog_epoch(workspace_layout, principal, workspace).await?;
    let removed = match workspace_layout.remove_file_path(&path).await {
        Ok(()) => crate::magician_v2::artifact_v2::io::sync_parent_dir(&path)
            .await
            .map_err(|error| format!("manual_resume_transaction_remove_sync_failed:{error}")),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::magician_v2::artifact_v2::io::sync_parent_dir(&path)
                .await
                .map_err(|error| format!("manual_resume_transaction_remove_sync_failed:{error}"))
        },
        Err(error) => Err(format!("manual_resume_transaction_remove_failed:{error}")),
    };
    if removed.is_ok() {
        locally_active_transactions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(transaction_id);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_member(id: &str) -> ManualResumeTransactionMember {
        ManualResumeTransactionMember::state_only(id.to_owned(), WaitingState::WaitingChildren, 1)
    }

    #[test]
    fn immutable_digest_ignores_claimed_revision_but_not_claim_identity() {
        let mut member = ManualResumeTransactionMember::exact(
            "child".to_owned(),
            "pause-key".to_owned(),
            "execution-segment-r4".to_owned(),
            "execution-segment-r5".to_owned(),
            WaitingState::Executing,
            1,
            Uuid::new_v4(),
        );
        let before = immutable_roster_digest("child", &[member.clone()]).unwrap();
        member.source_revision = Some("source-revision".to_owned());
        assert_eq!(
            before,
            immutable_roster_digest("child", &[member.clone()]).unwrap()
        );
        member.member_claim_id = Some(Uuid::new_v4().to_string());
        assert_ne!(before, immutable_roster_digest("child", &[member]).unwrap());
    }

    #[test]
    fn immutable_digest_rejects_a_later_paused_status_generation() {
        let member = state_member("root");
        let before = immutable_roster_digest("root", &[member.clone()]).unwrap();
        let mut later_pause = member;
        later_pause.source_status_revision += 1;
        assert_ne!(
            before,
            immutable_roster_digest("root", &[later_pause]).unwrap()
        );
    }

    #[test]
    fn sealed_transaction_rejects_body_tamper_and_cross_scope_copy() {
        let now = Utc::now().timestamp_millis().max(1);
        let member = state_member("root");
        let transaction = ManualResumeTransaction {
            schema_version: SCHEMA_VERSION,
            transaction_id: Uuid::new_v4().to_string(),
            principal: "principal".to_owned(),
            workspace: "workspace".to_owned(),
            root_execution_id: "root".to_owned(),
            roster_digest: immutable_roster_digest("root", &[member.clone()]).unwrap(),
            phase: ManualResumeTransactionPhase::Preparing,
            owner_instance_id: Uuid::new_v4().to_string(),
            owner_claim_id: Uuid::new_v4().to_string(),
            owner_lease_expires_at_ms: now + OWNER_LEASE_MS,
            created_at_ms: now,
            updated_at_ms: now,
            members: vec![member],
        };
        let sealed = seal_payload(transaction.clone()).expect("seal transaction");
        assert_eq!(
            verify_opened(sealed.clone(), "principal", "workspace").expect("open exact scope"),
            transaction
        );

        let mut tampered = sealed.clone();
        tampered.payload.owner_claim_id = Uuid::new_v4().to_string();
        assert_eq!(
            verify_opened(tampered, "principal", "workspace").unwrap_err(),
            "manual_resume_transaction_seal_mismatch"
        );
        assert_eq!(
            verify_opened(sealed, "principal", "another-workspace").unwrap_err(),
            "manual_resume_transaction_scope_mismatch"
        );
    }

    #[test]
    fn committed_transaction_requires_every_exact_source_revision() {
        let now = Utc::now().timestamp_millis().max(1);
        let member = ManualResumeTransactionMember::exact(
            "root".to_owned(),
            "pause-key".to_owned(),
            "execution-segment-r4".to_owned(),
            "execution-segment-r5".to_owned(),
            WaitingState::Executing,
            1,
            Uuid::new_v4(),
        );
        let transaction = ManualResumeTransaction {
            schema_version: SCHEMA_VERSION,
            transaction_id: Uuid::new_v4().to_string(),
            principal: "principal".to_owned(),
            workspace: "workspace".to_owned(),
            root_execution_id: "root".to_owned(),
            roster_digest: immutable_roster_digest("root", &[member.clone()]).unwrap(),
            phase: ManualResumeTransactionPhase::Committed,
            owner_instance_id: Uuid::new_v4().to_string(),
            owner_claim_id: Uuid::new_v4().to_string(),
            owner_lease_expires_at_ms: now + OWNER_LEASE_MS,
            created_at_ms: now,
            updated_at_ms: now,
            members: vec![member],
        };
        assert_eq!(
            validate_payload(&transaction).unwrap_err(),
            "manual_resume_transaction_unclaimed_commit"
        );
    }

    #[test]
    fn roster_must_be_sorted_unique_and_include_root() {
        let now = Utc::now().timestamp_millis().max(1);
        let members = vec![state_member("root"), state_member("child")];
        let transaction = ManualResumeTransaction {
            schema_version: SCHEMA_VERSION,
            transaction_id: Uuid::new_v4().to_string(),
            principal: "principal".to_owned(),
            workspace: "workspace".to_owned(),
            root_execution_id: "root".to_owned(),
            roster_digest: immutable_roster_digest("root", &members).unwrap(),
            phase: ManualResumeTransactionPhase::Preparing,
            owner_instance_id: Uuid::new_v4().to_string(),
            owner_claim_id: Uuid::new_v4().to_string(),
            owner_lease_expires_at_ms: now + OWNER_LEASE_MS,
            created_at_ms: now,
            updated_at_ms: now,
            members,
        };
        assert_eq!(
            validate_payload(&transaction).unwrap_err(),
            "manual_resume_transaction_member_invalid"
        );
    }

    #[test]
    fn exact_fields_cannot_be_partially_present() {
        let mut member = state_member("root");
        member.pause_key = Some("pause-key".to_owned());
        assert!(!exact_fields_are_coherent(&member));
    }

    #[test]
    fn state_only_roster_rejects_paused_and_terminal_sources() {
        for state in [
            WaitingState::Paused,
            WaitingState::Runnable,
            WaitingState::Completed,
            WaitingState::Failed,
        ] {
            let now = Utc::now().timestamp_millis().max(1);
            let member = ManualResumeTransactionMember::state_only("root".to_owned(), state, 1);
            let transaction = ManualResumeTransaction {
                schema_version: SCHEMA_VERSION,
                transaction_id: Uuid::new_v4().to_string(),
                principal: "principal".to_owned(),
                workspace: "workspace".to_owned(),
                root_execution_id: "root".to_owned(),
                roster_digest: immutable_roster_digest("root", &[member.clone()]).unwrap(),
                phase: ManualResumeTransactionPhase::Preparing,
                owner_instance_id: Uuid::new_v4().to_string(),
                owner_claim_id: Uuid::new_v4().to_string(),
                owner_lease_expires_at_ms: now + OWNER_LEASE_MS,
                created_at_ms: now,
                updated_at_ms: now,
                members: vec![member],
            };
            assert_eq!(
                validate_payload(&transaction).unwrap_err(),
                "manual_resume_transaction_member_invalid"
            );
        }
    }

    #[test]
    fn local_activity_claim_is_singleflight_and_drop_releases_it() {
        let transaction_id = Uuid::new_v4().to_string();
        let guard = try_claim_local_activity(&transaction_id)
            .expect("first activity owner should be admitted");
        assert!(try_claim_local_activity(&transaction_id).is_none());
        drop(guard);
        assert!(try_claim_local_activity(&transaction_id).is_some());
    }

    #[test]
    fn scope_admission_totals_accept_exact_boundaries_and_reject_each_overflow() {
        assert!(validate_scope_admission_totals(
            MAX_ACTIVE_TRANSACTIONS_PER_SCOPE - 1,
            MAX_SCOPE_ROSTER_MEMBERS - 1,
            MAX_SCOPE_TRANSACTION_BYTES - 1,
            1,
            1,
        )
        .is_ok());
        assert_eq!(
            validate_scope_admission_totals(MAX_ACTIVE_TRANSACTIONS_PER_SCOPE, 0, 0, 1, 1,)
                .unwrap_err(),
            "manual_resume_transaction_scope_count_exceeded"
        );
        assert_eq!(
            validate_scope_admission_totals(0, MAX_SCOPE_ROSTER_MEMBERS, 0, 1, 1,).unwrap_err(),
            "manual_resume_transaction_scope_members_exceeded"
        );
        assert_eq!(
            validate_scope_admission_totals(0, 0, MAX_SCOPE_TRANSACTION_BYTES, 1, 1,).unwrap_err(),
            "manual_resume_transaction_scope_bytes_exceeded"
        );
    }

    #[tokio::test]
    async fn catalog_epoch_is_durable_and_rotates() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        assert_eq!(
            scope_catalog_epoch(&workspace_layout, "principal", "workspace")
                .await
                .unwrap(),
            "legacy-unversioned-catalog"
        );
        let first = rotate_catalog_epoch(&workspace_layout, "principal", "workspace")
            .await
            .unwrap();
        assert_eq!(
            scope_catalog_epoch(&workspace_layout, "principal", "workspace")
                .await
                .unwrap(),
            first
        );
        let second = rotate_catalog_epoch(&workspace_layout, "principal", "workspace")
            .await
            .unwrap();
        assert_ne!(first, second);
    }
}
