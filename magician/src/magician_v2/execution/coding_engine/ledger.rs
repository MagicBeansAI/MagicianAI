//! Durable coding-invocation ledger for one Artifact V2 execution.
//!
//! Stage 1c of the Codex plan. The ledger lives next to `coding_events.jsonl`
//! as `coding_ledger.json` so we do not add a field to every
//! `ExecutionRecord` constructor. Append-or-reuse is the same contract the
//! selection types already test.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::codex_lifecycle::ContinuationFreshReason;
use super::selection::{
    CodingContinuationRef, CodingDispatchState, CodingEngineUsage, CodingExecutionLedger,
    CodingInvocationState, CodingTerminalClass, ResolvedCodingEngineSelection, SelectionError,
    INVOCATION_SCHEMA_VERSION,
};

pub const CODING_LEDGER_FILE_NAME: &str = "coding_ledger.json";
/// Bound for one execution. The agentic loop's iteration cap is much larger;
/// this is the coding-invocation budget, not the model-turn budget.
pub const DEFAULT_CODING_INVOCATION_CAP: usize = 32;
const CHAIN_CONTINUATION_DIR: &str = "chain_continuations";
const MAX_TASK_LEDGER_DIRS: usize = 8;

#[derive(Debug)]
pub enum LedgerStoreError {
    Selection(SelectionError),
    Io(std::io::Error),
    Parse(String),
}

impl std::fmt::Display for LedgerStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Selection(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "coding ledger I/O: {error}"),
            Self::Parse(error) => write!(f, "coding ledger parse: {error}"),
        }
    }
}

impl std::error::Error for LedgerStoreError {}

impl From<SelectionError> for LedgerStoreError {
    fn from(error: SelectionError) -> Self {
        Self::Selection(error)
    }
}

pub fn coding_ledger_path(execution_dir: &Path) -> PathBuf {
    execution_dir.join(CODING_LEDGER_FILE_NAME)
}

pub fn load_coding_ledger(execution_dir: &Path) -> Result<CodingExecutionLedger, LedgerStoreError> {
    let path = coding_ledger_path(execution_dir);
    match std::fs::read(&path) {
        // The message names the FILE and the remedy, because this is the one
        // `Parse` in this module that means "something on disk is unreadable"
        // rather than "something in memory would not serialise" -- and the
        // caller that surfaces it, `run_coding_task`, can only report that the
        // invocation could not be journalled, not what to do about it.
        //
        // Two causes reach here and one remedy covers both: a torn write (a
        // crash or a full disk mid-`store_coding_ledger`), and a ledger written
        // by a NEWER build carrying a field this one does not know --
        // `CodingInvocationState` is `deny_unknown_fields`, so an unrecognised
        // key fails the whole file rather than being ignored.
        //
        // Deleting is lossy in a bounded way, and the bound is what makes it
        // safe to recommend: this file is per-execution, so what goes is this
        // execution's invocation history and the resume handle for any job
        // still in flight under it. The chain-root continuation store lives
        // under `scope_root` rather than here, so resuming a chain across
        // executions is unaffected.
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            LedgerStoreError::Parse(format!(
                "{error} (reading {}) -- this file is unreadable, either torn by a \
                 crash mid-write or written by a newer build. Deleting it lets the \
                 next coding job start, and costs this execution's invocation \
                 history plus the resume handle for any job still in flight under \
                 it; chain continuations are stored elsewhere and survive",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CodingExecutionLedger::default())
        },
        Err(error) => Err(LedgerStoreError::Io(error)),
    }
}

/// Last coding continuation for a VibeDev chain root. Survives a follow-up
/// that opens a new task/execution (those ledgers start empty).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainContinuationRecord {
    pub continuation: CodingContinuationRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_reason: Option<ContinuationFreshReason>,
}

pub fn chain_continuation_dir(scope_root: &Path) -> PathBuf {
    scope_root
        .join("coding_engine")
        .join(CHAIN_CONTINUATION_DIR)
}

pub fn chain_continuation_path(scope_root: &Path, root_task_id: &str) -> PathBuf {
    chain_continuation_dir(scope_root).join(format!("{}.json", safe_path_segment(root_task_id)))
}

pub fn store_chain_continuation(
    scope_root: &Path,
    continuation: &CodingContinuationRef,
    fresh_reason: Option<ContinuationFreshReason>,
) -> Result<(), LedgerStoreError> {
    if continuation.root_task_id.trim().is_empty()
        || continuation.native_session_id.trim().is_empty()
    {
        return Err(LedgerStoreError::Parse(
            "chain continuation requires a root task id and native session id".to_string(),
        ));
    }
    let dir = chain_continuation_dir(scope_root);
    std::fs::create_dir_all(&dir).map_err(LedgerStoreError::Io)?;
    let path = chain_continuation_path(scope_root, &continuation.root_task_id);
    let record = ChainContinuationRecord {
        continuation: continuation.clone(),
        fresh_reason,
    };
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|error| LedgerStoreError::Parse(error.to_string()))?;
    crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&path, &bytes).map_err(|error| {
        match error {
            crate::magician_v2::artifact_v2::service::ArtifactV2Error::Io(io) => {
                LedgerStoreError::Io(io)
            },
            other => LedgerStoreError::Parse(other.to_string()),
        }
    })
}

pub fn load_chain_continuation_record(
    scope_root: &Path,
    root_task_id: &str,
) -> Option<ChainContinuationRecord> {
    if !is_safe_path_segment(root_task_id) {
        return None;
    }
    let path = chain_continuation_path(scope_root, root_task_id);
    let bytes = std::fs::read(path).ok()?;
    let record: ChainContinuationRecord = serde_json::from_slice(&bytes).ok()?;
    if record.continuation.native_session_id.trim().is_empty() {
        return None;
    }
    if !record.continuation.root_task_id.is_empty()
        && record.continuation.root_task_id != root_task_id
    {
        return None;
    }
    Some(record)
}

pub fn load_chain_continuation(
    scope_root: &Path,
    root_task_id: &str,
) -> Option<CodingContinuationRef> {
    load_chain_continuation_record(scope_root, root_task_id).map(|record| record.continuation)
}

/// Most recent continuation on an execution ledger, any engine.
pub fn latest_ledger_continuation(execution_dir: &Path) -> Option<CodingContinuationRef> {
    let ledger = load_coding_ledger(execution_dir).ok()?;
    ledger
        .invocations
        .iter()
        .rev()
        .filter_map(|entry| entry.continuation.clone())
        .find(|item| !item.native_session_id.trim().is_empty())
}

/// Execution dirs under a task that already have a coding ledger, newest first.
pub fn coding_ledger_dirs_for_task(scope_root: &Path, task_id: &str) -> Vec<PathBuf> {
    if !is_safe_path_segment(task_id) {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    for root_name in ["tasks", "internal_tasks"] {
        let executions = scope_root.join(root_name).join(task_id).join("executions");
        let Ok(entries) = std::fs::read_dir(&executions) else {
            continue;
        };
        let mut found = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_dir() || !is_safe_path_segment(&entry.file_name().to_string_lossy()) {
                    return None;
                }
                if !path.join(CODING_LEDGER_FILE_NAME).is_file() {
                    return None;
                }
                let mtime = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                Some((mtime, path))
            })
            .collect::<Vec<_>>();
        found.sort_by(|left, right| right.0.cmp(&left.0));
        dirs.extend(
            found
                .into_iter()
                .take(MAX_TASK_LEDGER_DIRS)
                .map(|(_, path)| path),
        );
    }
    dirs
}

pub fn is_safe_path_segment(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed == value
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.contains('\0')
}

fn safe_path_segment(value: &str) -> String {
    let safe: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "_".to_string()
    } else {
        safe
    }
}

pub fn store_coding_ledger(
    execution_dir: &Path,
    ledger: &CodingExecutionLedger,
) -> Result<(), LedgerStoreError> {
    std::fs::create_dir_all(execution_dir).map_err(LedgerStoreError::Io)?;
    let path = coding_ledger_path(execution_dir);
    let bytes = serde_json::to_vec_pretty(ledger)
        .map_err(|error| LedgerStoreError::Parse(error.to_string()))?;
    // The shared durable writer, not a hand-rolled temp-and-rename. The
    // previous version used a FIXED temp name (`<ledger>.json.tmp`), so two
    // executions writing the same ledger concurrently would write the same
    // staging file and one would publish the other's bytes; it also never
    // synced, so a crash could leave the rename durable and the contents not.
    // `write_bytes_atomic_sync` handles both — a unique temp, `sync_all`, and a
    // parent-directory sync.
    crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&path, &bytes).map_err(|error| {
        match error {
            crate::magician_v2::artifact_v2::service::ArtifactV2Error::Io(io) => {
                LedgerStoreError::Io(io)
            },
            other => LedgerStoreError::Parse(other.to_string()),
        }
    })
}

pub fn coding_input_digest(prompt: &str, profile_id: &str, repo_path: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", "magician.coding_engine.input.v1");
    absorb(&mut hasher, "prompt", prompt);
    absorb(&mut hasher, "profile_id", profile_id);
    absorb(&mut hasher, "repo_path", repo_path);
    hasher.finalize().to_hex().to_string()
}

pub fn coding_invocation_id(execution_id: &str, input_digest: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(
        &mut hasher,
        "domain",
        "magician.coding_engine.invocation_id.v1",
    );
    absorb(&mut hasher, "execution_id", execution_id);
    absorb(&mut hasher, "input", input_digest);
    format!("cinv-{}", &hasher.finalize().to_hex()[..24])
}

/// The invocation id a **loop** mints for a coding job before it fires.
///
/// Derived from two identifiers the loop already owns and neither of which is
/// content: the execution, and the effect id that keys the dispatch's row in
/// the loop-side effect ledger. That is what makes it usable as a
/// `reattach_ref` — see `phases::apply::declared_retry_safety`:
///
/// - **Stable across a re-run of the same effect.** An `EffectId` names one
///   assistant turn's one tool call, so a worker that picks the run back up
///   mints the same value and [`prepare_coding_invocation`] reuses the entry
///   already on disk rather than appending a second one.
/// - **Distinct between two coding jobs in one batch.** `PendingBatch::validate`
///   refuses a batch with a duplicate effect id, so two members cannot mint one
///   invocation id — which is the ambiguity that makes *"resume the latest
///   invocation"* resolve to the wrong session exactly when a run has several.
///
/// **Not** [`coding_invocation_id`], and deliberately domain-separated from it:
/// that one hashes the job's *content* (prompt, profile, repo), so two dispatches
/// of the same prompt collapse onto one id. Collapsing is the right answer for a
/// caller with nothing better to key by; it is the wrong answer for a reattach,
/// which has to name **which** dispatch it is resuming.
pub fn coding_invocation_id_for_effect(execution_id: &str, effect_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(
        &mut hasher,
        "domain",
        "magician.coding_engine.invocation_id.effect.v1",
    );
    absorb(&mut hasher, "execution_id", execution_id);
    absorb(&mut hasher, "effect_id", effect_id);
    format!("cinv-{}", &hasher.finalize().to_hex()[..24])
}

fn absorb(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

/// Journal a coding invocation before the job runs.
///
/// # `invocation_id` — supplied by the caller, or derived from the input
///
/// `Some(id)` is the caller stating the id, which is what a loop that has to be
/// able to find this entry again does: it minted the same value into its own
/// effect-ledger row at intent time (see [`coding_invocation_id_for_effect`]),
/// so the id in that row and the id on disk are the same value carried, not two
/// derivations agreeing. `None` keeps the content derivation for a call with no
/// dispatch identity to key by, and that is most of them: this function has one
/// production caller — `run_coding_task` — and it is reached from chat, from
/// VibeDev and from a direct handler invocation as well as from the agentic
/// loop's `Apply`. Only the last of those has a loop-side effect row, so only
/// the last supplies an id. (`run_project_checks` calls this too, from a test
/// fixture and from no production path.)
///
/// A blank supplied id is treated as absent rather than accepted. `""` would key
/// every unnamed invocation of an execution to one entry, which is the same
/// collapse `EffectId` refuses for the same reason.
///
/// # What a supplied id changes about `append_or_reuse`, and what it does not
///
/// Reuse is unchanged where it matters: a re-run of the same effect recomputes
/// the same id **and** the same `canonical_input_digest`, so
/// `CodingExecutionLedger::append_or_reuse` finds the stored entry, sees an equal
/// `authority_digest`, and returns it — one entry, which is the whole point.
///
/// Two things move, and both move in the safe direction:
///
/// - Two *different* coding effects in one execution that happen to carry an
///   identical prompt, profile and repo used to collapse onto one derived id and
///   share a single entry. They now get one entry each. That costs ledger slots
///   against `DEFAULT_CODING_INVOCATION_CAP` and buys an unambiguous reattach.
/// - A re-run whose *selection* resolved differently — a changed profile between
///   the first attempt and the pickup — now presents the same id with a different
///   `authority_digest` and is refused as `LedgerIntegrityConflict`. Under the
///   derived id it would silently have become a second entry, and the reattach
///   would have found the wrong one.
pub fn prepare_coding_invocation(
    execution_dir: &Path,
    execution_id: &str,
    invocation_id: Option<&str>,
    constraint_digest: &str,
    selection: ResolvedCodingEngineSelection,
    prompt: &str,
    repo_path: &str,
    cap: usize,
) -> Result<CodingInvocationState, LedgerStoreError> {
    let input_digest = coding_input_digest(prompt, &selection.profile_id, repo_path);
    let entry = CodingInvocationState {
        invocation_id: invocation_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| coding_invocation_id(execution_id, &input_digest)),
        schema_version: INVOCATION_SCHEMA_VERSION,
        constraint_digest: constraint_digest.to_string(),
        selection,
        dispatch: CodingDispatchState::Prepared {
            canonical_input_digest: input_digest.clone(),
            base_turn_id: None,
        },
        canonical_input_digest: input_digest,
        continuation: None,
        live_continuation: None,
        predecessor: None,
        context_envelope_digest: None,
        generation: 1,
        usage: None,
    };
    let mut ledger = load_coding_ledger(execution_dir)?;
    let stored = ledger.append_or_reuse(entry, cap)?.clone();
    store_coding_ledger(execution_dir, &ledger)?;
    Ok(stored)
}

/// The session a named coding invocation reports, or `None`.
///
/// The recovery half of [`coding_invocation_id_for_effect`]: a loop holding a
/// `reattach_ref` asks this, and the answer is the `native_session_id` the job
/// reported — through [`attach_invocation_continuation`] once the turn settled,
/// or through [`attach_live_invocation_session`] while it was still open.
///
/// # The settled session wins, and the order is the whole point
///
/// A settled `continuation` carries `last_completed_turn_id` and is the value
/// every other reader already trusts, so it is preferred whenever it exists.
/// `live_continuation` is consulted only in its absence — which is exactly the
/// mid-turn death this fallback exists for, and is why this is the ONE lookup
/// that reads that field. It is keyed by invocation id, so it answers about a
/// named job rather than about "the latest", and a live session is a correct
/// answer to *that* question in a way it is not to the resume-binding scans.
///
/// # `None` is still *the job never reported a session*
///
/// Three shapes reach it and none of them is "resume something else": no ledger
/// on disk, no entry under this id, or an entry that reported neither session —
/// the worker died between [`prepare_coding_invocation`] and the engine's first
/// word about a session, a window this change narrows to the engine handshake
/// but does not close. A caller that answered the *latest* invocation's session
/// instead would resume a real session belonging to a different dispatch —
/// silently, and only when a run fired more than one coding job, which is
/// exactly when it matters.
/// Recovery classification for one exact coding invocation.
///
/// A settled continuation is deliberately distinct from a live session. It is
/// evidence that the outward coding turn already completed, so treating it as
/// a session to resume would execute the prompt a second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvocationReattachState {
    Live(CodingContinuationRef),
    Settled {
        continuation: Option<CodingContinuationRef>,
    },
    Absent,
}

pub fn invocation_reattach_state(
    execution_dir: &Path,
    invocation_id: &str,
) -> InvocationReattachState {
    let Some(entry) = load_coding_ledger(execution_dir).ok().and_then(|ledger| {
        ledger
            .invocations
            .into_iter()
            .find(|entry| entry.invocation_id == invocation_id)
    }) else {
        return InvocationReattachState::Absent;
    };
    if matches!(entry.dispatch, CodingDispatchState::Settled { .. }) {
        return InvocationReattachState::Settled {
            continuation: entry.continuation.or(entry.live_continuation),
        };
    }
    entry
        .live_continuation
        .or(entry.continuation)
        .filter(|continuation| !continuation.native_session_id.trim().is_empty())
        .map(InvocationReattachState::Live)
        .unwrap_or(InvocationReattachState::Absent)
}

/// Compatibility read for non-recovery callers that only need the recorded
/// continuation. Stateless recovery must use [`invocation_reattach_state`].
pub fn invocation_continuation(
    execution_dir: &Path,
    invocation_id: &str,
) -> Option<CodingContinuationRef> {
    match invocation_reattach_state(execution_dir, invocation_id) {
        InvocationReattachState::Live(continuation) => Some(continuation),
        InvocationReattachState::Settled { continuation } => continuation,
        InvocationReattachState::Absent => None,
    }
}

pub fn settle_coding_invocation(
    execution_dir: &Path,
    invocation_id: &str,
    terminal: CodingTerminalClass,
) -> Result<(), LedgerStoreError> {
    mutate_invocation(execution_dir, invocation_id, |entry| {
        entry.dispatch = CodingDispatchState::Settled { terminal };
        Ok(())
    })
}

pub fn mark_invocation_may_have_started(
    execution_dir: &Path,
    invocation_id: &str,
) -> Result<(), LedgerStoreError> {
    mutate_invocation(execution_dir, invocation_id, |entry| {
        entry.dispatch = entry.dispatch.clone().mark_request_may_have_started()?;
        Ok(())
    })
}

pub fn accept_invocation_turn(
    execution_dir: &Path,
    invocation_id: &str,
    provider_turn_id: impl Into<String>,
) -> Result<(), LedgerStoreError> {
    let provider_turn_id = provider_turn_id.into();
    mutate_invocation(execution_dir, invocation_id, |entry| {
        entry.dispatch = entry.dispatch.clone().mark_accepted(provider_turn_id)?;
        Ok(())
    })
}

/// Record the session a turn **settled** on. The success-path write.
///
/// Clearing `live_continuation` is part of the write rather than housekeeping:
/// the settled ref supersedes the mid-flight one in every respect — same
/// session, plus a `last_completed_turn_id` the live write could not have — so
/// leaving both on disk would preserve a strictly worse duplicate of a value
/// that has just been superseded, and the next reader would have to be told
/// which of two refs for one invocation to believe.
pub fn attach_invocation_continuation(
    execution_dir: &Path,
    invocation_id: &str,
    continuation: CodingContinuationRef,
    usage: Option<CodingEngineUsage>,
) -> Result<(), LedgerStoreError> {
    mutate_invocation(execution_dir, invocation_id, |entry| {
        entry.continuation = Some(continuation);
        entry.live_continuation = None;
        if usage.is_some() {
            entry.usage = usage;
        }
        Ok(())
    })
}

/// Record the session a turn is running on, **before** that turn settles.
///
/// The engine-side half of the reattach rule. `attach_invocation_continuation`
/// runs on the handler's success path, so for the whole duration of a long
/// coding job the invocation sat on disk naming no session at all and a worker
/// killed mid-turn had nothing to resume. This is the write that closes that,
/// called the moment the engine first names its session or thread.
///
/// # It writes `live_continuation`, never `continuation`
///
/// See [`CodingInvocationState::live_continuation`] for why: `continuation` is
/// read by two "most recent session in this ledger" scans that hand the answer
/// to a *different* invocation, and a session with a turn still in flight must
/// not appear there.
///
/// # A settled continuation is never overwritten
///
/// The guard is not defensive tidiness. `attach_invocation_continuation` may
/// land first for an engine that reports its session late, and a settled ref
/// carries `last_completed_turn_id` where this one cannot; letting a live write
/// land afterwards would drop that turn id. Once settled, this is a no-op.
///
/// [`CodingInvocationState::live_continuation`]: super::selection::CodingInvocationState::live_continuation
pub fn attach_live_invocation_session(
    execution_dir: &Path,
    invocation_id: &str,
    continuation: CodingContinuationRef,
) -> Result<(), LedgerStoreError> {
    if continuation.native_session_id.trim().is_empty() {
        return Ok(());
    }
    mutate_invocation(execution_dir, invocation_id, |entry| {
        if entry.continuation.is_some() {
            return Ok(());
        }
        entry.live_continuation = Some(continuation);
        Ok(())
    })
}

/// Move an invocation to `CodingDispatchState::DispatchUnknown`.
///
/// # SUPERSEDED — zero callers, not even a test
///
/// **Status as of 2026-08-29.** This is the only writer of `DispatchUnknown`, so
/// while it is uncalled that state is unreachable on disk: no ledger anywhere
/// has ever contained one, and no reader will ever meet one. The other four
/// states are all written in production —
/// [`prepare_coding_invocation`] writes `Prepared`,
/// [`mark_invocation_may_have_started`] writes `RequestMayHaveStarted`,
/// [`accept_invocation_turn`] writes `Accepted`,
/// [`settle_coding_invocation`] writes `Settled` — which makes this the one gap
/// in an otherwise live state machine, and therefore the one worth naming.
///
/// The caller it was designed for is `codex_lifecycle::reconcile_dispatch`,
/// which decides `DispatchUnknown { reason }` from Codex's turn history and
/// would durably record it here. That function is superseded by the loop's
/// effect ledger and has no production callers either; see its documentation for
/// the evidence and for what wiring it would actually require.
///
/// Kept rather than deleted because the decision is the useful artifact: a
/// reader who finds `DispatchUnknown` in the enum and looks for its writer
/// should land here and learn that the ambiguous-dispatch outcome is now the
/// loop's `EffectDisposition::Reattach` → `EffectIndeterminate` path, not a
/// state on this record.
pub fn mark_invocation_unknown(
    execution_dir: &Path,
    invocation_id: &str,
) -> Result<(), LedgerStoreError> {
    mutate_invocation(execution_dir, invocation_id, |entry| {
        entry.dispatch = entry.dispatch.clone().mark_unknown();
        Ok(())
    })
}

fn mutate_invocation(
    execution_dir: &Path,
    invocation_id: &str,
    mutate: impl FnOnce(&mut CodingInvocationState) -> Result<(), SelectionError>,
) -> Result<(), LedgerStoreError> {
    let mut ledger = load_coding_ledger(execution_dir)?;
    let Some(entry) = ledger
        .invocations
        .iter_mut()
        .find(|entry| entry.invocation_id == invocation_id)
    else {
        return Ok(());
    };
    mutate(entry)?;
    store_coding_ledger(execution_dir, &ledger)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::coding_engine::{
        selection::{CodingSelectionSource, SELECTION_CONTRACT_REVISION},
        CodingEngineKind,
    };

    fn selection(profile_id: &str) -> ResolvedCodingEngineSelection {
        ResolvedCodingEngineSelection {
            profile_id: profile_id.to_string(),
            engine: CodingEngineKind::Pi,
            model: None,
            reasoning_effort: None,
            readiness_revision: None,
            readiness_receipt_digest: None,
            adapter_revision: SELECTION_CONTRACT_REVISION.to_string(),
            constraint_digest: "constraint-1".to_string(),
            selection_source: CodingSelectionSource::Fixed,
        }
    }

    /// The property the reattach rule rests on: a re-run of ONE effect finds the
    /// entry it already wrote, and two effects never share one.
    ///
    /// A re-run is the whole case. The loop mints the id from the effect id, so
    /// the second attempt presents the same string — and if that appended a
    /// second entry instead of reusing the first, the ref in the effect ledger
    /// would name an invocation with no continuation forever, and the run would
    /// hold at every pickup.
    #[test]
    fn a_minted_id_reuses_its_entry_on_a_re_run_and_keeps_two_effects_apart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first_effect = coding_invocation_id_for_effect("exec-1", "llm-1:tool:call-1");
        let second_effect = coding_invocation_id_for_effect("exec-1", "llm-1:tool:call-2");
        assert_ne!(
            first_effect, second_effect,
            "two coding jobs in one batch must not mint one invocation id; \"resume the latest\" \
             is precisely the ambiguity this replaces"
        );
        assert_eq!(
            first_effect,
            coding_invocation_id_for_effect("exec-1", "llm-1:tool:call-1"),
            "the mint must be a function of the effect, or a pickup names a job nobody prepared"
        );

        let prepared = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some(&first_effect),
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            4,
        )
        .expect("first attempt");
        assert_eq!(prepared.invocation_id, first_effect);

        // THE RE-RUN. Same effect, same arguments, a fresh worker.
        let reused = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some(&first_effect),
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            4,
        )
        .expect("pickup");
        assert_eq!(reused.invocation_id, first_effect);
        assert_eq!(
            load_coding_ledger(dir.path())
                .expect("load")
                .invocations
                .len(),
            1,
            "a re-run of one effect must find the entry it already wrote, not append a second"
        );

        // A DIFFERENT effect with byte-identical inputs. Under the content
        // derivation these collapsed onto one entry — which is the collapse that
        // makes a reattach ambiguous the moment a run fires two coding jobs.
        let sibling = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some(&second_effect),
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            4,
        )
        .expect("sibling");
        assert_eq!(sibling.invocation_id, second_effect);
        let ledger = load_coding_ledger(dir.path()).expect("load");
        assert_eq!(ledger.invocations.len(), 2);

        // And the lookup answers the RIGHT one. Asserting only "not `None`"
        // would pass against a resolver that returned the latest entry.
        attach_invocation_continuation(
            dir.path(),
            &first_effect,
            CodingContinuationRef::for_pi_session(
                "sess-first",
                dir.path(),
                dir.path(),
                Some("root"),
            ),
            None,
        )
        .expect("attach to the first");
        attach_invocation_continuation(
            dir.path(),
            &second_effect,
            CodingContinuationRef::for_pi_session(
                "sess-second",
                dir.path(),
                dir.path(),
                Some("root"),
            ),
            None,
        )
        .expect("attach to the second");
        assert_eq!(
            invocation_continuation(dir.path(), &first_effect)
                .map(|continuation| continuation.native_session_id),
            Some("sess-first".to_string())
        );
        assert_eq!(
            invocation_continuation(dir.path(), &second_effect)
                .map(|continuation| continuation.native_session_id),
            Some("sess-second".to_string())
        );
        assert!(
            invocation_continuation(dir.path(), "cinv-nothing-prepared-this").is_none(),
            "an id nobody prepared must answer nothing, not the newest entry"
        );
    }

    /// An invocation that never reported stays indeterminate.
    ///
    /// The lookup has a real entry to find — so `None` here is not "no ledger"
    /// or "no such id", it is the state a worker that died mid-job leaves, and
    /// it is the one this rule must never paper over.
    #[test]
    fn a_prepared_invocation_with_no_continuation_resolves_to_no_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let minted = coding_invocation_id_for_effect("exec-9", "llm-9:tool:call-9");
        prepare_coding_invocation(
            dir.path(),
            "exec-9",
            Some(&minted),
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            4,
        )
        .expect("prepare");
        assert_eq!(
            load_coding_ledger(dir.path())
                .expect("load")
                .invocations
                .len(),
            1,
            "the entry must exist, or this asserts the wrong absence"
        );
        assert!(invocation_continuation(dir.path(), &minted).is_none());
    }

    /// A blank supplied id is treated as absent, not accepted.
    ///
    /// `""` would key every unnamed invocation of an execution onto one entry,
    /// which is the collapse `EffectId` refuses for the same reason.
    #[test]
    fn a_blank_supplied_invocation_id_falls_back_to_the_derived_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some("   "),
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            4,
        )
        .expect("prepare");
        assert_eq!(
            entry.invocation_id,
            coding_invocation_id(
                "exec-1",
                &coding_input_digest("fix the footer", "coding-balanced", "apps/site")
            )
        );
    }

    #[test]
    fn prepare_reuses_the_same_tool_action_and_refuses_a_conflicting_authority() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            2,
        )
        .expect("first");
        let reused = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            2,
        )
        .expect("replay");
        assert_eq!(first.invocation_id, reused.invocation_id);
        assert_eq!(
            load_coding_ledger(dir.path())
                .expect("load")
                .invocations
                .len(),
            1
        );

        let err = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-CHANGED",
            selection("coding-balanced"),
            "fix the footer",
            "apps/site",
            2,
        )
        .expect_err("conflict");
        assert!(matches!(
            err,
            LedgerStoreError::Selection(SelectionError::LedgerIntegrityConflict { .. })
        ));
    }

    #[test]
    fn prepare_keeps_two_invocations_and_never_evicts_at_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "first",
            "apps/site",
            2,
        )
        .expect("one");
        prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "second",
            "apps/site",
            2,
        )
        .expect("two");
        let err = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "third",
            "apps/site",
            2,
        )
        .expect_err("cap");
        assert!(matches!(
            err,
            LedgerStoreError::Selection(SelectionError::LedgerCapReached { cap: 2 })
        ));
        assert_eq!(
            load_coding_ledger(dir.path())
                .expect("load")
                .invocations
                .len(),
            2
        );
    }

    #[test]
    fn settle_marks_only_the_named_invocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "first",
            "apps/site",
            4,
        )
        .expect("one");
        let second = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "second",
            "apps/site",
            4,
        )
        .expect("two");
        settle_coding_invocation(
            dir.path(),
            &first.invocation_id,
            CodingTerminalClass::Completed,
        )
        .expect("settle");
        let ledger = load_coding_ledger(dir.path()).expect("load");
        assert!(matches!(
            ledger.invocations[0].dispatch,
            CodingDispatchState::Settled {
                terminal: CodingTerminalClass::Completed
            }
        ));
        assert_eq!(ledger.invocations[1].invocation_id, second.invocation_id);
        assert!(matches!(
            ledger.invocations[1].dispatch,
            CodingDispatchState::Prepared { .. }
        ));
    }

    #[test]
    fn started_before_write_then_accepted_and_continued() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "first",
            "apps/site",
            4,
        )
        .expect("one");
        mark_invocation_may_have_started(dir.path(), &entry.invocation_id).expect("started");
        accept_invocation_turn(dir.path(), &entry.invocation_id, "turn-1").expect("accepted");
        attach_invocation_continuation(
            dir.path(),
            &entry.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_codex_thread(
                "thread-1",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            ),
            None,
        )
        .expect("continue");
        let stored = load_coding_ledger(dir.path()).expect("load");
        assert!(matches!(
            stored.invocations[0].dispatch,
            CodingDispatchState::Accepted { .. }
        ));
        assert_eq!(
            stored.invocations[0]
                .continuation
                .as_ref()
                .map(|item| item.native_session_id.as_str()),
            Some("thread-1")
        );
        assert!(!stored.invocations[0].dispatch.automatic_retry_allowed());
    }

    #[test]
    fn chain_continuation_roundtrips_under_the_root_task_id() {
        let scope = tempfile::tempdir().expect("scope");
        let continuation =
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_grok_session(
                "sess-acp",
                scope.path(),
                std::path::Path::new("/tmp/project"),
                Some("root-task"),
            );
        store_chain_continuation(
            scope.path(),
            &continuation,
            Some(ContinuationFreshReason::ContinuationLost),
        )
        .expect("store");
        let loaded = load_chain_continuation_record(scope.path(), "root-task").expect("load");
        assert_eq!(loaded.continuation.native_session_id, "sess-acp");
        assert_eq!(loaded.continuation.engine, CodingEngineKind::GrokAcp);
        assert_eq!(
            loaded.fresh_reason,
            Some(ContinuationFreshReason::ContinuationLost)
        );
        assert!(load_chain_continuation(scope.path(), "other-root").is_none());
    }

    /// THE REGRESSION THIS CHANGE WAS MOST LIKELY TO CAUSE, as a test.
    ///
    /// A session reported while its turn is still open has to be reachable
    /// **by name**, because that is what a reattach asks, and invisible to
    /// *"the most recent session in this ledger"*, because that is what
    /// `latest_ledger_continuation` asks on behalf of a DIFFERENT invocation
    /// about to open a turn. If one answer served both, a second coding job in
    /// the same execution could bind `resume_thread_id` to a thread another
    /// process is at that moment driving, and two turns would interleave on one
    /// native session.
    ///
    /// The second assertion is written against `entry.continuation` directly
    /// rather than only through `latest_ledger_continuation`, because the field
    /// is the invariant and the helper is one reader of it. (The Codex resume
    /// bind used to be a second, hand-rolled reader of the same field; since
    /// 2026-08-29 it goes through `resolve_previous_chain_continuation` like
    /// Grok, Claude and Agy — so the helper now covers every resume bind, and
    /// asserting the field still covers the helper.)
    #[test]
    fn a_live_session_answers_its_own_reattach_and_no_other_invocation_s_resume() {
        let dir = tempfile::tempdir().expect("tempdir");
        let running = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some("cinv-still-running"),
            "constraint-1",
            selection("coding-balanced"),
            "build the thing",
            "apps/site",
            4,
        )
        .expect("prepare");
        attach_live_invocation_session(
            dir.path(),
            &running.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_codex_thread(
                "thread-live",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            ),
        )
        .expect("live session");

        assert_eq!(
            invocation_continuation(dir.path(), &running.invocation_id)
                .map(|item| item.native_session_id),
            Some("thread-live".to_string()),
            "a reattach names this invocation, and a live session is the right answer to that \
             question — it is the whole reason the mid-turn write exists"
        );
        let ledger = load_coding_ledger(dir.path()).expect("load");
        assert!(
            ledger
                .invocations
                .iter()
                .all(|entry| entry.continuation.is_none()),
            "the resume binds read `continuation`; a turn still in flight must not appear there"
        );
        assert!(
            latest_ledger_continuation(dir.path()).is_none(),
            "the latest session in this ledger is asked on behalf of a different dispatch, and \
             a live one is the wrong answer to it"
        );
    }

    /// The settled write supersedes the live one rather than sitting beside it.
    ///
    /// Both refs name the same session, but only the settled one can carry
    /// `last_completed_turn_id` — so leaving both on disk would keep a strictly
    /// worse duplicate and force the next reader to pick between two refs for
    /// one invocation. After the turn ends the session is a legitimate answer
    /// to the resume scans too, which is the last assertion.
    #[test]
    fn the_settled_continuation_replaces_the_live_one_and_becomes_bindable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some("cinv-settles"),
            "constraint-1",
            selection("coding-balanced"),
            "build the thing",
            "apps/site",
            4,
        )
        .expect("prepare");
        attach_live_invocation_session(
            dir.path(),
            &entry.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_codex_thread(
                "thread-live",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            ),
        )
        .expect("live session");
        let mut settled =
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_codex_thread(
                "thread-live",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            );
        settled.last_completed_turn_id = Some("turn-7".to_string());
        settle_coding_invocation(
            dir.path(),
            &entry.invocation_id,
            CodingTerminalClass::Completed,
        )
        .expect("settle invocation");
        attach_invocation_continuation(dir.path(), &entry.invocation_id, settled, None)
            .expect("settled");

        let stored = load_coding_ledger(dir.path())
            .expect("load")
            .invocations
            .into_iter()
            .find(|item| item.invocation_id == entry.invocation_id)
            .expect("entry");
        assert!(
            stored.live_continuation.is_none(),
            "the live ref is superseded, not kept — two refs for one invocation is a question \
             the next reader should never have to answer"
        );
        assert_eq!(
            stored
                .continuation
                .as_ref()
                .and_then(|item| item.last_completed_turn_id.as_deref()),
            Some("turn-7"),
            "the settled ref carries the turn id the live one could not"
        );
        assert!(matches!(
            invocation_reattach_state(dir.path(), &entry.invocation_id),
            InvocationReattachState::Settled {
                continuation: Some(_)
            }
        ));
        assert!(matches!(
            invocation_reattach_state(dir.path(), "missing-invocation"),
            InvocationReattachState::Absent
        ));
        assert_eq!(
            latest_ledger_continuation(dir.path()).map(|item| item.native_session_id),
            Some("thread-live".to_string()),
            "once the turn has ended the session is a correct answer to the resume scans"
        );
    }

    /// A live write must never demote a settled one. The ordering is not
    /// hypothetical: an engine that reports its session on a late stream frame
    /// could land after the handler's success-path write, and the settled ref
    /// carries a turn id the live one cannot.
    #[test]
    fn a_late_live_write_cannot_overwrite_a_settled_continuation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some("cinv-late"),
            "constraint-1",
            selection("coding-balanced"),
            "build the thing",
            "apps/site",
            4,
        )
        .expect("prepare");
        let mut settled =
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_pi_session(
                "pi-settled",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            );
        settled.last_completed_turn_id = Some("turn-1".to_string());
        attach_invocation_continuation(dir.path(), &entry.invocation_id, settled, None)
            .expect("settled");
        attach_live_invocation_session(
            dir.path(),
            &entry.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_pi_session(
                "pi-late",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            ),
        )
        .expect("late live write");

        assert_eq!(
            invocation_continuation(dir.path(), &entry.invocation_id)
                .map(|item| item.native_session_id),
            Some("pi-settled".to_string()),
            "the settled session wins; a late live write is not new information"
        );
    }

    /// `Prepared` -> `RequestMayHaveStarted` survives a reload, which is the
    /// only form of the answer that matters: the reader is a different process.
    ///
    /// Before this transition had a production caller a live invocation went
    /// `Prepared` -> (nothing) -> `Settled`, so recovery could not tell *never
    /// started* from *started, result unknown* — and `automatic_retry_allowed`,
    /// which licenses re-running a job against a real repository, answered the
    /// same for both.
    #[test]
    fn may_have_started_survives_a_reload_and_withdraws_the_retry_licence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            Some("cinv-dispatching"),
            "constraint-1",
            selection("coding-balanced"),
            "build the thing",
            "apps/site",
            4,
        )
        .expect("prepare");
        assert!(
            entry.dispatch.automatic_retry_allowed(),
            "nothing has been written to the engine yet, so a re-fire is licensed"
        );

        mark_invocation_may_have_started(dir.path(), &entry.invocation_id).expect("may have");

        let reloaded = load_coding_ledger(dir.path())
            .expect("load")
            .invocations
            .into_iter()
            .find(|item| item.invocation_id == entry.invocation_id)
            .expect("entry");
        assert_eq!(reloaded.dispatch.class_name(), "request_may_have_started");
        assert!(
            !reloaded.dispatch.automatic_retry_allowed(),
            "the engine may already hold this request; re-firing it would run the job twice"
        );

        accept_invocation_turn(dir.path(), &entry.invocation_id, "turn-9").expect("accept");
        let accepted = load_coding_ledger(dir.path())
            .expect("load")
            .invocations
            .into_iter()
            .find(|item| item.invocation_id == entry.invocation_id)
            .expect("entry");
        assert_eq!(accepted.dispatch.class_name(), "accepted");
    }

    #[test]
    fn latest_ledger_continuation_is_the_most_recent_any_engine() {
        let dir = tempfile::tempdir().expect("dir");
        let first = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "first",
            "apps/site",
            4,
        )
        .expect("first");
        attach_invocation_continuation(
            dir.path(),
            &first.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_pi_session(
                "pi-sess",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            ),
            None,
        )
        .expect("pi");
        let second = prepare_coding_invocation(
            dir.path(),
            "exec-1",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "second",
            "apps/site",
            4,
        )
        .expect("second");
        attach_invocation_continuation(
            dir.path(),
            &second.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_grok_session(
                "sess-grok",
                std::path::Path::new("/tmp/scope"),
                std::path::Path::new("/tmp/project"),
                Some("root"),
            ),
            None,
        )
        .expect("grok");
        let latest = latest_ledger_continuation(dir.path()).expect("latest");
        assert_eq!(latest.engine, CodingEngineKind::GrokAcp);
        assert_eq!(latest.native_session_id, "sess-grok");
    }

    #[test]
    fn coding_ledger_dirs_for_task_skips_the_empty_follow_up_execution() {
        let scope = tempfile::tempdir().expect("scope");
        let predecessor = scope
            .path()
            .join("tasks")
            .join("root-task")
            .join("executions")
            .join("exec-old");
        let follow_up = scope
            .path()
            .join("tasks")
            .join("child-task")
            .join("executions")
            .join("exec-new");
        std::fs::create_dir_all(&predecessor).expect("pred");
        std::fs::create_dir_all(&follow_up).expect("child");
        let entry = prepare_coding_invocation(
            &predecessor,
            "exec-old",
            None,
            "constraint-1",
            selection("coding-balanced"),
            "first",
            "apps/site",
            4,
        )
        .expect("prepare");
        attach_invocation_continuation(
            &predecessor,
            &entry.invocation_id,
            crate::magician_v2::execution::coding_engine::CodingContinuationRef::for_grok_session(
                "sess-acp",
                scope.path(),
                std::path::Path::new("/tmp/project"),
                Some("root-task"),
            ),
            None,
        )
        .expect("attach");
        let dirs = coding_ledger_dirs_for_task(scope.path(), "root-task");
        assert_eq!(dirs, vec![predecessor]);
        assert!(coding_ledger_dirs_for_task(scope.path(), "child-task").is_empty());
        assert!(coding_ledger_dirs_for_task(scope.path(), "../escape").is_empty());
    }
}
