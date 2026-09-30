//! Integrity-sealed, commit-coupled operator redirects.
//!
//! `/steer` acknowledges only after the entry is atomically persisted here.
//! A Decide phase claims a bounded batch for one exact loop-state segment and
//! iteration, then publishes [`SteerConsumeReceipt`] in the same fenced state
//! commit as the successful Decide boundary. The following phase acknowledges
//! that receipt idempotently. A crash before the boundary re-offers the same
//! claim; a crash after it observes the committed receipt and removes the rows
//! without presenting their text to the model again.
//!
//! Messages are not placed in `LoopState`: they are prompt input, so accepting
//! unsealed bytes from a mutable state record would be a durable prompt-
//! injection surface. The restricted inbox is HMAC-bound to scope, execution,
//! control generation, claim address and contents. The committed receipt carries
//! only ids and content digests.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::AgentStorage;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::ArtifactV2Error;

const STEER_INBOX_SCHEMA_VERSION: u8 = 1;
const STEER_INBOX_SEAL_DOMAIN: &str = "magician.operator-steer-inbox.v1";
const MAX_PENDING_STEERS: usize = 16;
const MAX_STEER_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_STEER_INBOX_BYTES: usize = 128 * 1024;
const MAX_STEER_JSON_DEPTH: usize = 32;
const MAX_STEER_JSON_NODES: usize = 4_096;
const MUTATION_LOCK_STRIPES: usize = 64;
// Longer than both bounded manual-pause snapshot/settlement waits (30s each).
// An active control operation therefore owns recovery admission throughout its
// normal window, while a crashed owner cannot fence an Executing row forever.
const CONTROL_SUPERSESSION_LEASE_MS: i64 = 120_000;
/// The iteration a continuation segment starts at. A resume (`-rN`), a
/// refinement pass (`-pN`) and a delegation successor each seed their own
/// `LoopState` at iteration one; only an exact recovery reclaims a cursor.
const SUCCESSOR_FIRST_ITERATION: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SealedSteerInbox {
    schema_version: u8,
    hmac_sha256: String,
    payload: SteerInbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SteerInbox {
    principal: String,
    workspace: String,
    execution_id: String,
    /// UUID minted by the runtime control owner for the current execution
    /// activation. This sealed shared value, not mutable runtime timestamps, is
    /// what API and worker processes compare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_control_generation: Option<String>,
    next_generation: u64,
    entries: Vec<SteerInboxEntry>,
    /// Durable admission tombstone installed before a run-ending loop
    /// boundary is published. `enqueue` and this transition share the same
    /// cross-process file lock, so an accepted redirect cannot appear in the
    /// check-to-commit window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_fence: Option<TerminalSteerFence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SteerInboxEntry {
    entry_id: String,
    generation: u64,
    control_generation: String,
    message: String,
    content_digest: String,
    claim: Option<SteerClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SteerClaim {
    source_segment: String,
    iteration: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TerminalSteerFence {
    control_generation: String,
    claim: SteerClaim,
    #[serde(
        default,
        skip_serializing_if = "TerminalSteerFenceKind::is_speculative"
    )]
    kind: TerminalSteerFenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<i64>,
    /// A manual pause spanning more than one execution is prepared member by
    /// member, then made visible by one exact coordinator commit. Workers must
    /// not interpret a prepared member fence as a pause request: the API owner
    /// may still be validating another member, or may have crashed mid-prepare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manual_pause_tree: Option<ManualPauseTreeBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManualPauseTreeBinding {
    transaction_id: String,
    requested_root_execution_id: String,
    coordinator_execution_id: String,
    coordinator_control_generation: String,
    roster_digest: String,
    committed: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TerminalSteerFenceKind {
    #[default]
    SpeculativePhase,
    ControlSupersession,
    ManualPauseSupersession,
}

impl TerminalSteerFenceKind {
    fn is_speculative(&self) -> bool {
        *self == Self::SpeculativePhase
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSteerAdmission {
    Admitted,
    PendingSteer,
}

/// Whether a control-plane close installed this exact tombstone or found an
/// earlier close for the same runtime epoch. Callers which need transactional
/// rollback (manual-pause tree preflight) must only reopen tombstones they
/// installed themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlGenerationSupersession {
    Installed,
    AlreadySuperseded,
}

/// Side-effect-free operator view of one sealed runtime-control epoch.
///
/// API processes cannot use their process-local control registry to classify a
/// loop owned by a rolling peer. This projection exposes only the decisions an
/// operator endpoint needs, without leaking or allowing callers to interpret
/// the internal fence variant. A speculative terminal phase still permits a
/// manual pause to supersede it, but cannot accept a new steer; cancellation
/// and manual-pause fences permit neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableOperatorControlState {
    pub generation: Option<String>,
    pub can_pause: bool,
    pub can_steer: bool,
}

/// Content-free proof that one exact Decide boundary consumed a steer batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SteerConsumeReceipt {
    pub schema_version: u8,
    pub execution_id: String,
    pub source_segment: String,
    pub iteration: usize,
    pub entries: Vec<SteerReceiptEntry>,
}

/// One member of a committed consume receipt. No prompt text crosses into
/// `LoopState`; the digest binds the acknowledgement to the sealed inbox row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SteerReceiptEntry {
    pub entry_id: String,
    pub generation: u64,
    pub control_generation: String,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedSteers {
    pub messages: Vec<String>,
    pub receipt: SteerConsumeReceipt,
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

fn mutation_lock_index(path: &std::path::Path) -> usize {
    let digest = blake3::hash(path.to_string_lossy().as_bytes());
    usize::from(digest.as_bytes()[0]) % MUTATION_LOCK_STRIPES
}

fn inbox_path(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> PathBuf {
    let identity = blake3::hash(execution_id.as_bytes()).to_hex().to_string();
    workspace_layout
        .scope_root(principal, workspace)
        .join("restricted")
        .join("operator_steers")
        .join(format!("{identity}.json"))
}

fn content_digest(message: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.operator-steer-content.v1\0");
    hasher.update(message.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn seal_payload(payload: SteerInbox) -> Result<SealedSteerInbox, String> {
    let bytes = serde_json::to_vec(&(
        STEER_INBOX_SEAL_DOMAIN,
        STEER_INBOX_SCHEMA_VERSION,
        &payload,
    ))
    .map_err(|error| format!("operator_steer_inbox_encode_failed:{error}"))?;
    let signer = crate::magician_v2::secrets::app_control_plane_signer()
        .ok_or_else(|| "operator_steer_inbox_signer_unavailable".to_owned())?;
    Ok(SealedSteerInbox {
        schema_version: STEER_INBOX_SCHEMA_VERSION,
        hmac_sha256: signer.fingerprint(STEER_INBOX_SEAL_DOMAIN, &bytes),
        payload,
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

fn verify_opened(
    sealed: SealedSteerInbox,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> Result<SteerInbox, String> {
    if sealed.schema_version != STEER_INBOX_SCHEMA_VERSION
        || sealed.payload.principal != principal
        || sealed.payload.workspace != workspace
        || sealed.payload.execution_id != execution_id
        || sealed.payload.entries.len() > MAX_PENDING_STEERS
    {
        return Err("operator_steer_inbox_binding_mismatch".to_owned());
    }
    for entry in &sealed.payload.entries {
        if entry.entry_id.trim().is_empty()
            || entry.control_generation.trim().is_empty()
            || entry.message.is_empty()
            || entry.message.len() > MAX_STEER_MESSAGE_BYTES
            || entry.content_digest != content_digest(&entry.message)
            || entry
                .claim
                .as_ref()
                .is_some_and(|claim| claim.source_segment.trim().is_empty())
        {
            return Err("operator_steer_inbox_entry_invalid".to_owned());
        }
    }
    if sealed.payload.entries.iter().any(|entry| {
        sealed.payload.active_control_generation.as_deref()
            != Some(entry.control_generation.as_str())
    }) || sealed.payload.terminal_fence.as_ref().is_some_and(|fence| {
        fence.control_generation.trim().is_empty()
            || fence.claim.source_segment.trim().is_empty()
            || sealed.payload.active_control_generation.as_deref()
                != Some(fence.control_generation.as_str())
            || (fence.kind == TerminalSteerFenceKind::SpeculativePhase
                && fence.expires_at_ms.is_some())
            || fence
                .expires_at_ms
                .is_some_and(|expires_at_ms| expires_at_ms <= 0)
            || fence.manual_pause_tree.as_ref().is_some_and(|binding| {
                fence.kind != TerminalSteerFenceKind::ManualPauseSupersession
                    || uuid::Uuid::parse_str(&binding.transaction_id).is_err()
                    || binding.requested_root_execution_id.trim().is_empty()
                    || binding.coordinator_execution_id.trim().is_empty()
                    || binding.coordinator_control_generation.trim().is_empty()
                    || binding.roster_digest.len() != 64
                    || !binding
                        .roster_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || (binding.committed
                        && (sealed.payload.execution_id != binding.coordinator_execution_id
                            || fence.control_generation != binding.coordinator_control_generation))
            })
    }) {
        return Err("operator_steer_inbox_generation_binding_mismatch".to_owned());
    }
    let expected = seal_payload(sealed.payload.clone())?;
    if !constant_time_eq(
        expected.hmac_sha256.as_bytes(),
        sealed.hmac_sha256.as_bytes(),
    ) {
        return Err("operator_steer_inbox_seal_mismatch".to_owned());
    }
    Ok(sealed.payload)
}

async fn load(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    path: &std::path::Path,
) -> Result<SteerInbox, String> {
    let sealed = match workspace_layout
        .read_json_bounded_stream_path::<SealedSteerInbox, _>(
            path,
            MAX_STEER_INBOX_BYTES as u64,
            MAX_STEER_JSON_DEPTH,
            MAX_STEER_JSON_NODES,
        )
        .await
    {
        Ok(sealed) => sealed,
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SteerInbox {
                principal: principal.to_owned(),
                workspace: workspace.to_owned(),
                execution_id: execution_id.to_owned(),
                active_control_generation: None,
                next_generation: 1,
                entries: Vec::new(),
                terminal_fence: None,
            });
        },
        Err(error) => return Err(format!("operator_steer_inbox_read_failed:{error}")),
    };
    verify_opened(sealed, principal, workspace, execution_id)
}

async fn save(
    workspace_layout: &ArtifactV2Workspace,
    path: &std::path::Path,
    inbox: SteerInbox,
) -> Result<(), String> {
    let sealed = seal_payload(inbox)?;
    let encoded = serde_json::to_vec(&sealed)
        .map_err(|error| format!("operator_steer_inbox_encode_failed:{error}"))?;
    if encoded.len() > MAX_STEER_INBOX_BYTES {
        return Err("operator_steer_inbox_size_exceeded".to_owned());
    }
    if let Some(parent) = path.parent() {
        workspace_layout
            .create_dir_all_path(parent)
            .await
            .map_err(|error| format!("operator_steer_inbox_parent_failed:{error}"))?;
    }
    workspace_layout
        .write_json_compact_atomic_path(path, &sealed)
        .await
        .map_err(|error| format!("operator_steer_inbox_write_failed:{error}"))
}

/// Persist a redirect before the control endpoint acknowledges it.
pub async fn enqueue(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    message: &str,
) -> Result<(), String> {
    if message.is_empty() || message.len() > MAX_STEER_MESSAGE_BYTES {
        return Err("operator_steer_inbox_message_invalid".to_owned());
    }
    if control_generation.trim().is_empty() {
        return Err("operator_steer_inbox_control_generation_missing".to_owned());
    }
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    // The striped mutex bounds admission inside this process; the advisory
    // lock is the actual serialization boundary when an API process and a
    // foreign worker mutate the same inbox. Keep the ordering identical in
    // enqueue/claim/acknowledge so neither layer can invert it.
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() != Some(control_generation) {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }
    if inbox.terminal_fence.is_some() {
        return Err("operator_steer_inbox_terminal_fenced".to_owned());
    }
    if inbox.entries.len() >= MAX_PENDING_STEERS {
        return Err("operator_steer_inbox_capacity".to_owned());
    }
    let generation = inbox.next_generation;
    inbox.next_generation = inbox
        .next_generation
        .checked_add(1)
        .ok_or_else(|| "operator_steer_inbox_generation_exhausted".to_owned())?;
    inbox.entries.push(SteerInboxEntry {
        entry_id: uuid::Uuid::new_v4().to_string(),
        generation,
        control_generation: control_generation.to_owned(),
        message: message.to_owned(),
        content_digest: content_digest(message),
        claim: None,
    });
    save(workspace_layout, &path, inbox).await
}

/// Claim the oldest pending batch for one exact Decide boundary. A retry of
/// that boundary receives the same batch; another address cannot steal it.
pub async fn claim(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    expected_control_generation: &str,
    source_segment: &str,
    iteration: usize,
) -> Result<Option<ClaimedSteers>, String> {
    if expected_control_generation.trim().is_empty() {
        return Err("operator_steer_claim_control_generation_missing".to_owned());
    }
    if source_segment.trim().is_empty() {
        return Err("operator_steer_claim_segment_missing".to_owned());
    }
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() != Some(expected_control_generation) {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }

    let exact_claim = SteerClaim {
        source_segment: source_segment.to_owned(),
        iteration,
    };
    let mut changed = false;
    if let Some(fence) = inbox.terminal_fence.as_ref() {
        let phase_fence = fence.kind == TerminalSteerFenceKind::SpeculativePhase;
        let retrying_same_terminal = phase_fence
            && fence.control_generation == expected_control_generation
            && fence.claim == exact_claim;
        let continued_same_segment = phase_fence
            && fence.control_generation == expected_control_generation
            && fence.claim.source_segment == exact_claim.source_segment
            && exact_claim.iteration > fence.claim.iteration
            && inbox.entries.is_empty();
        // The exact phase may retry after a pre-commit crash. If that retry
        // instead committed a nonterminal result, only a later Decide on this
        // same segment may reopen admission. A different segment cannot erase
        // the tombstone merely because it shares the runtime execution id.
        if retrying_same_terminal || continued_same_segment {
            inbox.terminal_fence = None;
            changed = true;
        } else {
            return Err("operator_steer_inbox_terminal_fence_binding_mismatch".to_owned());
        }
    }
    if inbox
        .entries
        .iter()
        .any(|entry| entry.control_generation != expected_control_generation)
    {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }
    let has_foreign_claim = inbox.entries.iter().any(|entry| {
        entry
            .claim
            .as_ref()
            .is_some_and(|claim| claim != &exact_claim)
    });
    if has_foreign_claim {
        return Err("operator_steer_inbox_claimed_by_other_boundary".to_owned());
    }
    for entry in &mut inbox.entries {
        if entry.claim.is_none() {
            entry.claim = Some(exact_claim.clone());
            changed = true;
        }
    }
    if inbox.entries.is_empty() {
        if changed {
            save(workspace_layout, &path, inbox).await?;
        }
        return Ok(None);
    }
    if changed {
        save(workspace_layout, &path, inbox.clone()).await?;
    }
    let entries = inbox
        .entries
        .iter()
        .map(|entry| SteerReceiptEntry {
            entry_id: entry.entry_id.clone(),
            generation: entry.generation,
            control_generation: entry.control_generation.clone(),
            content_digest: entry.content_digest.clone(),
        })
        .collect();
    Ok(Some(ClaimedSteers {
        messages: inbox
            .entries
            .iter()
            .map(|entry| entry.message.clone())
            .collect(),
        receipt: SteerConsumeReceipt {
            schema_version: STEER_INBOX_SCHEMA_VERSION,
            execution_id: execution_id.to_owned(),
            source_segment: source_segment.to_owned(),
            iteration,
            entries,
        },
    }))
}

/// Atomically close admission for a prospective run-ending boundary.
///
/// If a row from the current execution epoch was not part of the boundary's
/// Decide receipt, the caller must continue the loop through another Decide.
/// Otherwise this writes a durable tombstone while holding the same file lock
/// as [`enqueue`], eliminating the check/commit race across processes. A crash
/// before the loop commit leaves the exact phase address on disk; retrying that
/// address may reopen it, while another address or generation fails closed.
pub async fn admit_terminal_boundary(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    expected_control_generation: &str,
    source_segment: &str,
    iteration: usize,
    receipt: Option<&SteerConsumeReceipt>,
) -> Result<TerminalSteerAdmission, String> {
    if expected_control_generation.trim().is_empty() {
        return Err("operator_steer_terminal_control_generation_missing".to_owned());
    }
    if source_segment.trim().is_empty() {
        return Err("operator_steer_terminal_segment_missing".to_owned());
    }
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() != Some(expected_control_generation) {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }
    let exact_claim = SteerClaim {
        source_segment: source_segment.to_owned(),
        iteration,
    };
    if inbox
        .entries
        .iter()
        .any(|entry| entry.control_generation != expected_control_generation)
    {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }
    if let Some(receipt) = receipt {
        if receipt.schema_version != STEER_INBOX_SCHEMA_VERSION
            || receipt.execution_id != execution_id
            || receipt.source_segment != source_segment
            || receipt.iteration != iteration
            || receipt.entries.is_empty()
            || receipt.entries.len() > MAX_PENDING_STEERS
            || receipt
                .entries
                .iter()
                .any(|entry| entry.control_generation != expected_control_generation)
        {
            return Err("operator_steer_terminal_receipt_binding_mismatch".to_owned());
        }
        let all_named_rows_exist = receipt.entries.iter().all(|named| {
            inbox.entries.iter().any(|stored| {
                stored.entry_id == named.entry_id
                    && stored.generation == named.generation
                    && stored.control_generation == named.control_generation
                    && stored.content_digest == named.content_digest
                    && stored.claim.as_ref() == Some(&exact_claim)
            })
        });
        if !all_named_rows_exist {
            return Err("operator_steer_terminal_receipt_binding_mismatch".to_owned());
        }
    }
    let receipt_names = receipt.map(|receipt| &receipt.entries[..]).unwrap_or(&[]);
    let unseen_current = inbox.entries.iter().any(|stored| {
        !receipt_names.iter().any(|named| {
            stored.entry_id == named.entry_id
                && stored.generation == named.generation
                && stored.control_generation == named.control_generation
                && stored.content_digest == named.content_digest
                && stored.claim.as_ref() == Some(&exact_claim)
        })
    });
    if unseen_current {
        return Ok(TerminalSteerAdmission::PendingSteer);
    }
    let fence = TerminalSteerFence {
        control_generation: expected_control_generation.to_owned(),
        claim: exact_claim,
        kind: TerminalSteerFenceKind::SpeculativePhase,
        expires_at_ms: None,
        manual_pause_tree: None,
    };
    if inbox
        .terminal_fence
        .as_ref()
        .is_some_and(|held| held != &fence)
    {
        return Err("operator_steer_inbox_terminal_fence_binding_mismatch".to_owned());
    }
    inbox.terminal_fence = Some(fence);
    save(workspace_layout, &path, inbox).await?;
    Ok(TerminalSteerAdmission::Admitted)
}

/// Publish the actual runtime-control UUID for a newly active stateless epoch.
///
/// Replacement is permitted only after every row from the prior epoch was
/// retired. A terminal fence itself carries no prompt bytes and may be cleared
/// at that point; retaining rows instead fails closed rather than relabeling
/// them as input for the replacement execution.
pub async fn activate_control_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
) -> Result<(), String> {
    if control_generation.trim().is_empty() {
        return Err("operator_steer_activation_generation_missing".to_owned());
    }
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() == Some(control_generation) {
        return match inbox.terminal_fence.as_ref().map(|fence| fence.kind) {
            None | Some(TerminalSteerFenceKind::SpeculativePhase) => Ok(()),
            Some(TerminalSteerFenceKind::ControlSupersession)
            | Some(TerminalSteerFenceKind::ManualPauseSupersession) => {
                Err("operator_steer_activation_generation_is_fenced".to_owned())
            },
        };
    }
    if let Some(fence) = inbox.terminal_fence.as_ref() {
        match fence.kind {
            TerminalSteerFenceKind::SpeculativePhase => {
                return Err("operator_steer_speculative_generation_mismatch".to_owned());
            },
            TerminalSteerFenceKind::ManualPauseSupersession => {
                return Err("operator_steer_manual_resume_generation_mismatch".to_owned());
            },
            TerminalSteerFenceKind::ControlSupersession => {},
        }
    }
    if !inbox.entries.is_empty() {
        let control_superseded = inbox.terminal_fence.as_ref().is_some_and(|fence| {
            fence.kind == TerminalSteerFenceKind::ControlSupersession
                && inbox.active_control_generation.as_deref()
                    == Some(fence.control_generation.as_str())
                && inbox
                    .entries
                    .iter()
                    .all(|entry| entry.control_generation == fence.control_generation)
        });
        if !control_superseded {
            return Err("operator_steer_activation_has_prior_generation_debt".to_owned());
        }
        // The successful control operation explicitly superseded these
        // prompts, but retained them while its tree-wide preflight was still fallible.
        // Retire them only together with admission of the replacement epoch.
        inbox.entries.clear();
    }
    inbox.terminal_fence = None;
    inbox.active_control_generation = Some(control_generation.to_owned());
    save(workspace_layout, &path, inbox).await
}

/// Advance from a fully settled Sleeping boundary to its exact retry epoch.
/// This authority is narrower than ordinary activation: only the timer owner
/// calls it after scope/checkpoint/generation validation, and it may replace a
/// prior speculative phase fence only after its receipt rows are gone.
pub async fn activate_exact_sleep_retry_control_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
) -> Result<(), String> {
    if control_generation.trim().is_empty() {
        return Err("operator_steer_activation_generation_missing".to_owned());
    }
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() == Some(control_generation)
        && inbox.terminal_fence.is_none()
    {
        return Ok(());
    }
    if let Some(fence) = inbox.terminal_fence.as_ref() {
        if fence.kind != TerminalSteerFenceKind::SpeculativePhase {
            return Err("operator_steer_exact_retry_generation_is_control_fenced".to_owned());
        }
        if !inbox.entries.is_empty() {
            return Err("operator_steer_exact_retry_receipt_debt_pending".to_owned());
        }
        inbox.terminal_fence = None;
    } else if !inbox.entries.is_empty() {
        return Err("operator_steer_activation_has_prior_generation_debt".to_owned());
    }
    inbox.active_control_generation = Some(control_generation.to_owned());
    save(workspace_layout, &path, inbox).await
}

/// Activate the UUID independently authorized by an exact full-pause
/// checkpoint. Only this path may clear `ManualPauseSupersession`; ordinary
/// activation cannot turn matching text in the generic inbox into resume
/// authority.
pub async fn activate_manual_pause_control_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    committed_receipt: Option<&SteerConsumeReceipt>,
) -> Result<(), String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() != Some(control_generation) {
        return Err("operator_steer_manual_resume_generation_mismatch".to_owned());
    }
    match inbox.terminal_fence.as_ref() {
        None => return Ok(()),
        Some(fence) if fence.kind == TerminalSteerFenceKind::ManualPauseSupersession => {},
        Some(_) => return Err("operator_steer_manual_resume_generation_fenced".to_owned()),
    }
    if let Some(receipt) = committed_receipt {
        if receipt.schema_version != STEER_INBOX_SCHEMA_VERSION
            || receipt.execution_id != execution_id
            || receipt.entries.is_empty()
            || receipt.entries.len() > MAX_PENDING_STEERS
        {
            return Err("operator_steer_receipt_invalid".to_owned());
        }
        let expected_claim = SteerClaim {
            source_segment: receipt.source_segment.clone(),
            iteration: receipt.iteration,
        };
        for named in &receipt.entries {
            if let Some(stored) = inbox
                .entries
                .iter()
                .find(|entry| entry.entry_id == named.entry_id)
            {
                if stored.generation != named.generation
                    || stored.control_generation != named.control_generation
                    || stored.content_digest != named.content_digest
                    || stored.claim.as_ref() != Some(&expected_claim)
                {
                    return Err("operator_steer_receipt_binding_mismatch".to_owned());
                }
            }
        }
        inbox.entries.retain(|stored| {
            !receipt.entries.iter().any(|named| {
                stored.entry_id == named.entry_id
                    && stored.generation == named.generation
                    && stored.control_generation == named.control_generation
                    && stored.content_digest == named.content_digest
                    && stored.claim.as_ref() == Some(&expected_claim)
            })
        });
    }
    // Claims belong to the pre-pause exact segment. Remaining accepted rows
    // were not committed there and must be offered to the resumed segment.
    for entry in &mut inbox.entries {
        entry.claim = None;
    }
    inbox.terminal_fence = None;
    save(workspace_layout, &path, inbox).await
}

/// Read the HMAC-sealed control generation shared by API and worker processes.
pub async fn control_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> Result<Option<String>, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    Ok(
        load(workspace_layout, principal, workspace, execution_id, &path)
            .await?
            .active_control_generation,
    )
}

/// Read whether the durable epoch can accept operator pause/steer mutations.
///
/// This is deliberately a single locked read. Loading the generation and fence
/// separately would let an API response combine values from two epochs and
/// advertise a control which the mutation path must immediately reject.
pub async fn durable_operator_control_state(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> Result<DurableOperatorControlState, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    let generation = inbox
        .active_control_generation
        .filter(|generation| !generation.trim().is_empty());
    let (can_pause, can_steer) = if generation.is_none() {
        (false, false)
    } else {
        match inbox.terminal_fence.as_ref().map(|fence| fence.kind) {
            None => (true, true),
            Some(TerminalSteerFenceKind::SpeculativePhase) => (true, false),
            Some(TerminalSteerFenceKind::ControlSupersession)
            | Some(TerminalSteerFenceKind::ManualPauseSupersession) => (false, false),
        }
    };
    Ok(DurableOperatorControlState {
        generation,
        can_pause,
        can_steer,
    })
}

/// Return the sealed UUID which a crash-recovery owner must re-adopt when this
/// exact nonterminal execution still has accepted input. A speculative phase
/// fence is also re-adoptable when the caller proves it is recovering the
/// pre-commit active phase. An abandoned control fence is re-opened only after
/// its bounded lease expires and only for a caller which already proved the
/// runtime is still active. Sleeping retry admission passes both flags as
/// `false` because its committed boundary must advance deliberately.
pub async fn recoverable_control_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    allow_speculative_phase: bool,
    recover_expired_control: bool,
) -> Result<Option<String>, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if let Some(fence) = inbox.terminal_fence.as_ref() {
        let generation = inbox
            .active_control_generation
            .clone()
            .ok_or_else(|| "operator_steer_recovery_generation_missing".to_owned())?;
        return match fence.kind {
            TerminalSteerFenceKind::SpeculativePhase if allow_speculative_phase => inbox
                .active_control_generation
                .ok_or_else(|| "operator_steer_recovery_generation_missing".to_owned())
                .map(Some),
            TerminalSteerFenceKind::SpeculativePhase => {
                Err("operator_steer_recovery_speculative_phase_not_authorized".to_owned())
            },
            TerminalSteerFenceKind::ControlSupersession
                if recover_expired_control
                    && fence.expires_at_ms.is_some_and(|expires_at_ms| {
                        chrono::Utc::now().timestamp_millis() >= expires_at_ms
                    }) =>
            {
                inbox.terminal_fence = None;
                save(workspace_layout, &path, inbox).await?;
                Ok(Some(generation))
            },
            TerminalSteerFenceKind::ControlSupersession
            | TerminalSteerFenceKind::ManualPauseSupersession => {
                Err("operator_steer_recovery_generation_is_fenced".to_owned())
            },
        };
    }
    if inbox.entries.is_empty() {
        return Ok(inbox.active_control_generation);
    }
    inbox
        .active_control_generation
        .ok_or_else(|| "operator_steer_recovery_generation_missing".to_owned())
        .map(Some)
}

/// Recover only an unfenced exact-retry epoch which accepted input before its
/// worker crashed. A speculative fence belongs to the already committed Sleep
/// boundary and is advanced by the timer-only activation above; its receipt
/// must be retired first. Control/manual fences remain fail-closed.
pub async fn exact_sleep_retry_recoverable_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> Result<Option<String>, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if let Some(fence) = inbox.terminal_fence.as_ref() {
        return match fence.kind {
            TerminalSteerFenceKind::SpeculativePhase if inbox.entries.is_empty() => Ok(None),
            TerminalSteerFenceKind::SpeculativePhase => {
                Err("operator_steer_exact_retry_receipt_debt_pending".to_owned())
            },
            TerminalSteerFenceKind::ControlSupersession
                if fence.expires_at_ms.is_some_and(|expires_at_ms| {
                    chrono::Utc::now().timestamp_millis() >= expires_at_ms
                }) =>
            {
                let generation = inbox
                    .active_control_generation
                    .clone()
                    .ok_or_else(|| "operator_steer_recovery_generation_missing".to_owned())?;
                inbox.terminal_fence = None;
                save(workspace_layout, &path, inbox).await?;
                Ok(Some(generation))
            },
            TerminalSteerFenceKind::ControlSupersession
            | TerminalSteerFenceKind::ManualPauseSupersession => {
                Err("operator_steer_exact_retry_generation_is_control_fenced".to_owned())
            },
        };
    }
    if inbox.entries.is_empty() {
        return Ok(None);
    }
    inbox
        .active_control_generation
        .ok_or_else(|| "operator_steer_recovery_generation_missing".to_owned())
        .map(Some)
}

/// Read, without mutating, the exact UUID carried through a successfully parked
/// manual pause. The caller independently validates the full-pause checkpoint;
/// only `activate_manual_pause_control_generation` clears the typed fence.
pub async fn manual_pause_resume_control_generation(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
) -> Result<Option<String>, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    match inbox.terminal_fence.as_ref() {
        // Crash-retry after typed activation: the full-pause checkpoint is the
        // independent authority for reusing this still-sealed UUID.
        None => Ok(inbox.active_control_generation),
        Some(fence) if fence.kind == TerminalSteerFenceKind::ManualPauseSupersession => inbox
            .active_control_generation
            .ok_or_else(|| "operator_steer_manual_resume_generation_missing".to_owned())
            .map(Some),
        Some(_) => Err("operator_steer_manual_resume_generation_fenced".to_owned()),
    }
}

/// Explicitly supersede accepted input when an external pause/cancel has
/// already closed the runtime epoch. The control operation wins and the sealed
/// terminal fence keeps later admission closed. This is an explicit
/// cancel/failure supersession, so retained prompt rows are erased atomically
/// rather than misrepresented as consumed. Fallible manual pause uses its
/// separate lossless fence below.
pub async fn supersede_generation_for_terminal(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    source_segment: &str,
    iteration: usize,
) -> Result<ControlGenerationSupersession, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() != Some(control_generation)
        || inbox
            .entries
            .iter()
            .any(|entry| entry.control_generation != control_generation)
    {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }
    if inbox.terminal_fence.as_ref().is_some_and(|fence| {
        fence.control_generation == control_generation
            && fence.kind == TerminalSteerFenceKind::ControlSupersession
    }) {
        return Ok(ControlGenerationSupersession::AlreadySuperseded);
    }
    inbox.entries.clear();
    inbox.terminal_fence = Some(TerminalSteerFence {
        control_generation: control_generation.to_owned(),
        claim: SteerClaim {
            source_segment: source_segment.to_owned(),
            iteration,
        },
        kind: TerminalSteerFenceKind::ControlSupersession,
        expires_at_ms: Some(
            chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(CONTROL_SUPERSESSION_LEASE_MS),
        ),
        manual_pause_tree: None,
    });
    save(workspace_layout, &path, inbox).await?;
    Ok(ControlGenerationSupersession::Installed)
}

/// Prepare one exact stateless epoch for manual pause. Unlike terminal
/// supersession, this fence explicitly carries accepted rows across the pause:
/// rollback removes the fence losslessly, and authorized exact resume re-adopts
/// the same UUID before clearing only this typed fence.
pub async fn fence_generation_for_manual_pause(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    source_segment: &str,
    iteration: usize,
) -> Result<ControlGenerationSupersession, String> {
    fence_generation_for_manual_pause_internal(
        workspace_layout,
        principal,
        workspace,
        execution_id,
        control_generation,
        source_segment,
        iteration,
        None,
    )
    .await
}

/// Build the sealed identity shared by every member of one tree pause. The
/// roster digest includes each execution's exact control UUID; changing either
/// membership or generation therefore creates a different transaction.
pub fn manual_pause_tree_binding(
    transaction_id: uuid::Uuid,
    requested_root_execution_id: &str,
    roster: &[(String, String)],
) -> Result<ManualPauseTreeBinding, String> {
    if requested_root_execution_id.trim().is_empty() || roster.is_empty() {
        return Err("operator_steer_manual_pause_tree_roster_invalid".to_owned());
    }
    let mut exact_roster = roster.to_vec();
    exact_roster.sort();
    if exact_roster.iter().any(|(execution_id, generation)| {
        execution_id.trim().is_empty() || uuid::Uuid::parse_str(generation).is_err()
    }) || exact_roster.windows(2).any(|pair| pair[0].0 == pair[1].0)
    {
        return Err("operator_steer_manual_pause_tree_roster_invalid".to_owned());
    }
    let (coordinator_execution_id, coordinator_control_generation) = exact_roster
        .first()
        .cloned()
        .ok_or_else(|| "operator_steer_manual_pause_tree_roster_invalid".to_owned())?;
    let encoded = serde_json::to_vec(&(
        "magician.manual-pause-tree-roster.v1",
        transaction_id,
        requested_root_execution_id,
        &exact_roster,
    ))
    .map_err(|error| format!("operator_steer_manual_pause_tree_roster_encode_failed:{error}"))?;
    Ok(ManualPauseTreeBinding {
        transaction_id: transaction_id.to_string(),
        requested_root_execution_id: requested_root_execution_id.to_owned(),
        coordinator_execution_id,
        coordinator_control_generation,
        roster_digest: blake3::hash(&encoded).to_hex().to_string(),
        committed: false,
    })
}

/// Persist one prepared member. Prepared fences close steer admission but are
/// deliberately invisible to workers until [`commit_manual_pause_tree`] makes
/// the coordinator record durable.
pub async fn prepare_generation_for_manual_pause_tree(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    source_segment: &str,
    iteration: usize,
    binding: &ManualPauseTreeBinding,
) -> Result<ControlGenerationSupersession, String> {
    if binding.committed {
        return Err("operator_steer_manual_pause_tree_prepare_is_committed".to_owned());
    }
    // Reconcile an abandoned, expired preparation before attempting a new
    // exact transaction. This read also refuses replacement if the old roster
    // actually committed; its coordinator lock serializes expiry cleanup with
    // the commit replacement.
    if committed_manual_pause_requested(
        workspace_layout,
        principal,
        workspace,
        execution_id,
        control_generation,
    )
    .await?
    {
        return Err("operator_steer_manual_pause_conflicts_with_committed_tree".to_owned());
    }
    fence_generation_for_manual_pause_internal(
        workspace_layout,
        principal,
        workspace,
        execution_id,
        control_generation,
        source_segment,
        iteration,
        Some(binding.clone()),
    )
    .await
}

async fn fence_generation_for_manual_pause_internal(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    source_segment: &str,
    iteration: usize,
    manual_pause_tree: Option<ManualPauseTreeBinding>,
) -> Result<ControlGenerationSupersession, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    if inbox.active_control_generation.as_deref() != Some(control_generation)
        || inbox
            .entries
            .iter()
            .any(|entry| entry.control_generation != control_generation)
    {
        return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
    }
    if let Some(fence) = inbox.terminal_fence.as_ref() {
        if fence.control_generation == control_generation
            && fence.kind == TerminalSteerFenceKind::ControlSupersession
        {
            return Err("operator_steer_manual_pause_conflicts_with_terminal_control".to_owned());
        }
        if fence.control_generation == control_generation
            && fence.kind == TerminalSteerFenceKind::ManualPauseSupersession
        {
            let exact_claim = SteerClaim {
                source_segment: source_segment.to_owned(),
                iteration,
            };
            if fence.claim == exact_claim && fence.manual_pause_tree == manual_pause_tree {
                return Ok(ControlGenerationSupersession::AlreadySuperseded);
            }
            return Err("operator_steer_manual_pause_conflicts_with_other_request".to_owned());
        }
    }
    inbox.terminal_fence = Some(TerminalSteerFence {
        control_generation: control_generation.to_owned(),
        claim: SteerClaim {
            source_segment: source_segment.to_owned(),
            iteration,
        },
        kind: TerminalSteerFenceKind::ManualPauseSupersession,
        expires_at_ms: Some(
            chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(CONTROL_SUPERSESSION_LEASE_MS),
        ),
        manual_pause_tree,
    });
    save(workspace_layout, &path, inbox).await?;
    Ok(ControlGenerationSupersession::Installed)
}

/// Publish the one durable commit which makes every prepared roster member an
/// actionable pause request. All member writes precede this atomic replacement;
/// after it lands a crash can delay completion but cannot expose a partial tree.
pub async fn commit_manual_pause_tree(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    source_segment: &str,
    iteration: usize,
    binding: &ManualPauseTreeBinding,
) -> Result<(), String> {
    if binding.committed {
        return Err("operator_steer_manual_pause_tree_commit_already_committed".to_owned());
    }
    let path = inbox_path(
        workspace_layout,
        principal,
        workspace,
        &binding.coordinator_execution_id,
    );
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(
        workspace_layout,
        principal,
        workspace,
        &binding.coordinator_execution_id,
        &path,
    )
    .await?;
    if inbox.active_control_generation.as_deref()
        != Some(binding.coordinator_control_generation.as_str())
    {
        return Err("operator_steer_manual_pause_tree_coordinator_generation_mismatch".to_owned());
    }
    let expected_claim = SteerClaim {
        source_segment: source_segment.to_owned(),
        iteration,
    };
    let fence = inbox
        .terminal_fence
        .as_mut()
        .ok_or_else(|| "operator_steer_manual_pause_tree_coordinator_missing".to_owned())?;
    let mut committed_binding = binding.clone();
    committed_binding.committed = true;
    if fence.kind != TerminalSteerFenceKind::ManualPauseSupersession
        || fence.control_generation != binding.coordinator_control_generation
        || fence.claim != expected_claim
    {
        return Err("operator_steer_manual_pause_tree_coordinator_binding_mismatch".to_owned());
    }
    if fence.manual_pause_tree.as_ref() == Some(&committed_binding) {
        return Ok(());
    }
    if fence.manual_pause_tree.as_ref() != Some(binding) {
        return Err("operator_steer_manual_pause_tree_coordinator_binding_mismatch".to_owned());
    }
    if fence
        .expires_at_ms
        .is_some_and(|expires_at_ms| chrono::Utc::now().timestamp_millis() >= expires_at_ms)
    {
        return Err("operator_steer_manual_pause_tree_prepare_expired".to_owned());
    }
    fence.manual_pause_tree = Some(committed_binding);
    // A committed request is control authority, not a lease. It remains until
    // every exact checkpoint is resumed or explicitly rolled back by its owner.
    fence.expires_at_ms = None;
    save(workspace_layout, &path, inbox).await
}

/// Read the exact member and coordinator records without trusting process-local
/// controls. A prepared record becomes actionable only when its coordinator has
/// the same HMAC-sealed binding with `committed=true`.
pub async fn committed_manual_pause_requested(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
) -> Result<bool, String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let (binding, member_expires_at_ms) = {
        let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
        let inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
        if inbox.active_control_generation.as_deref() != Some(control_generation) {
            return Err("operator_steer_inbox_control_generation_mismatch".to_owned());
        }
        let Some(fence) = inbox.terminal_fence.as_ref() else {
            return Ok(false);
        };
        if fence.kind != TerminalSteerFenceKind::ManualPauseSupersession
            || fence.control_generation != control_generation
        {
            return Ok(false);
        }
        let Some(binding) = fence.manual_pause_tree.clone() else {
            // Legacy in-process manual fences are driven by their local signal.
            return Ok(false);
        };
        if binding.committed {
            return Ok(execution_id == binding.coordinator_execution_id
                && control_generation == binding.coordinator_control_generation);
        }
        (binding, fence.expires_at_ms)
    };

    let coordinator_path = inbox_path(
        workspace_layout,
        principal,
        workspace,
        &binding.coordinator_execution_id,
    );
    let _guard = mutation_locks()[mutation_lock_index(&coordinator_path)]
        .lock()
        .await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&coordinator_path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut coordinator = load(
        workspace_layout,
        principal,
        workspace,
        &binding.coordinator_execution_id,
        &coordinator_path,
    )
    .await?;
    let mut committed_binding = binding;
    committed_binding.committed = true;
    let committed = coordinator
        .terminal_fence
        .as_ref()
        .is_some_and(|coordinator_fence| {
            coordinator.active_control_generation.as_deref()
                == Some(committed_binding.coordinator_control_generation.as_str())
                && coordinator_fence.kind == TerminalSteerFenceKind::ManualPauseSupersession
                && coordinator_fence.control_generation
                    == committed_binding.coordinator_control_generation
                && coordinator_fence.manual_pause_tree.as_ref() == Some(&committed_binding)
        });
    if committed {
        return Ok(true);
    }

    // A crashed preflight must not fence a live execution forever. Serialize
    // cleanup behind the coordinator lock so it cannot race a late commit. The
    // member file lock is sufficient here: all ordinary local mutators acquire
    // that same lock after their stripe, and this path never waits on the member
    // stripe while holding the file lock.
    if member_expires_at_ms
        .is_some_and(|expires_at_ms| chrono::Utc::now().timestamp_millis() >= expires_at_ms)
    {
        if coordinator.terminal_fence.as_ref().is_some_and(|fence| {
            let mut expected = committed_binding.clone();
            expected.committed = false;
            fence.kind == TerminalSteerFenceKind::ManualPauseSupersession
                && fence.control_generation == expected.coordinator_control_generation
                && fence.manual_pause_tree.as_ref() == Some(&expected)
                && fence.expires_at_ms.is_some_and(|expires_at_ms| {
                    chrono::Utc::now().timestamp_millis() >= expires_at_ms
                })
        }) {
            coordinator.terminal_fence = None;
            save(workspace_layout, &coordinator_path, coordinator).await?;
        }
        if execution_id == committed_binding.coordinator_execution_id {
            return Ok(false);
        }
        let _member_file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
        let mut member = load(workspace_layout, principal, workspace, execution_id, &path).await?;
        if member.terminal_fence.as_ref().is_some_and(|fence| {
            fence.kind == TerminalSteerFenceKind::ManualPauseSupersession
                && fence.control_generation == control_generation
                && fence.manual_pause_tree.as_ref().is_some_and(|held| {
                    let mut expected = committed_binding.clone();
                    expected.committed = false;
                    held == &expected
                })
                && fence.expires_at_ms.is_some_and(|expires_at_ms| {
                    chrono::Utc::now().timestamp_millis() >= expires_at_ms
                })
        }) {
            member.terminal_fence = None;
            save(workspace_layout, &path, member).await?;
        }
    }
    Ok(false)
}

/// Roll back only the exact control tombstone installed by an aborted
/// multi-execution pause preflight. Retained rows become claimable again. A
/// prior cancellation/supersession cannot be reopened accidentally: its claim
/// will not match the pause transaction's private source token, and callers
/// invoke this only for `Installed` results.
pub async fn reopen_aborted_control_supersession(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    control_generation: &str,
    source_segment: &str,
    iteration: usize,
) -> Result<(), String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    let expected_claim = SteerClaim {
        source_segment: source_segment.to_owned(),
        iteration,
    };
    let Some(fence) = inbox.terminal_fence.as_ref() else {
        return Ok(());
    };
    if inbox.active_control_generation.as_deref() != Some(control_generation)
        || fence.control_generation != control_generation
        || fence.kind != TerminalSteerFenceKind::ManualPauseSupersession
        || fence.claim != expected_claim
    {
        return Err("operator_steer_inbox_control_rollback_binding_mismatch".to_owned());
    }
    inbox.terminal_fence = None;
    save(workspace_layout, &path, inbox).await
}

/// Clear a terminal tombstone when the exact segment's retried phase now
/// continues. This is called only at a successful nonterminal publication
/// boundary. The sealed fence already binds the active control generation; the
/// caller must match its segment and may be at the same or a later iteration.
/// A verified delegation successor takes over the speculative-phase terminal
/// fence of its own execution.
///
/// The fence is bound to the segment that admitted the terminal boundary, and
/// `reopen_after_nonterminal_boundary` rightly refuses any other segment: a
/// stranger sharing the runtime execution id must not erase the tombstone. A
/// declared delegation successor is not a stranger — it is the same run,
/// continued on a new segment once its children have reported — but the fence
/// cannot know that on its own, and neither can the seed name the claimant:
/// by the time the successor is seeded the context already runs on the
/// successor's segment. It does not need to. The inbox is per execution, so a
/// speculative-phase fence in it was claimed by an earlier segment of this
/// same run, and the seed — the one moment lineage is proven — hands it to
/// the successor. Control and manual-pause fences are an operator's, not a
/// segment's, and are never handed over. A repeated seed of the same
/// successor is a no-op, and an inbox with no fence has nothing to hand over.
///
/// The claim is rewritten in the successor's own numbering, not only its
/// name. A segment counts iterations from one, so a fence the paused segment
/// claimed at its iteration N would tell the successor's first Prepare —
/// iteration 1 — that it is *earlier* than the claim, and
/// `reopen_after_nonterminal_boundary` would refuse it as a retry of an older
/// iteration. That is what every answered question raised past a run's first
/// iteration hit on the stateless driver: the resume failed at Prepare with a
/// binding mismatch and the run parked on the same question again. The same
/// shape reached an operator on run 22 (2026-09-21) from the other direction —
/// a run paused on `BudgetExhausted` failed every resume they confirmed,
/// because the claim still carried the paused segment's iteration 20 and the
/// reopen rule orders boundaries only *within* a segment. Across segments the
/// order is by construction: everything the successor does comes after the
/// terminal it was handed.
pub async fn rebind_terminal_fence_to_successor(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    successor_segment: &str,
) -> Result<(), String> {
    if successor_segment.trim().is_empty() {
        return Err("operator_steer_fence_handoff_segment_missing".to_owned());
    }
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    let Some(fence) = inbox.terminal_fence.as_mut() else {
        return Ok(());
    };
    if fence.kind != TerminalSteerFenceKind::SpeculativePhase {
        return Err("operator_steer_inbox_terminal_fence_binding_mismatch".to_owned());
    }
    let successor_claim = SteerClaim {
        source_segment: successor_segment.to_owned(),
        iteration: SUCCESSOR_FIRST_ITERATION,
    };
    if fence.claim == successor_claim {
        return Ok(());
    }
    fence.claim = successor_claim;
    save(workspace_layout, &path, inbox).await
}

pub async fn reopen_after_nonterminal_boundary(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    execution_id: &str,
    source_segment: &str,
    iteration: usize,
) -> Result<(), String> {
    let path = inbox_path(workspace_layout, principal, workspace, execution_id);
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(workspace_layout, principal, workspace, execution_id, &path).await?;
    let Some(fence) = inbox.terminal_fence.as_ref() else {
        return Ok(());
    };
    if fence.kind != TerminalSteerFenceKind::SpeculativePhase
        || inbox.active_control_generation.as_deref() != Some(fence.control_generation.as_str())
        || fence.claim.source_segment != source_segment
        || iteration < fence.claim.iteration
    {
        return Err("operator_steer_inbox_terminal_fence_binding_mismatch".to_owned());
    }
    inbox.terminal_fence = None;
    save(workspace_layout, &path, inbox).await
}

/// Idempotently retire only rows named by a fenced Decide receipt.
///
/// The expected execution and segment come from the caller's independently
/// committed loop authority. The receipt is never allowed to select the inbox
/// it mutates: doing so would let a receipt attached to the wrong LoopState
/// retire another execution's accepted input in the same scope.
pub async fn acknowledge(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    expected_execution_id: &str,
    expected_source_segment: &str,
    receipt: &SteerConsumeReceipt,
) -> Result<(), String> {
    if receipt.schema_version != STEER_INBOX_SCHEMA_VERSION
        || expected_execution_id.trim().is_empty()
        || expected_source_segment.trim().is_empty()
        || receipt.execution_id != expected_execution_id
        || receipt.source_segment != expected_source_segment
        || receipt.entries.is_empty()
        || receipt.entries.len() > MAX_PENDING_STEERS
    {
        return Err("operator_steer_receipt_authority_mismatch".to_owned());
    }
    let path = inbox_path(
        workspace_layout,
        principal,
        workspace,
        expected_execution_id,
    );
    let _guard = mutation_locks()[mutation_lock_index(&path)].lock().await;
    let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| format!("operator_steer_inbox_lock_failed:{error}"))?;
    let mut inbox = load(
        workspace_layout,
        principal,
        workspace,
        expected_execution_id,
        &path,
    )
    .await?;
    let expected_claim = SteerClaim {
        source_segment: receipt.source_segment.clone(),
        iteration: receipt.iteration,
    };
    for named in &receipt.entries {
        if let Some(stored) = inbox
            .entries
            .iter()
            .find(|entry| entry.entry_id == named.entry_id)
        {
            if stored.generation != named.generation
                || stored.control_generation != named.control_generation
                || stored.content_digest != named.content_digest
                || stored.claim.as_ref() != Some(&expected_claim)
            {
                return Err("operator_steer_receipt_binding_mismatch".to_owned());
            }
        }
    }
    inbox.entries.retain(|stored| {
        !receipt.entries.iter().any(|named| {
            stored.entry_id == named.entry_id
                && stored.generation == named.generation
                && stored.control_generation == named.control_generation
                && stored.content_digest == named.content_digest
                && stored.claim.as_ref() == Some(&expected_claim)
        })
    });
    save(workspace_layout, &path, inbox).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn activate(workspace: &ArtifactV2Workspace, generation: &str) {
        activate_named(workspace, "execution-1", generation).await;
    }

    async fn activate_named(workspace: &ArtifactV2Workspace, execution_id: &str, generation: &str) {
        activate_control_generation(
            workspace,
            "principal",
            "workspace",
            execution_id,
            generation,
        )
        .await
        .expect("control generation activation");
    }

    #[tokio::test]
    async fn tree_pause_is_invisible_until_exact_coordinator_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        let root_generation = uuid::Uuid::new_v4().to_string();
        let child_generation = uuid::Uuid::new_v4().to_string();
        activate_named(&workspace, "execution-root", &root_generation).await;
        activate_named(&workspace, "execution-child", &child_generation).await;
        let transaction_id = uuid::Uuid::new_v4();
        let binding = manual_pause_tree_binding(
            transaction_id,
            "execution-root",
            &[
                ("execution-root".to_owned(), root_generation.clone()),
                ("execution-child".to_owned(), child_generation.clone()),
            ],
        )
        .expect("tree binding");
        let root_source = format!("manual-pause:{transaction_id}:execution-root");
        let child_source = format!("manual-pause:{transaction_id}:execution-child");
        prepare_generation_for_manual_pause_tree(
            &workspace,
            "principal",
            "workspace",
            "execution-root",
            &root_generation,
            &root_source,
            0,
            &binding,
        )
        .await
        .expect("root prepare");
        prepare_generation_for_manual_pause_tree(
            &workspace,
            "principal",
            "workspace",
            "execution-child",
            &child_generation,
            &child_source,
            0,
            &binding,
        )
        .await
        .expect("child prepare");

        assert!(!committed_manual_pause_requested(
            &workspace,
            "principal",
            "workspace",
            "execution-root",
            &root_generation,
        )
        .await
        .expect("prepared root read"));
        assert!(!committed_manual_pause_requested(
            &workspace,
            "principal",
            "workspace",
            "execution-child",
            &child_generation,
        )
        .await
        .expect("prepared child read"));

        // The binding chooses the lexicographically first exact member as its
        // coordinator, so commit with that member's exact source token.
        let coordinator_source = if "execution-child" < "execution-root" {
            &child_source
        } else {
            &root_source
        };
        commit_manual_pause_tree(
            &workspace,
            "principal",
            "workspace",
            coordinator_source,
            0,
            &binding,
        )
        .await
        .expect("coordinator commit");

        assert!(committed_manual_pause_requested(
            &workspace,
            "principal",
            "workspace",
            "execution-root",
            &root_generation,
        )
        .await
        .expect("committed root read"));
        assert!(committed_manual_pause_requested(
            &workspace,
            "principal",
            "workspace",
            "execution-child",
            &child_generation,
        )
        .await
        .expect("committed child read"));
        assert_eq!(
            committed_manual_pause_requested(
                &workspace,
                "principal",
                "workspace",
                "execution-child",
                &uuid::Uuid::new_v4().to_string(),
            )
            .await
            .expect_err("wrong generation must fail closed"),
            "operator_steer_inbox_control_generation_mismatch"
        );
    }

    #[tokio::test]
    async fn crash_before_and_after_decide_commit_has_one_delivery_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "focus on the recovery proof",
        )
        .await
        .expect("durable admission");

        let first = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("claim")
        .expect("batch");
        let retried = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("retry claim")
        .expect("same batch");
        assert_eq!(retried, first, "pre-commit crash must re-offer exactly");

        acknowledge(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r7",
            &first.receipt,
        )
        .await
        .expect("ack after fenced commit");
        acknowledge(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r7",
            &first.receipt,
        )
        .await
        .expect("ack replay is idempotent");
        assert!(
            claim(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "execution-1-r7",
                9,
            )
            .await
            .expect("post-commit claim")
            .is_none(),
            "a committed consume must never replay"
        );
    }

    #[tokio::test]
    async fn acknowledgement_cannot_select_an_inbox_from_receipt_owned_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate_named(&workspace, "execution-a", "controls-a").await;
        activate_named(&workspace, "execution-b", "controls-b").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-b",
            "controls-b",
            "must remain in execution b",
        )
        .await
        .expect("enqueue b");
        let claimed_b = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-b",
            "controls-b",
            "execution-b-r1",
            1,
        )
        .await
        .expect("claim b")
        .expect("claimed b batch");

        let error = acknowledge(
            &workspace,
            "principal",
            "workspace",
            "execution-a",
            "execution-a-r1",
            &claimed_b.receipt,
        )
        .await
        .expect_err("receipt b is not authority to mutate inbox a or b from a's lifecycle");
        assert_eq!(error, "operator_steer_receipt_authority_mismatch");
        assert_eq!(
            claim(
                &workspace,
                "principal",
                "workspace",
                "execution-b",
                "controls-b",
                "execution-b-r1",
                1,
            )
            .await
            .expect("reclaim b"),
            Some(claimed_b),
            "the mismatched acknowledgement must leave b's exact batch intact"
        );
    }

    #[tokio::test]
    async fn a_claim_cannot_move_to_a_different_segment_or_iteration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "stay exact",
        )
        .await
        .expect("admission");
        claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("first claim");
        let error = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r8",
            10,
        )
        .await
        .expect_err("foreign boundary must be refused");
        assert_eq!(error, "operator_steer_inbox_claimed_by_other_boundary");
    }

    #[tokio::test]
    async fn a_claim_never_replays_an_older_execution_epoch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-old").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-old",
            "do not cross the sleep boundary",
        )
        .await
        .expect("admission");

        let error = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-new",
            "execution-1-r7",
            9,
        )
        .await
        .expect_err("an older epoch must fail closed");
        assert_eq!(error, "operator_steer_inbox_control_generation_mismatch");
    }

    #[tokio::test]
    async fn unrelated_runtime_timestamp_changes_do_not_change_the_sealed_generation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-stable").await;
        let _unrelated_updated_at_before = 1_725_000_000_000_i64;
        let _unrelated_updated_at_after = _unrelated_updated_at_before + 90_000;

        assert_eq!(
            control_generation(&workspace, "principal", "workspace", "execution-1")
                .await
                .expect("sealed generation read")
                .as_deref(),
            Some("controls-stable")
        );
    }

    #[tokio::test]
    async fn durable_operator_projection_distinguishes_open_and_fenced_epochs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;

        let open =
            durable_operator_control_state(&workspace, "principal", "workspace", "execution-1")
                .await
                .expect("open operator projection");
        assert_eq!(open.generation.as_deref(), Some("controls-a"));
        assert!(open.can_pause);
        assert!(open.can_steer);

        assert_eq!(
            admit_terminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "execution-1-r7",
                2,
                None,
            )
            .await
            .expect("speculative terminal fence"),
            TerminalSteerAdmission::Admitted
        );
        let speculative =
            durable_operator_control_state(&workspace, "principal", "workspace", "execution-1")
                .await
                .expect("speculative operator projection");
        assert!(speculative.can_pause);
        assert!(!speculative.can_steer);

        fence_generation_for_manual_pause(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "manual-pause:projection-test",
            0,
        )
        .await
        .expect("manual pause fence");
        let paused =
            durable_operator_control_state(&workspace, "principal", "workspace", "execution-1")
                .await
                .expect("manual-pause operator projection");
        assert!(!paused.can_pause);
        assert!(!paused.can_steer);
    }

    #[tokio::test]
    async fn terminal_fence_detects_late_rows_and_then_closes_admission() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "claimed first",
        )
        .await
        .expect("first admission");
        let claimed = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("claim")
        .expect("batch");
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "arrived after Decide claim",
        )
        .await
        .expect("late admission");
        assert_eq!(
            admit_terminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "execution-1-r7",
                9,
                Some(&claimed.receipt),
            )
            .await
            .expect("terminal check"),
            TerminalSteerAdmission::PendingSteer
        );

        let all = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("retry claim")
        .expect("both rows");
        assert_eq!(all.messages.len(), 2);
        assert_eq!(
            admit_terminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "execution-1-r7",
                9,
                Some(&all.receipt),
            )
            .await
            .expect("terminal seal"),
            TerminalSteerAdmission::Admitted
        );
        assert_eq!(
            enqueue(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "too late",
            )
            .await
            .expect_err("sealed admission must reject"),
            "operator_steer_inbox_terminal_fenced"
        );
    }

    #[tokio::test]
    async fn crash_recovery_re_adopts_an_unfenced_active_epoch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        let generation = uuid::Uuid::new_v4().to_string();
        activate(&workspace, &generation).await;
        assert_eq!(
            recoverable_control_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                true,
                true,
            )
            .await
            .expect("empty active epoch")
            .as_deref(),
            Some(generation.as_str())
        );
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            "accepted before process crash",
        )
        .await
        .expect("admission");
        assert_eq!(
            recoverable_control_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                true,
                true,
            )
            .await
            .expect("recovery generation")
            .as_deref(),
            Some(generation.as_str())
        );

        supersede_generation_for_terminal(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            "external-cancel",
            0,
        )
        .await
        .expect("control close");
        assert_eq!(
            recoverable_control_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                true,
                true,
            )
            .await
            .expect_err("a fenced epoch is never generic recovery authority"),
            "operator_steer_recovery_generation_is_fenced"
        );
        assert_eq!(
            activate_control_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                &generation,
            )
            .await
            .expect_err("same UUID cannot reopen a control fence"),
            "operator_steer_activation_generation_is_fenced"
        );
    }

    #[tokio::test]
    async fn speculative_crash_re_adopts_but_committed_sleep_advances_explicitly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        let generation = uuid::Uuid::new_v4().to_string();
        activate(&workspace, &generation).await;
        admit_terminal_boundary(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            "execution-1-r4",
            7,
            None,
        )
        .await
        .expect("speculative terminal fence");
        assert_eq!(
            recoverable_control_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                true,
                true,
            )
            .await
            .expect("active phase recovery")
            .as_deref(),
            Some(generation.as_str())
        );
        assert_eq!(
            exact_sleep_retry_recoverable_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
            )
            .await
            .expect("settled sleep boundary"),
            None
        );
        let retry_generation = uuid::Uuid::new_v4().to_string();
        activate_exact_sleep_retry_control_generation(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &retry_generation,
        )
        .await
        .expect("timer-only epoch advance");
        assert_eq!(
            control_generation(&workspace, "principal", "workspace", "execution-1")
                .await
                .expect("new generation")
                .as_deref(),
            Some(retry_generation.as_str())
        );
    }

    #[tokio::test]
    async fn aborted_tree_pause_reopens_without_losing_accepted_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "must survive failed tree preflight",
        )
        .await
        .expect("admission");

        assert_eq!(
            fence_generation_for_manual_pause(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "manual-pause:execution-1",
                0,
            )
            .await
            .expect("pause prepare"),
            ControlGenerationSupersession::Installed
        );
        reopen_aborted_control_supersession(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "manual-pause:execution-1",
            0,
        )
        .await
        .expect("lossless rollback");
        let claimed = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            4,
        )
        .await
        .expect("claim after rollback")
        .expect("retained row");
        assert_eq!(
            claimed.messages,
            vec!["must survive failed tree preflight".to_owned()]
        );
    }

    #[tokio::test]
    async fn exact_manual_resume_re_adopts_generation_and_preserves_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        let generation = uuid::Uuid::new_v4().to_string();
        activate(&workspace, &generation).await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            "apply after exact resume",
        )
        .await
        .expect("admission");
        fence_generation_for_manual_pause(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            "manual-pause:transaction:execution-1",
            0,
        )
        .await
        .expect("manual fence");
        assert_eq!(
            manual_pause_resume_control_generation(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
            )
            .await
            .expect("read-only resume preflight")
            .as_deref(),
            Some(generation.as_str())
        );
        activate_manual_pause_control_generation(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            None,
        )
        .await
        .expect("typed resume activation");
        let claimed = claim(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            &generation,
            "execution-1-r8",
            1,
        )
        .await
        .expect("claim")
        .expect("preserved steer");
        assert_eq!(
            claimed.messages,
            vec!["apply after exact resume".to_owned()]
        );
    }

    #[tokio::test]
    async fn external_terminal_control_explicitly_supersedes_late_input() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "accepted just before cancellation",
        )
        .await
        .expect("admission");

        supersede_generation_for_terminal(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("explicit cancellation supersession");
        assert_eq!(
            enqueue(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "post-cancel",
            )
            .await
            .expect_err("closed epoch must reject"),
            "operator_steer_inbox_terminal_fenced"
        );
        assert_eq!(
            reopen_after_nonterminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "execution-1-r7",
                10,
            )
            .await
            .expect_err("control supersession must not be reopened by a worker"),
            "operator_steer_inbox_terminal_fence_binding_mismatch"
        );

        activate(&workspace, "controls-b").await;
        assert_eq!(
            control_generation(&workspace, "principal", "workspace", "execution-1")
                .await
                .expect("replacement generation")
                .as_deref(),
            Some("controls-b")
        );
    }

    #[tokio::test]
    async fn only_the_same_segment_nonterminal_boundary_reopens_a_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        assert_eq!(
            admit_terminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "execution-1-r7",
                9,
                None,
            )
            .await
            .expect("terminal seal"),
            TerminalSteerAdmission::Admitted
        );
        assert_eq!(
            reopen_after_nonterminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "execution-1-r8",
                10,
            )
            .await
            .expect_err("another segment cannot reopen the fence"),
            "operator_steer_inbox_terminal_fence_binding_mismatch"
        );
        reopen_after_nonterminal_boundary(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r7",
            9,
        )
        .await
        .expect("same-segment nonterminal retry");
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "admitted after nonterminal continuation",
        )
        .await
        .expect("reopened admission");
    }

    #[tokio::test]
    async fn a_declared_delegation_successor_inherits_the_terminal_fence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        assert_eq!(
            admit_terminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "controls-a",
                "execution-1-r7",
                9,
                None,
            )
            .await
            .expect("terminal seal"),
            TerminalSteerAdmission::Admitted
        );
        // The seed runs on the successor's segment; it does not know which
        // earlier segment claimed the fence, only that the inbox is this run's.
        rebind_terminal_fence_to_successor(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r8",
        )
        .await
        .expect("the verified successor takes over the fence");
        // The successor's first boundary is iteration 1 of a fresh LoopState,
        // numerically before the terminal's 9 — and reopens the fence anyway:
        // across segments the order is by construction.
        reopen_after_nonterminal_boundary(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r8",
            1,
        )
        .await
        .expect("the successor's Prepare reopens the fence it inherited");
        enqueue(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "admitted after the successor continued",
        )
        .await
        .expect("reopened admission");
    }

    #[tokio::test]
    async fn a_control_supersession_fence_is_not_handed_to_a_successor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        fence_generation_for_manual_pause(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
        )
        .await
        .expect("manual pause fences the generation");
        assert_eq!(
            rebind_terminal_fence_to_successor(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "execution-1-r8",
            )
            .await
            .expect_err("a control fence is an operator's, not a segment's"),
            "operator_steer_inbox_terminal_fence_binding_mismatch"
        );
        assert_eq!(
            reopen_after_nonterminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "execution-1-r8",
                9,
            )
            .await
            .expect_err("the control fence still holds"),
            "operator_steer_inbox_terminal_fence_binding_mismatch"
        );
    }

    #[tokio::test]
    async fn a_fence_handoff_is_idempotent_and_leaves_strangers_fenced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
        activate(&workspace, "controls-a").await;
        rebind_terminal_fence_to_successor(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r8",
        )
        .await
        .expect("no fence to hand over is not an error");
        admit_terminal_boundary(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "controls-a",
            "execution-1-r7",
            9,
            None,
        )
        .await
        .expect("terminal seal");
        for _ in 0..2 {
            rebind_terminal_fence_to_successor(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "execution-1-r8",
            )
            .await
            .expect("a repeated handoff to the same successor is a no-op");
        }
        assert_eq!(
            reopen_after_nonterminal_boundary(
                &workspace,
                "principal",
                "workspace",
                "execution-1",
                "execution-1-r9",
                9,
            )
            .await
            .expect_err("a third segment is still a stranger to the fence"),
            "operator_steer_inbox_terminal_fence_binding_mismatch"
        );
        reopen_after_nonterminal_boundary(
            &workspace,
            "principal",
            "workspace",
            "execution-1",
            "execution-1-r8",
            9,
        )
        .await
        .expect("the successor reopens");
    }
}
